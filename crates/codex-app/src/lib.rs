mod home;
mod process;
pub mod usage;

use anyhow::{Context, Result, ensure};
pub use home::{prepare_home, rollout_home, validate_home};
pub use process::ProcessIdentity;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, VecDeque},
    fmt,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

const DISCUSSION_PROFILE: &str = "tend-discussion";
const ROUTING_PROFILE: &str = "tend-router-routing";
const RESPONSE_COMPLETED: &str = "rawResponse/completed";

const RPC_TIMEOUT: Duration = Duration::from_secs(15);

fn verify_persistent_thread(response: &Value) -> Result<()> {
    ensure!(
        response["thread"]["ephemeral"] != true,
        "App Server returned an ephemeral thread"
    );
    Ok(())
}
const ROUTING_DISABLED_FEATURES: &[&str] = &[
    "shell_tool",
    "unified_exec",
    "apply_patch_freeform",
    "web_search_request",
    "browser_use",
    "browser_use_external",
    "browser_use_full_cdp_access",
    "in_app_browser",
    "view_image",
    "image_generation",
    "apps",
    "plugins",
    "remote_plugin",
    "multi_agent",
    "multi_agent_v2",
    "multi_agent_v2_dynamic_tools",
    "skill_mcp_dependency_install",
    "code_mode",
    "code_mode_host",
    "goals",
    "memories",
    "hooks",
    "remote_control",
    "tool_suggest",
];

#[derive(Debug, PartialEq, Eq)]
pub enum WorkThreadResume {
    Ready,
    Unavailable,
    Active,
}

pub struct Client {
    child: Child,
    #[cfg(unix)]
    deadline: Option<process::ProcessDeadline>,
    stdin: Option<ChildStdin>,
    events: Receiver<Result<Value>>,
    notifications: VecDeque<Value>,
    request_id: u64,
    work_rollout: Option<(String, PathBuf)>,
    rollout_fresh: bool,
    routing_cwd: Option<PathBuf>,
}

#[derive(Debug, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    pub id: String,
    pub status: TurnStatus,
    pub items: Vec<Value>,
    pub error: Option<TurnError>,
}

#[derive(Debug, Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TurnStatus {
    InProgress,
    Completed,
    Failed,
    Interrupted,
}

#[derive(Debug, Deserialize, serde::Serialize)]
pub struct TurnError {
    pub message: String,
}

pub struct Answer<'a> {
    pub item_id: &'a str,
    pub text: &'a str,
}

pub struct TurnInput<'a> {
    pub thread_id: &'a str,
    pub client_id: &'a str,
    pub text: &'a str,
    pub context: Option<&'a str>,
    pub effort: Option<&'a str>,
    pub discussion_scope: Option<DiscussionScope<'a>>,
}

#[derive(Clone, Copy)]
pub struct DiscussionScope<'a> {
    pub cwd: &'a Path,
    // The Discussion service has already resolved and approved these canonical paths.
    pub roots: &'a BTreeSet<PathBuf>,
}

impl DiscussionScope<'_> {
    fn apply(self, params: &mut Value) -> Result<()> {
        params["cwd"] = serde_json::to_value(self.cwd)?;
        params["permissions"] = json!(DISCUSSION_PROFILE);
        params["approvalPolicy"] = json!("never");
        // An empty set is explicit so it also revokes the previous turn's roots.
        params["runtimeWorkspaceRoots"] = serde_json::to_value(self.roots)?;
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    pub thread_id: String,
    pub turn_id: String,
    pub call_id: String,
    pub tool: String,
    pub namespace: Option<String>,
    pub arguments: Value,
}

pub struct ToolOutput {
    pub success: bool,
    pub body: Value,
}

pub trait TurnHandler {
    fn checks_controls(&self) -> bool {
        false
    }
    fn controls(&mut self, _client: &mut Client, _thread: &str, _turn: &str) -> Result<()> {
        Ok(())
    }
    fn native_item(&mut self, _thread: &str, _turn: &str, _item: &Value) -> Result<()> {
        Ok(())
    }
    /// A completed assistant item for the confirmed root thread and turn.
    fn completed_agent_message(&mut self, _thread: &str, _turn: &str, _item: &Value) {}
    fn usage(&mut self, _thread: &str, _turn: &str, _usage: &Value) -> Result<()> {
        Ok(())
    }
    /// A completion for this confirmed native turn, not a context snapshot.
    fn request_usage(&mut self, _thread: &str, _turn: &str, _response: usage::Response) {}
    fn child_request_usage(
        &mut self,
        _root_thread: &str,
        _root_turn: &str,
        _thread: &str,
        _turn: &str,
        _response: usage::Response,
    ) {
    }
    fn observed_user_message(&mut self, _client_id: &str) -> Result<()> {
        Ok(())
    }
    fn started(&mut self, turn_id: &str) -> Result<()>;
    fn tool(&mut self, call: ToolCall) -> Result<ToolOutput>;
}

