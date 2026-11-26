mod process;

use anyhow::{Context, Result, ensure};
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
const RPC_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, PartialEq, Eq)]
pub enum WorkThreadResume {
    Ready,
    Unavailable,
    Active,
}

pub struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    events: Receiver<Result<Value>>,
    notifications: VecDeque<Value>,
    request_id: u64,
    work_rollout: Option<(String, PathBuf)>,
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
    fn usage(&mut self, _thread: &str, _turn: &str, _usage: &Value) -> Result<()> {
        Ok(())
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
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .context("start local Codex App Server")?;
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
            stdin: Some(stdin),
            events,
            notifications: VecDeque::new(),
            request_id: 0,
            work_rollout: None,
        };
        client.call(
            "initialize",
            json!({
                "clientInfo": { "name": "tend-ng", "version": env!("CARGO_PKG_VERSION") },
                "capabilities": { "experimentalApi": true }
            }),
        )?;
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
        let response = self.call(
            "thread/start",
            json!({
                "cwd": cwd, "ephemeral": false, "historyMode": "legacy",
                "sandbox": "danger-full-access", "approvalPolicy": "never",
                "developerInstructions": instructions, "dynamicTools": tools, "model": model
            }),
        )?;
        let id = response["thread"]["id"]
            .as_str()
            .context("thread/start missing thread ID")?
            .to_owned();
        self.remember_work_rollout(&response["thread"], &id);
        Ok(id)
    }

    fn remember_work_rollout(&mut self, thread: &Value, id: &str) {
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
            "ephemeral": false, "historyMode": "legacy",
            "developerInstructions": instructions, "dynamicTools": tools, "model": model,
        });
        scope.apply(&mut params)?;
        let response = self.call("thread/start", params)?;
        verify_discussion_permissions(&response, scope.roots)?;
        Ok(response["thread"]["id"]
            .as_str()
            .context("thread/start missing thread ID")?
            .to_owned())
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
                verify_discussion_permissions(&response, scope.roots)?;
                ensure!(
                    response["thread"]["id"] == thread_id,
                    "resumed an unexpected Discussion thread"
                );
                Ok(true)
            }
            Err(error) if missing_thread(&error, thread_id) => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub fn process_identity(&self) -> Result<ProcessIdentity> {
        ProcessIdentity::observe(self.child.id())
    }

    pub fn resume_thread(&mut self, thread_id: &str) -> Result<bool> {
        match self.call(
            "thread/resume",
            json!({ "threadId": thread_id, "excludeTurns": true }),
        ) {
            Ok(_) => Ok(true),
            Err(error) if missing_thread(&error, thread_id) => Ok(false),
            Err(error) => Err(error),
        }
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
        let request_id = self.send_request("turn/start", params)?;
        let startup_deadline = Instant::now() + RPC_TIMEOUT;
        let mut turn_id: Option<String> = None;
        let mut before_start = VecDeque::new();
        let mut messages = Vec::new();
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
            if params["threadId"] != thread_id {
                continue;
            }
            let Some(turn_id) = &turn_id else {
                if matches!(
                    method,
                    Some("item/completed" | "turn/completed" | "thread/tokenUsage/updated")
                ) {
                    before_start.push_back(event);
                }
                continue;
            };
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
                    | "item/tool/call"
                    | "thread/tokenUsage/updated",
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

fn live_usage<'a>(event: &'a Value, thread: &str, turn: &str) -> Option<&'a Value> {
    let params = &event["params"];
    (event["method"] == "thread/tokenUsage/updated"
        && event.get("id").is_none()
        && params["threadId"] == thread
        && params["turnId"] == turn)
        .then(|| &params["tokenUsage"])
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
