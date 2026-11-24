use crate::store::{Attempt, Input, Store};
use anyhow::{Result, ensure};
use codex_app::{Client, ToolCall, ToolOutput, Turn, TurnHandler, TurnInput, TurnStatus};
use protocol::{DispatchIssue, IssueDispatch};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    ffi::OsString,
    path::PathBuf,
    process::Command,
    thread::{self, JoinHandle},
    time::Duration,
};

const MAX_ATTEMPTS: i64 = 2;
const TOOL_REVISION: i64 = 3;
const ROUTER_INSTRUCTIONS: &str = concat!(
    "You are the user's Tend Router. Answer ordinary chat directly. ",
    "Use tend_dispatch_issue to delegate project work. Its receipt tells you the current assignment status. ",
    "Tend may supply a replay record before the request. It means the following text is the same original input. ",
    "Compare the user's goals against the committed dispatches. Confirm existing work with tend_read_dispatch ",
    "using its operation_id and report its actual status. Create a dispatch only for a distinct goal that still needs one. ",
    "If the remaining work is unclear, explain what Tend has already saved or ask a focused question. ",
    "A Tend Issue report is a background fact, not a fresh user request. Reply to the original topic, ",
    "summarize the actual result, and mention any supplied delivery files. Tend attaches those files to your final answer."
);

fn dispatch_tool() -> Value {
    json!({
        "type": "function",
        "name": "tend_dispatch_issue",
        "description": "Queue a top-level Tend Issue for an Issue Agent. Return the durable Issue, Task, and operation identities.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "title": { "type": "string" },
                "objective": { "type": "string" },
                "repo_ids": { "type": "array", "items": { "type": "string" } },
                "required_capabilities": { "type": "array", "items": { "type": "string" } }
            },
            "required": ["title", "objective"],
            "additionalProperties": false
        }
    })
}

fn read_dispatch_tool() -> Value {
    json!({
        "type": "function",
        "name": "tend_read_dispatch",
        "description": "Read a committed dispatch from the current Chat input by its original operation ID.",
        "inputSchema": {
            "type": "object",
            "properties": { "operation_id": { "type": "string" } },
            "required": ["operation_id"],
            "additionalProperties": false
        }
    })
}

#[derive(Clone)]
pub struct Config {
    pub db: PathBuf,
    pub cwd: PathBuf,
    pub program: OsString,
    pub args: Vec<OsString>,
}

pub fn spawn(config: Config) {
    thread::spawn(move || {
        if let Err(error) = dispatch(config) {
            eprintln!("Router scheduler failed: {error:#}");
            std::process::exit(1);
        }
    });
}

fn dispatch(config: Config) -> Result<()> {
    let store = Store::open(&config.db)?;
    let mut workers: HashMap<String, JoinHandle<()>> = HashMap::new();
    loop {
        workers.retain(|_, worker| !worker.is_finished());
        match store.pending_chats() {
            Ok(chats) => {
                for chat in chats {
                    workers.entry(chat.clone()).or_insert_with(|| {
                        let config = config.clone();
                        thread::spawn(move || {
                            if let Err(error) = consume(&config, &chat) {
                                eprintln!("Router chat {chat} is pending: {error:#}");
                                thread::sleep(Duration::from_secs(2));
                            }
                        })
                    });
                }
            }
            Err(error) => eprintln!("Query pending Router input: {error:#}"),
        }
        thread::sleep(Duration::from_millis(200));
    }
}

fn consume(config: &Config, chat: &str) -> Result<()> {
    let mut store = Store::open(&config.db)?;
    let mut command = Command::new(&config.program);
    command
        .args(&config.args)
        .current_dir(&config.cwd)
        .env_remove(protocol::TOKEN_ENV);
    let mut client = Client::connect(&mut command)?;
    let process = client.process_identity()?;
    let mut loaded_thread = None;
    let tools = [dispatch_tool(), read_dispatch_tool()];
    while let Some(input) = store.next_input(chat)? {
        let number = if let Some(attempt) = &input.attempt {
            if let Some(turn) = recover(&mut client, &store, attempt)? {
                finish(&mut store, input.seq, turn)?;
                continue;
            }
            if attempt.number >= MAX_ATTEMPTS {
                store.clear_thread(chat, &attempt.thread_id)?;
                store.finish(
                    input.seq,
                    &[],
                    Some("Could not recover the Router response after two attempts. You can send the request again."),
                )?;
                continue;
            }
            attempt.number + 1
        } else {
            1
        };

        let previous_thread = if number == 1 {
            store.thread_id(chat, TOOL_REVISION)?
        } else {
            None
        };
        let (thread_id, context) = match previous_thread {
            Some(id) if loaded_thread.as_ref() == Some(&id) || client.resume_thread(&id)? => {
                (id, None)
            }
            _ => {
                let id = client.start_thread(&config.cwd, ROUTER_INSTRUCTIONS, &tools, None)?;
                store.save_thread(chat, &id, TOOL_REVISION)?;
                let context: Vec<_> = [
                    store.recent_context(
                        chat,
                        if input.report_id.is_some() {
                            i64::MAX
                        } else {
                            input.message_seq
                        },
                    )?,
                    (number > 1)
                        .then(|| replay_context(&store, &input, number))
                        .transpose()?,
                ]
                .into_iter()
                .flatten()
                .collect();
                (id, (!context.is_empty()).then(|| context.join("\n\n")))
            }
        };
        loaded_thread = Some(thread_id.clone());
        let attempt_id = store.start_attempt(input.seq, number, &thread_id, &process)?;
        let turn = client.run_turn(
            TurnInput {
                thread_id: &thread_id,
                client_id: &attempt_id,
                text: &input.text,
                context: context.as_deref(),
                effort: None,
            },
            &mut DispatchHandler {
                store: &mut store,
                attempt_id: &attempt_id,
            },
        )?;
        finish(&mut store, input.seq, turn)?;
    }
    Ok(())
}