#[derive(Debug)]
struct RpcError {
    method: String,
    code: i64,
    message: String,
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "App Server {}: {}", self.method, self.message)
    }
}

impl std::error::Error for RpcError {}

fn missing_thread(error: &anyhow::Error, thread_id: &str) -> bool {
    error.downcast_ref::<RpcError>().is_some_and(|error| {
        error.code == -32600
            && (error.message == format!("no rollout found for thread id {thread_id}")
                || error.message == format!("thread not found: {thread_id}"))
    })
}

impl Turn {
    pub fn answers(&self) -> Result<Vec<Answer<'_>>> {
        let mut final_answers = Vec::new();
        let mut unclassified_answer = None;
        for item in &self.items {
            if item["type"] != "agentMessage" || item["phase"] == "commentary" {
                continue;
            }
            let answer = Answer {
                item_id: item["id"].as_str().context("agent message without an ID")?,
                text: item["text"]
                    .as_str()
                    .context("agent message without text")?,
            };
            if item["phase"] == "final_answer" {
                final_answers.push(answer);
            } else if item["phase"].is_null() {
                unclassified_answer = Some(answer);
            }
        }
        if final_answers.is_empty() {
            final_answers.extend(unclassified_answer);
        }
        Ok(final_answers)
    }
}

impl Client {
    pub fn connect(command: &mut Command) -> Result<Self> {
        Self::connect_inner(command, None)
    }

