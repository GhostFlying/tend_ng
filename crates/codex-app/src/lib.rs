use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

const RPC_TIMEOUT: Duration = Duration::from_secs(15);

pub struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    events: Receiver<Result<Value>>,
    notifications: VecDeque<Value>,
    request_id: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    pub id: String,
    pub status: TurnStatus,
    pub items: Vec<Value>,
    pub error: Option<TurnError>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TurnStatus {
    InProgress,
    Completed,
    Failed,
    Interrupted,
}

#[derive(Debug, Deserialize)]
pub struct TurnError {
    pub message: String,
}

pub struct Answer<'a> {
    pub item_id: &'a str,
    pub text: &'a str,
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

    pub fn start_thread(&mut self, cwd: &Path) -> Result<String> {
        let response = self.call(
            "thread/start",
            json!({
                "cwd": cwd, "ephemeral": false, "historyMode": "legacy",
                "sandbox": "danger-full-access", "approvalPolicy": "never",
                "developerInstructions": "You are the user's Tend Router. Answer ordinary chat directly. Delegate work to the responsible agents when Tend tools are available."
            }),
        )?;
        Ok(response["thread"]["id"]
            .as_str()
            .context("thread/start missing thread ID")?
            .to_owned())
    }

    pub fn resume_thread(&mut self, thread_id: &str) -> Result<()> {
        self.call(
            "thread/resume",
            json!({ "threadId": thread_id, "excludeTurns": true }),
        )?;
        Ok(())
    }

    pub fn start_turn(&mut self, thread_id: &str, client_id: &str, text: &str) -> Result<String> {
        let response = self.call(
            "turn/start",
            json!({
                "threadId": thread_id, "clientUserMessageId": client_id,
                "input": [{ "type": "text", "text": text, "text_elements": [] }]
            }),
        )?;
        Ok(response["turn"]["id"]
            .as_str()
            .context("turn/start missing turn ID")?
            .to_owned())
    }

    pub fn find_turn(
        &mut self,
        thread_id: &str,
        turn_id: Option<&str>,
        client_id: &str,
    ) -> Result<Option<Turn>> {
        let mut cursor = Value::Null;
        loop {
            let response = self.call(
                "thread/turns/list",
                json!({
                    "threadId": thread_id, "itemsView": "full", "limit": 16, "cursor": cursor
                }),
            )?;
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

    pub fn await_turn(&mut self, thread_id: &str, turn_id: &str, client_id: &str) -> Result<Turn> {
        let mut messages = Vec::new();
        loop {
            let event = match self.notifications.pop_front() {
                Some(event) => event,
                None => match self.events.recv_timeout(Duration::from_secs(10)) {
                    Ok(event) => event?,
                    Err(RecvTimeoutError::Timeout) => {
                        // Check history if the live terminal notification was missed.
                        if let Ok(Some(turn)) = self.find_turn(thread_id, Some(turn_id), client_id)
                            && turn.status != TurnStatus::InProgress
                        {
                            return Ok(turn);
                        }
                        continue;
                    }
                    Err(error) => return Err(error).context("reading the App Server turn"),
                },
            };
            if let Some(request_id) = event.get("id")
                && event.get("method").is_some()
            {
                self.reject_request(request_id)?;
                continue;
            }
            let params = &event["params"];
            if params["threadId"] != thread_id {
                continue;
            }
            match event["method"].as_str() {
                Some("item/completed")
                    if params["turnId"] == turn_id && params["item"]["type"] == "agentMessage" =>
                {
                    messages.push(params["item"].clone());
                }
                Some("turn/completed") if params["turn"]["id"] == turn_id => {
                    let mut turn: Turn = serde_json::from_value(params["turn"].clone())?;
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

    fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.request_id += 1;
        let id = self.request_id;
        self.write(&json!({"id": id, "method": method, "params": params}))?;
        let deadline = Instant::now() + RPC_TIMEOUT;
        loop {
            let wait = deadline.saturating_duration_since(Instant::now());
            ensure!(!wait.is_zero(), "App Server {method} response timed out");
            let event = self
                .events
                .recv_timeout(wait)
                .with_context(|| format!("waiting for App Server {method} response"))??;
            if event.get("method").is_some() {
                if let Some(request_id) = event.get("id") {
                    self.reject_request(request_id)?;
                } else if matches!(
                    event["method"].as_str(),
                    Some("item/completed" | "turn/completed")
                ) {
                    self.notifications.push_back(event);
                }
                continue;
            }
            if event["id"] == id {
                if let Some(error) = event.get("error") {
                    bail!("App Server {method}: {}", error["message"]);
                }
                return event
                    .get("result")
                    .cloned()
                    .context("missing JSON-RPC result");
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

impl Drop for Client {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