fn replay_context(store: &Store, input: &Input, attempt_number: i64) -> Result<String> {
    let record = json!({
        "kind": "replay",
        "source_message_id": input.message_id,
        "report_id": input.report_id,
        "attempt_number": attempt_number,
        "committed_dispatches": store.dispatches_for_input(input.seq)?,
    });
    let kind = if input.report_id.is_some() {
        "Issue report"
    } else {
        "request"
    };
    Ok(format!(
        "Tend replay record. The next text is the original {kind}.\n{record}"
    ))
}

struct DispatchHandler<'a> {
    store: &'a mut Store,
    attempt_id: &'a str,
}

impl TurnHandler for DispatchHandler<'_> {
    fn started(&mut self, turn_id: &str) -> Result<()> {
        self.store.save_turn_id(self.attempt_id, turn_id)
    }

    fn tool(&mut self, call: ToolCall) -> Result<ToolOutput> {
        if call.namespace.is_some() {
            return Ok(tool_error("Unknown Router tool"));
        }
        match call.tool.as_str() {
            "tend_read_dispatch" => {
                let Some(operation_id) = call.arguments["operation_id"]
                    .as_str()
                    .filter(|id| protocol::valid_id(id))
                else {
                    return Ok(tool_error("A valid operation ID is required"));
                };
                tool_receipt(
                    self.store
                        .read_dispatch(self.attempt_id, &call, operation_id)?,
                    "No dispatch with that operation ID belongs to this Chat input",
                )
            }
            "tend_dispatch_issue" => self.dispatch(&call),
            _ => Ok(tool_error("Unknown Router tool")),
        }
    }
}

impl DispatchHandler<'_> {
    fn dispatch(&mut self, call: &ToolCall) -> Result<ToolOutput> {
        let work: DispatchIssue = match serde_json::from_value(call.arguments.clone()) {
            Ok(work) => work,
            Err(error) => return Ok(tool_error(&format!("Invalid dispatch request ({error})"))),
        };
        if work.title.trim().is_empty()
            || work.objective.trim().is_empty()
            || work
                .repo_ids
                .iter()
                .chain(&work.required_capabilities)
                .any(|id| id.trim().is_empty())
        {
            return Ok(tool_error(
                "Dispatch needs a title, objective, and valid requirements",
            ));
        }
        tool_receipt(
            self.store.dispatch_issue(self.attempt_id, call, &work)?,
            "Dispatch call does not match its saved origin or intent",
        )
    }
}

fn tool_receipt(receipt: Option<IssueDispatch>, missing: &str) -> Result<ToolOutput> {
    Ok(match receipt {
        Some(receipt) => ToolOutput {
            success: true,
            body: serde_json::to_value(receipt)?,
        },
        None => tool_error(missing),
    })
}

fn tool_error(message: &str) -> ToolOutput {
    ToolOutput {
        success: false,
        body: json!({ "error": message }),
    }
}

fn recover(client: &mut Client, store: &Store, attempt: &Attempt) -> Result<Option<Turn>> {
    if let Some(process) = &attempt.process {
        ensure!(
            process.is_stopped()?,
            "App Server for attempt {} is still running",
            attempt.id,
        );
    }
    let turn = match client.find_turn(&attempt.thread_id, attempt.turn_id.as_deref(), &attempt.id) {
        Ok(turn) => turn,
        Err(error) if attempt.process.is_some() => {
            eprintln!(
                "Could not read history for stopped attempt {}: {error:#}",
                attempt.id
            );
            None
        }
        Err(error) => return Err(error),
    };
    if let Some(turn) = turn
        && turn.status != TurnStatus::InProgress
    {
        if attempt.turn_id.is_none() {
            store.save_turn_id(&attempt.id, &turn.id)?;
        }
        return Ok(Some(turn));
    }
    ensure!(
        attempt.process.is_some(),
        "missing stop evidence for the original attempt {}",
        attempt.id,
    );
    Ok(None)
}

fn finish(store: &mut Store, seq: i64, turn: Turn) -> Result<()> {
    match turn.status {
        TurnStatus::Completed => store.finish(seq, &turn.answers()?, None),
        TurnStatus::Failed | TurnStatus::Interrupted => {
            let error = turn
                .error
                .as_ref()
                .map_or("Codex turn ended early", |error| &error.message);
            store.finish(seq, &[], Some(error))
        }
        TurnStatus::InProgress => unreachable!("only a terminal turn can finish an input"),
    }
}