    fn connect_inner(command: &mut Command, limit: Option<Duration>) -> Result<Self> {
        let home = command
            .get_envs()
            .find(|(key, _)| *key == "CODEX_HOME")
            .and_then(|(_, value)| value)
            .map(PathBuf::from)
            .context("App Server requires an explicit CODEX_HOME")?;
        ensure!(
            validate_home(&home)? == home,
            "App Server requires the canonical CODEX_HOME"
        );
        #[cfg(unix)]
        if limit.is_some() {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        #[cfg(not(unix))]
        ensure!(limit.is_none(), "process group deadlines require Unix");
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .context("start local Codex App Server")?;
        #[cfg(unix)]
        let deadline = limit.map(|limit| process::ProcessDeadline::start(child.id(), limit));
        let stdin = child.stdin.take().context("App Server stdin")?;
        let stdout = child.stdout.take().context("App Server stdout")?;
        let (sender, events) = mpsc::sync_channel(32);
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let message = line
                    .context("read App Server JSON-RPC")
                    .and_then(|line| Ok(serde_json::from_str(&line)?));
                let failed = message.is_err();
                if sender.send(message).is_err() || failed {
                    break;
                }
            }
        });
        let mut client = Self {
            child,
            #[cfg(unix)]
            deadline,
            stdin: Some(stdin),
            events,
            notifications: VecDeque::new(),
            request_id: 0,
            work_rollout: None,
            rollout_fresh: false,
            routing_cwd: None,
        };
        let initialized = client.call(
            "initialize",
            json!({
                "clientInfo": { "name": "tend-ng", "version": env!("CARGO_PKG_VERSION") },
                "capabilities": { "experimentalApi": true,
                    "optOutNotificationMethods": ["rawResponseItem/completed"] }
            }),
        )?;
        if let Some(reported) = initialized["codexHome"].as_str() {
            ensure!(
                Path::new(reported).canonicalize().ok().as_ref() == Some(&home),
                "App Server did not use its configured CODEX_HOME"
            );
        }
        client.write(&json!({"method": "initialized"}))?;
        Ok(client)
    }

    pub fn start_thread(
        &mut self,
        cwd: &Path,
        instructions: &str,
        tools: &[Value],
        model: Option<&str>,
    ) -> Result<String> {
        self.start_thread_inner(cwd, instructions, tools, model)
            .map(|(id, _)| id)
    }

    /// Start a new, persisted, tool-free thread in the restricted service.
    pub fn start_analysis(
        &mut self,
        cwd: &Path,
        instructions: &str,
        model: &str,
    ) -> Result<(String, String)> {
        ensure!(
            self.routing_cwd.is_some(),
            "analysis requires a verified restricted App Server"
        );
        let (id, selected) = self.start_thread_inner(cwd, instructions, &[], Some(model))?;
        Ok((
            id,
            selected.context("App Server did not confirm the analysis model")?,
        ))
    }

    fn start_thread_inner(
        &mut self,
        cwd: &Path,
        instructions: &str,
        tools: &[Value],
        model: Option<&str>,
    ) -> Result<(String, Option<String>)> {
        let mut params = json!({
                "cwd": cwd,
                "experimentalRawEvents": true,
                "sandbox": "danger-full-access", "approvalPolicy": "never",
                "developerInstructions": instructions, "dynamicTools": tools, "model": model
        });
        if let Some(verified) = &self.routing_cwd {
            ensure!(
                cwd == verified,
                "Router cwd differs from its verified configuration"
            );
            routing_thread_scope(&mut params, verified);
        }
        let response = self.start_persistent_thread(params)?;
        if self.routing_cwd.is_some() {
            verify_routing_scope(&response)?;
        }
        let id = response["thread"]["id"]
            .as_str()
            .context("thread/start missing thread ID")?
            .to_owned();
        self.remember_work_rollout(&response["thread"], &id);
        self.rollout_fresh = true;
        let selected = response["model"]
            .as_str()
            .filter(|model| protocol::TokenModel::valid_id(model))
            .map(str::to_owned);
        Ok((id, selected))
    }

    fn start_persistent_thread(&mut self, mut params: Value) -> Result<Value> {
        params["ephemeral"] = json!(false);
        params["historyMode"] = json!("legacy");
        let response = self.call("thread/start", params)?;
        verify_persistent_thread(&response)?;
        Ok(response)
    }

    fn remember_work_rollout(&mut self, thread: &Value, id: &str) {
        self.rollout_fresh = false;
        self.work_rollout = thread["path"]
            .as_str()
            .map(PathBuf::from)
            .map(|path| (id.to_owned(), path));
    }

    pub fn work_rollout(&self, thread: &str) -> Option<&Path> {
        self.work_rollout
            .as_ref()
            .filter(|(id, _)| id == thread)
            .map(|(_, path)| path.as_path())
    }

    pub fn check_discussion_config(&mut self) -> Result<()> {
        let response = self.call("config/read", json!({"includeLayers": false}))?;
        ensure!(
            response["config"].is_object(),
            "App Server did not confirm the service config"
        );
        ensure!(
            response["config"]["mcp_servers"]
                .as_object()
                .is_none_or(|servers| servers.is_empty()),
            "Discussion App Server must not configure MCP servers"
        );
        Ok(())
    }

    pub fn start_discussion_thread(
        &mut self,
        scope: DiscussionScope<'_>,
        instructions: &str,
        tools: &[Value],
        model: Option<&str>,
    ) -> Result<String> {
        let mut params = json!({
            "experimentalRawEvents": true,
            "developerInstructions": instructions, "dynamicTools": tools, "model": model,
        });
        scope.apply(&mut params)?;
        let response = self.start_persistent_thread(params)?;
        verify_discussion_permissions(&response, scope.roots)?;
        let id = response["thread"]["id"]
            .as_str()
            .context("thread/start missing thread ID")?
            .to_owned();
        self.remember_work_rollout(&response["thread"], &id);
        self.rollout_fresh = true;
        Ok(id)
    }

    pub fn resume_discussion_thread(
        &mut self,
        thread_id: &str,
        scope: DiscussionScope<'_>,
        model: Option<&str>,
    ) -> Result<bool> {
        let mut params = json!({ "threadId": thread_id, "excludeTurns": true, "model": model });
        scope.apply(&mut params)?;
        match self.call("thread/resume", params) {
            Ok(response) => {
                verify_persistent_thread(&response)?;
                verify_discussion_permissions(&response, scope.roots)?;
                ensure!(
                    response["thread"]["id"] == thread_id,
                    "resumed an unexpected Discussion thread"
                );
                self.remember_work_rollout(&response["thread"], thread_id);
                Ok(true)
            }
            Err(error) if missing_thread(&error, thread_id) => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub fn discussion_tail_turn(&mut self, thread: &str) -> Result<Option<String>> {
        let response = self.call(
            "thread/turns/list",
            json!({"threadId":thread,"itemsView":"full","limit":1}),
        )?;
        Ok(response["data"]
            .as_array()
            .context("App Server did not confirm the native turn list")?
            .first()
            .map(|turn| turn["id"].as_str().context("A native turn has no identity"))
            .transpose()?
            .map(str::to_owned))
    }

    /// Fork through the last native turn known settled at registration. A
    /// prefix check is independent of any later turn being known to Tend.
    pub fn fork_discussion_through(
        &mut self,
        thread: &str,
        through_turn: &str,
        scope: DiscussionScope<'_>,
        model: Option<&str>,
    ) -> Result<Option<String>> {
        let mut params = json!({
            "threadId": thread, "lastTurnId": through_turn, "excludeTurns": true,
            "ephemeral": false, "model": model,
        });
        scope.apply(&mut params)?;
        let response = match self.call("thread/fork", params) {
            Ok(response) => response,
            // Older App Servers or compacted rollouts may not support a
            // bounded fork. The caller can use saved, bounded history.
            Err(error) if error.downcast_ref::<RpcError>().is_some() => return Ok(None),
            Err(error) => return Err(error),
        };
        verify_persistent_thread(&response)?;
        verify_discussion_permissions(&response, scope.roots)?;
        let fork = response["thread"]["id"]
            .as_str()
            .context("thread/fork missing thread ID")?;
        ensure!(
            fork != thread,
            "bounded Discussion fork reused its parent identity"
        );
        if self.discussion_tail_turn(fork)?.as_deref() != Some(through_turn) {
            return Ok(None);
        }
        self.remember_work_rollout(&response["thread"], fork);
        Ok(Some(fork.to_owned()))
    }

    pub fn process_identity(&self) -> Result<ProcessIdentity> {
        ProcessIdentity::observe(self.child.id())
    }

    pub fn resume_thread(&mut self, thread_id: &str) -> Result<bool> {
        let ordinary = json!({ "threadId": thread_id, "excludeTurns": true });
        let mut params = ordinary.clone();
        if let Some(cwd) = &self.routing_cwd {
            routing_thread_scope(&mut params, cwd);
        }
        let mut response = self.call("thread/resume", params);
        if self.routing_cwd.is_none()
            && response
                .as_ref()
                .err()
                .and_then(|e| e.downcast_ref::<RpcError>())
                .is_some_and(|e| {
                    e.message.contains(&format!(
                        "Could not load saved permission profile `{ROUTING_PROFILE}`"
                    ))
                })
        {
            // Switch back to the ordinary Router's start-thread defaults
            // only when Codex persisted our temporary receiving profile.
            let mut ordinary = ordinary;
            ordinary["sandbox"] = json!("danger-full-access");
            ordinary["approvalPolicy"] = json!("never");
            response = self.call("thread/resume", ordinary);
        }
        match response {
            Ok(response) => {
                verify_persistent_thread(&response)?;
                ensure!(
                    response["thread"]["id"] == thread_id,
                    "resumed an unexpected Router thread"
                );
                if self.routing_cwd.is_some() {
                    verify_routing_scope(&response)?;
                }
                self.remember_work_rollout(&response["thread"], thread_id);
                Ok(true)
            }
            Err(error) if missing_thread(&error, thread_id) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Start a Router process using its normal model and session storage, but
    /// expose no native side-effect tools. Only the checked Tend RPCs remain.
    pub fn connect_routing_only(command: &mut Command, cwd: &Path) -> Result<Self> {
        Self::connect_routing_inner(command, cwd, None)
    }

    pub fn connect_routing_only_with_deadline(
        command: &mut Command,
        cwd: &Path,
        limit: Duration,
    ) -> Result<Self> {
        Self::connect_routing_inner(command, cwd, Some(limit))
    }

    fn connect_routing_inner(
        command: &mut Command,
        cwd: &Path,
        limit: Option<Duration>,
    ) -> Result<Self> {
        command.arg("--strict-config");
        for feature in ROUTING_DISABLED_FEATURES {
            command.arg("-c").arg(format!("features.{feature}=false"));
        }
        for setting in [
            "approval_policy=\"never\"",
            "agents.enabled=false",
            "web_search=\"disabled\"",
            "mcp_servers={}",
            "tools.experimental_request_user_input.enabled=false",
            "shell_environment_policy={inherit=\"none\"}",
        ] {
            command.arg("-c").arg(setting);
        }
        command
            .arg("-c")
            .arg(format!("default_permissions=\"{ROUTING_PROFILE}\""));
        command.arg("-c").arg(format!("permissions.{ROUTING_PROFILE}.filesystem={{\":minimal\"=\"read\",\":workspace_roots\"=\"read\"}}"));
        command.arg("-c").arg(format!(
            "permissions.{ROUTING_PROFILE}.network.enabled=false"
        ));
        let mut client = Self::connect_inner(command, limit)?;
        let response = client.call("config/read", json!({"includeLayers":false, "cwd":cwd}))?;
        let config = &response["config"];
        ensure!(
            config["approval_policy"] == "never"
                && config["agents"]["enabled"] == false
                && config["web_search"] == "disabled"
                && config["mcp_servers"]
                    .as_object()
                    .is_some_and(|servers| servers.is_empty())
                && ROUTING_DISABLED_FEATURES
                    .iter()
                    .all(|feature| config["features"][*feature] == false),
            "App Server did not confirm that Router native tools are disabled"
        );
        client.routing_cwd = Some(cwd.to_owned());
        Ok(client)
    }

    /// Resume a previously stopped work thread before submitting any turn.
    pub fn resume_work_thread(
        &mut self,
        thread_id: &str,
        cwd: &Path,
        model: Option<&str>,
        require_idle: bool,
        confirmed_unavailable_only: bool,
    ) -> Result<WorkThreadResume> {
        let response = match self.call(
            "thread/resume",
            json!({
                "threadId": thread_id, "excludeTurns": true, "cwd": cwd, "model": model,
                "sandbox": "danger-full-access", "approvalPolicy": "never"
            }),
        ) {
            Ok(response) => response,
            Err(error)
                if missing_thread(&error, thread_id)
                    || (!confirmed_unavailable_only
                        && error.downcast_ref::<RpcError>().is_some()) =>
            {
                return Ok(WorkThreadResume::Unavailable);
            }
            Err(error) => return Err(error),
        };
        verify_persistent_thread(&response)?;
        let status = response["thread"]["status"]["type"].as_str();
        if status == Some("active") {
            return Ok(WorkThreadResume::Active);
        }
        if (!(require_idle || confirmed_unavailable_only) || status == Some("idle"))
            && status != Some("systemError")
            && response["thread"]["id"] == thread_id
            && response["cwd"]
                .as_str()
                .is_none_or(|path| Path::new(path) == cwd)
            && model.is_none_or(|expected| {
                response["model"]
                    .as_str()
                    .is_none_or(|model| model == expected)
            })
        {
            self.remember_work_rollout(&response["thread"], thread_id);
            Ok(WorkThreadResume::Ready)
        } else if confirmed_unavailable_only {
            anyhow::bail!("App Server did not confirm the original thread is ready to resume");
        } else {
            Ok(WorkThreadResume::Unavailable)
        }
    }

    pub fn find_turn(
        &mut self,
        thread_id: &str,
        turn_id: Option<&str>,
        client_id: &str,
    ) -> Result<Option<Turn>> {
        let mut cursor = Value::Null;
        loop {
            let response = match self.call(
                "thread/turns/list",
                json!({
                    "threadId": thread_id, "itemsView": "full", "limit": 16, "cursor": cursor
                }),
            ) {
                Ok(response) => response,
                Err(error) if missing_thread(&error, thread_id) => return Ok(None),
                Err(error) => return Err(error),
            };
            let page = response["data"]
                .as_array()
                .context("thread/turns/list missing turn history")?;
            if let Some(turn) = page.iter().find(|turn| match turn_id {
                Some(id) => turn["id"] == id,
                None => turn["items"].as_array().is_some_and(|items| {
                    items
                        .iter()
                        .any(|item| item["type"] == "userMessage" && item["clientId"] == client_id)
                }),
            }) {
                return Ok(Some(serde_json::from_value(turn.clone())?));
            }
            let next = &response["nextCursor"];
            if next.is_null() {
                return Ok(None);
            }
            ensure!(
                next != &cursor,
                "App Server returned the same history cursor"
            );
            cursor = next.clone();
        }
    }

    pub fn steer(&mut self, thread: &str, turn: &str, operation: &str, text: &str) -> Result<bool> {
        match self.call(
            "turn/steer",
            json!({
                "threadId": thread, "expectedTurnId": turn, "clientUserMessageId": operation,
                "input": [{"type": "text", "text": text, "text_elements": []}],
            }),
        ) {
            Ok(response) => {
                ensure!(
                    response["turnId"] == turn,
                    "steer receipt has a different turn"
                );
                Ok(true)
            }
            Err(error) if error.is::<RpcError>() => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub fn interrupt(&mut self, thread: &str, turn: &str) -> Result<()> {
        self.call(
            "turn/interrupt",
            json!({"threadId": thread, "turnId": turn}),
        )?;
        Ok(())
    }

    /// Observation is best-effort and cannot fail or retry a model turn.
    pub fn run_metered_turn(
        &mut self,
        input: TurnInput<'_>,
        handler: &mut impl TurnHandler,
    ) -> Result<Turn> {
        let thread = input.thread_id;
        let collector = self.work_rollout(thread).and_then(|path| {
            usage::Collector::new(path, thread, !self.rollout_fresh).unwrap_or_else(|error| {
                eprintln!("Open native request evidence: {error}");
                None
            })
        });
        self.rollout_fresh = false;
        let mut metered = usage::MeteredTurn::new(handler, collector);
        let result = self.run_turn(input, &mut metered);
        metered.flush(thread);
        result
    }

    pub fn run_turn(
        &mut self,
        input: TurnInput<'_>,
        handler: &mut impl TurnHandler,
    ) -> Result<Turn> {
        let TurnInput {
            thread_id,
            client_id,
            text,
            context,
            effort,
            discussion_scope,
        } = input;
        let mut items = Vec::new();
        if let Some(context) = context {
            items.push(json!({ "type": "text", "text": context, "text_elements": [] }));
        }
        items.push(json!({ "type": "text", "text": text, "text_elements": [] }));
        let mut params = json!({
            "threadId": thread_id, "clientUserMessageId": client_id, "input": items,
            "effort": effort,
        });
        if let Some(scope) = discussion_scope {
            scope.apply(&mut params)?;
        }
        if let Some(cwd) = &self.routing_cwd {
            ensure!(
                discussion_scope.is_none(),
                "Discussion scope cannot override a routing-only turn"
            );
            params["cwd"] = json!(cwd);
            params["permissions"] = json!(ROUTING_PROFILE);
            params["approvalPolicy"] = json!("never");
            params["runtimeWorkspaceRoots"] = json!([]);
        }
        let request_id = self.send_request("turn/start", params)?;
        let startup_deadline = Instant::now() + RPC_TIMEOUT;
        let mut turn_id: Option<String> = None;
        let mut before_start = VecDeque::new();
        let mut messages = Vec::new();
        let mut child_turns = BTreeSet::new();
        let mut next_control = Instant::now();
        let mut history_due = Instant::now() + Duration::from_secs(10);
        let mut history_terminal = None;
        loop {
            if let Some(turn) = &turn_id
                && history_terminal.is_none()
                && handler.checks_controls()
                && Instant::now() >= next_control
            {
                if let Err(error) = handler.controls(self, thread_id, turn) {
                    // A synchronous control RPC may have already received
                    // usage before its reply. Preserve those observations
                    // without waiting for more frames or dispatching tools
                    // after a control has ended the turn.
                    for event in self.notifications.drain(..) {
                        observe_response(&event, thread_id, turn, handler, &mut child_turns);
                        if let Some(usage) = live_usage(&event, thread_id, turn) {
                            let _ = handler.usage(thread_id, turn, usage);
                        }
                    }
                    return Err(error);
                }
                next_control = Instant::now() + Duration::from_millis(250);
            }
            let wait = if turn_id.is_some() {
                if handler.checks_controls() {
                    Duration::from_millis(250)
                } else {
                    Duration::from_secs(10)
                }
            } else {
                startup_deadline.saturating_duration_since(Instant::now())
            };
            ensure!(
                !wait.is_zero(),
                "App Server turn/start could not be confirmed"
            );
            if self.notifications.is_empty()
                && let Some(turn) = history_terminal.take()
            {
                return Ok(turn);
            }
            let event = match self.notifications.pop_front() {
                Some(event) => event,
                None => match self.events.recv_timeout(wait) {
                    Ok(event) => event?,
                    Err(RecvTimeoutError::Timeout) if turn_id.is_some() => {
                        if Instant::now() < history_due {
                            continue;
                        }
                        history_due = Instant::now() + Duration::from_secs(10);
                        if let Ok(Some(turn)) =
                            self.find_turn(thread_id, turn_id.as_deref(), client_id)
                            && turn.status != TurnStatus::InProgress
                        {
                            // The synchronous history RPC can have queued usage
                            // and other events while waiting for its response.
                            history_terminal = Some(turn);
                        }
                        continue;
                    }
                    Err(error) => return Err(error).context("reading the App Server turn"),
                },
            };
            let params = &event["params"];
            let method = event["method"].as_str();
            let started = if method.is_none() && event["id"] == request_id {
                Some(
                    rpc_result(&event, "turn/start")?["turn"]["id"]
                        .as_str()
                        .context("turn/start missing turn ID")?
                        .to_owned(),
                )
            } else if matches!(method, Some("item/started" | "item/completed"))
                && params["threadId"] == thread_id
                && params["item"]["type"] == "userMessage"
                && params["item"]["clientId"] == client_id
            {
                Some(
                    params["turnId"]
                        .as_str()
                        .context("user message missing turn ID")?
                        .to_owned(),
                )
            } else {
                None
            };
            if let Some(started) = started {
                if let Some(previous) = &turn_id {
                    ensure!(
                        previous == &started,
                        "App Server returned conflicting turn identities"
                    );
                } else {
                    handler.started(&started)?;
                    turn_id = Some(started);
                    history_due = Instant::now() + Duration::from_secs(10);
                    before_start.append(&mut self.notifications);
                    self.notifications = std::mem::take(&mut before_start);
                }
                continue;
            }
            if let Some(id) = event.get("id").filter(|_| method.is_some()) {
                if method != Some("item/tool/call") || params["threadId"] != thread_id {
                    self.reject_request(id)?;
                    continue;
                }
                let Some(turn_id) = &turn_id else {
                    before_start.push_back(event);
                    continue;
                };
                let call: ToolCall = serde_json::from_value(params.clone())?;
                if &call.turn_id != turn_id {
                    self.reject_request(id)?;
                    continue;
                }
                let output = handler.tool(call)?;
                self.write(&json!({
                    "id": id,
                    "result": {
                        "success": output.success,
                        "contentItems": [{ "type": "inputText", "text": output.body.to_string() }]
                    }
                }))?;
                continue;
            }
            let Some(turn_id) = &turn_id else {
                if matches!(
                    method,
                    Some(
                        "item/completed"
                            | "turn/completed"
                            | "turn/started"
                            | "thread/tokenUsage/updated"
                            | RESPONSE_COMPLETED
                    )
                ) {
                    before_start.push_back(event);
                }
                continue;
            };
            observe_response(&event, thread_id, turn_id, handler, &mut child_turns);
            if params["threadId"] != thread_id {
                continue;
            }
            if matches!(method, Some("item/started" | "item/completed"))
                && params["turnId"] == *turn_id
                && params["item"]["type"] == "userMessage"
                && let Some(client_id) = params["item"]["clientId"].as_str()
            {
                handler.observed_user_message(client_id)?;
            }
            if matches!(method, Some("item/started" | "item/completed"))
                && params["turnId"] == *turn_id
                && params["item"]["type"] == "collabAgentToolCall"
            {
                handler.native_item(thread_id, turn_id, &params["item"])?;
            }
            if let Some(usage) = live_usage(&event, thread_id, turn_id) {
                handler.usage(thread_id, turn_id, usage)?;
            }
            match method {
                Some("item/completed")
                    if params["turnId"] == *turn_id && params["item"]["type"] == "agentMessage" =>
                {
                    handler.completed_agent_message(thread_id, turn_id, &params["item"]);
                    messages.push(params["item"].clone());
                }
                Some("turn/completed") if params["turn"]["id"] == *turn_id => {
                    let mut turn: Turn = serde_json::from_value(params["turn"].clone())?;
                    for item in &turn.items {
                        if item["type"] == "collabAgentToolCall" {
                            handler.native_item(thread_id, turn_id, item)?;
                        }
                    }
                    for message in messages {
                        if !turn.items.iter().any(|item| item["id"] == message["id"]) {
                            turn.items.push(message);
                        }
                    }
                    return Ok(turn);
                }
                _ => {}
            }
        }
    }

    fn send_request(&mut self, method: &str, params: Value) -> Result<u64> {
        self.request_id += 1;
        let id = self.request_id;
        self.write(&json!({"id": id, "method": method, "params": params}))?;
        Ok(id)
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.send_request(method, params)?;
        let deadline = Instant::now() + RPC_TIMEOUT;
        loop {
            let wait = deadline.saturating_duration_since(Instant::now());
            ensure!(!wait.is_zero(), "App Server {method} response timed out");
            let event = self
                .events
                .recv_timeout(wait)
                .with_context(|| format!("waiting for App Server {method} response"))??;
            match event["method"].as_str() {
                Some(
                    "item/started"
                    | "item/completed"
                    | "turn/completed"
                    | "turn/started"
                    | "item/tool/call"
                    | "thread/tokenUsage/updated"
                    | RESPONSE_COMPLETED,
                ) => {
                    self.notifications.push_back(event);
                }
                Some(_) => {
                    if let Some(request_id) = event.get("id") {
                        self.reject_request(request_id)?;
                    }
                }
                None if event["id"] == id => return rpc_result(&event, method),
                None => {}
            }
        }
    }

    fn reject_request(&mut self, id: &Value) -> Result<()> {
        self.write(&json!({
            "id": id,
            "error": { "code": -32601, "message": "Tend has no handler for this request" }
        }))
    }

    fn write(&mut self, frame: &Value) -> Result<()> {
        let stdin = self.stdin.as_mut().context("App Server stdin is closed")?;
        serde_json::to_writer(&mut *stdin, frame)?;
        writeln!(stdin)?;
        stdin.flush()?;
        Ok(())
    }
}

fn observe_response(
    event: &Value,
    thread: &str,
    turn: &str,
    handler: &mut impl TurnHandler,
    child_turns: &mut BTreeSet<(String, String)>,
) {
    let p = &event["params"];
    let Some(event_thread) = p["threadId"]
        .as_str()
        .filter(|id| usage::valid_native_id(id))
    else {
        return;
    };
    if event_thread != thread
        && matches!(
            event["method"].as_str(),
            Some("turn/started" | "turn/completed")
        )
        && p["turn"]["rootTurnId"] == turn
        && let Some(child_turn) = p["turn"]["id"]
            .as_str()
            .filter(|id| *id != turn && usage::valid_native_id(id))
    {
        child_turns.insert((event_thread.to_owned(), child_turn.to_owned()));
    }
    if event["method"] != RESPONSE_COMPLETED {
        return;
    }
    let Some(event_turn) = p["turnId"].as_str().filter(|id| usage::valid_native_id(id)) else {
        return;
    };
    let Some(response) = usage::Response::from_event(p) else {
        return;
    };
    if event_thread == thread && event_turn == turn {
        handler.request_usage(thread, turn, response);
    } else if event_thread != thread
        && child_turns.contains(&(event_thread.to_owned(), event_turn.to_owned()))
    {
        handler.child_request_usage(thread, turn, event_thread, event_turn, response);
    }
}

fn live_usage<'a>(event: &'a Value, thread: &str, turn: &str) -> Option<&'a Value> {
    let params = &event["params"];
    (event["method"] == "thread/tokenUsage/updated"
        && event.get("id").is_none()
        && params["threadId"] == thread
        && params["turnId"] == turn)
        .then(|| &params["tokenUsage"])
}

fn routing_thread_scope(params: &mut Value, cwd: &Path) {
    params["cwd"] = json!(cwd);
    if let Some(object) = params.as_object_mut() {
        object.remove("sandbox");
    }
    params["permissions"] = json!(ROUTING_PROFILE);
    params["approvalPolicy"] = json!("never");
    params["runtimeWorkspaceRoots"] = json!([]);
    // A resumed ordinary rollout may carry its former feature settings. Use
    // per-thread overrides as well as the process config for this load.
    let disabled: serde_json::Map<_, _> = ROUTING_DISABLED_FEATURES
        .iter()
        .map(|feature| (feature.to_string(), json!(false)))
        .collect();
    params["config"] = json!({"agents":{"enabled":false},"features":disabled,"mcp_servers":{},"web_search":"disabled"});
}

fn verify_routing_scope(response: &Value) -> Result<()> {
    ensure!(
        response["activePermissionProfile"]["id"] == ROUTING_PROFILE
            && response["activePermissionProfile"]["extends"].is_null()
            && response["sandbox"]["type"] == "readOnly"
            && response["sandbox"]["networkAccess"] == false
            && response["approvalPolicy"] == "never"
            && response["runtimeWorkspaceRoots"]
                .as_array()
                .is_some_and(Vec::is_empty),
        "App Server did not confirm the read-only, no-network Router sandbox"
    );
    Ok(())
}

fn verify_discussion_permissions(response: &Value, roots: &BTreeSet<PathBuf>) -> Result<()> {
    ensure!(
        response["activePermissionProfile"]["id"] == DISCUSSION_PROFILE
            && response["activePermissionProfile"]["extends"].is_null()
            && response["approvalPolicy"] == "never"
            && response["sandbox"]["type"] == "readOnly"
            && response["sandbox"]["networkAccess"] == false,
        "App Server did not confirm the read-only Discussion permission profile"
    );
    let actual: BTreeSet<_> = response["runtimeWorkspaceRoots"]
        .as_array()
        .context("App Server did not confirm Discussion runtime workspace roots")?
        .iter()
        .map(|root| -> Result<_> {
            let root = Path::new(
                root.as_str()
                    .context("invalid Discussion runtime workspace root")?,
            );
            ensure!(
                root.is_absolute(),
                "relative Discussion runtime workspace root"
            );
            root.canonicalize()
                .context("could not resolve Discussion runtime workspace root")
        })
        .collect::<Result<_>>()?;
    ensure!(
        &actual == roots,
        "App Server returned unexpected Discussion runtime workspace roots"
    );
    Ok(())
}

fn rpc_result(event: &Value, method: &str) -> Result<Value> {
    if let Some(error) = event.get("error") {
        return Err(RpcError {
            method: method.to_owned(),
            code: error["code"]
                .as_i64()
                .context("missing JSON-RPC error code")?,
            message: error["message"].as_str().unwrap_or_default().to_owned(),
        }
        .into());
    }
    event
        .get("result")
        .cloned()
        .context("missing JSON-RPC result")
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

impl Client {
    pub fn stop(&mut self) -> Result<()> {
        drop(self.stdin.take());
        #[cfg(unix)]
        if let Some(deadline) = self.deadline.take() {
            deadline.stop();
            self.child.wait()?;
            return Ok(());
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            if self.child.try_wait()?.is_some() {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(20));
        }
        self.child.kill()?;
        self.child.wait()?;
        Ok(())
    }
}

#[cfg(test)]
mod discussion_tests {
    use super::*;

    #[test]
    fn discussion_responses_confirm_exact_canonical_workspace_roots() -> Result<()> {
        let cwd = std::env::current_dir()?.canonicalize()?;
        let parent = cwd.parent().context("test cwd has no parent")?.to_owned();
        let expected = BTreeSet::from([cwd.clone(), parent.clone()]);
        let mut response = json!({
            "activePermissionProfile": {"id": DISCUSSION_PROFILE, "extends": null},
            "approvalPolicy": "never",
            "sandbox": {"type": "readOnly", "networkAccess": false},
        });
        assert!(verify_discussion_permissions(&response, &BTreeSet::new()).is_err());
        response["runtimeWorkspaceRoots"] = json!([]);
        verify_discussion_permissions(&response, &BTreeSet::new())?;
        assert!(verify_discussion_permissions(&response, &expected).is_err());
        response["runtimeWorkspaceRoots"] = json!([cwd.join("."), parent]);
        verify_discussion_permissions(&response, &expected)?;
        assert!(verify_discussion_permissions(&response, &BTreeSet::from([cwd])).is_err());
        response["sandbox"]["networkAccess"] = json!(true);
        assert!(verify_discussion_permissions(&response, &expected).is_err());
        Ok(())
    }
}
