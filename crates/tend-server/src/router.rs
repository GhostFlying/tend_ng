use crate::store::{Attempt, Input, Store};
use anyhow::{Result, ensure};
use codex_app::{Client, ToolCall, ToolOutput, Turn, TurnHandler, TurnInput, TurnStatus};
use protocol::{AgentQuery, DispatchIssue, ExternalCondition};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    ffi::OsString,
    path::PathBuf,
    process::Command,
    thread::{self, JoinHandle},
    time::Duration,
};

pub(crate) const MAX_ATTEMPTS: i64 = 2;
const TOOL_REVISION: i64 = 16;
const ROUTER_INSTRUCTIONS: &str = concat!(
    "You are the user's Tend Router. Answer ordinary chat directly. ",
    "Use tend_dispatch_issue to delegate project work. Its receipt tells you the current assignment status. ",
    "Preserve the user's original goal, authorization, and stopping point in the dispatch. Add only the context needed to carry out that work. ",
    "For new work the user asks to start now, when the user explicitly names a repository in the Tend registered repository IDs list, ",
    "call tend_dispatch_issue exactly once for that request with the matching canonical repo_ids from the list. ",
    "You may match the name case-insensitively, but pass the ID exactly as listed. ",
    "For this direct dispatch, do not call tend_agent_candidates, look up or verify the MR or PR, or pin an Agent or Runner. ",
    "Leave base_refs unset unless the user explicitly provides them. ",
    "The Work Agent uses its own repository access and permissions to locate the MR or PR. ",
    "For a review-only request, make the objective read-only and have the Work Agent stop after reporting findings without modifying or merging the MR or PR. ",
    "If the user does not explicitly name a registered repository, use the ordinary routing rules. ",
    "Keep read-only investigations and recommendations pending confirmation read-only. Stop before implementation unless the user authorizes it. ",
    "When the user explicitly asks this Issue to deliver a plan, report, or attachment and then wait in place for confirmation or review, treat it as a stage delivery. ",
    "In the persistent dispatch objective, describe the stage delivery and waiting for the user's decision as part of the work. Do not make receipt of the delivery the completion condition. ",
    "When the user wants to record work for later, use tend_record_issue to save it without scheduling execution. ",
    "When the user asks to start that scope, use tend_start_issue with its Issue ID or displayed short number. ",
    "Finish routing the current input, acknowledge the durable handoff, and end the turn. ",
    "Do not poll or wait for delegated work, a report, or an Attention to arrive in this turn. ",
    "Tend delivers these as separate inputs, including when the user asks for an eventual result. ",
    "For other dispatches, when a named configuration matters, use tend_agent_candidates before specifying the Agent and Runner. ",
    "Tend may supply a replay record before the request. It means the following text is the same original input. ",
    "Compare the user's goals against the committed dispatches. Confirm existing work with tend_read_dispatch ",
    "using its operation_id and report its actual status. Do not repeat a committed direct dispatch. ",
    "Create a dispatch only for a distinct goal that still needs one. ",
    "Use tend_relay_issue_message to deliver the current user message to a related existing Issue when its Owner needs to act. ",
    "Use tend_list_issues to locate recent related work in this Chat and tend_read_issue to check current work facts before answering a follow-up. ",
    "If the remaining work is unclear, explain what Tend has already saved or ask a focused question. ",
    "A Tend Issue report is a background fact, not a fresh user request or authorization to implement. Reply to the original topic, ",
    "summarize the actual result, and mention any supplied delivery files. Tend attaches those files to your final answer. ",
    "For a stage delivery, publish the report with its files and the related Attention in the original Chat. Keep the top-level Issue open pending the user's decision. The report receipt does not complete the Issue. ",
    "A Tend Attention is a decision the user may need to make. Use tend_publish_attention with its original ID ",
    "to ask the question in the original Chat when warranted by priority and timing. Do not guess a decision. ",
    "Use tend_read_attentions to inspect open decisions and tend_answer_attention only when the current user message ",
    "actually answers an identified ordinary question. When the answer is unclear, ask the user to clarify. ",
    "For a structured proposal, publish its current concrete question using tend_publish_attention. ",
    "Direct the user to the exact version approval action in this Chat or the Issue. Do not turn prose into ",
    "proposal approval or relay an apparent approval as instructions to begin. ",
    "Use tend_subscribe to receive GitHub facts relevant to an existing topic. ",
    "A Tend external facts batch is background input with each fact's original topic. ",
    "When no user update is needed, answer exactly TEND_NO_UPDATE. Make timely decisions about useful user updates."
);

fn dispatch_tool(name: &str, description: &str) -> Value {
    json!({
        "type": "function",
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": {
                "title": { "type": "string" },
                "objective": { "type": "string" },
                "repo_ids": { "type": "array", "items": { "type": "string" } },
                "agent": {"type":"object","properties":{"id":{"type":"string"},"runner_id":{"type":"string"}},
                          "required":["id","runner_id"],"additionalProperties":false},
                "base_refs": { "type":"object", "additionalProperties": {"type":"string"} },
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

fn relay_tool() -> Value {
    json!({
        "type": "function",
        "name": "tend_relay_issue_message",
        "description": "Queue the current user message, with its original text and source identity, for the Owner of an existing Issue from this Chat. Supply the Issue UUID or displayed short number (#N or N).",
        "inputSchema": {
            "type": "object",
            "properties": {"issue_id": {"type": "string"}},
            "required": ["issue_id"],
            "additionalProperties": false
        }
    })
}

fn subscribe_tool() -> Value {
    json!({
        "type":"function", "name":"tend_subscribe",
        "description":"Subscribe this Router to an explicit GitHub pull request or CI condition for the current topic.",
        "inputSchema":{"type":"object","properties":{
            "url":{"type":"string"}, "event":{"type":"string","enum":["pull","ci"]},
            "state":{"type":"string"}, "head_sha":{"type":"string"}, "check":{"type":"string"},
            "run_id":{"type":"string"}
        },"required":["url","event","state"],"additionalProperties":false}
    })
}

fn attention_tools() -> [Value; 3] {
    [
        json!({
            "type": "function", "name": "tend_publish_attention",
            "description": "Publish the decision question in its original Chat and return the existing question receipt on replay.",
            "inputSchema": {"type": "object", "properties": {
                "attention_id": {"type": "string"}, "text": {"type": "string"}
            }, "required": ["attention_id", "text"], "additionalProperties": false}
        }),
        json!({
            "type": "function", "name": "tend_answer_attention",
            "description": "Record the decision from the current user message and send it to the original responsible Task.",
            "inputSchema": {"type": "object", "properties": {
                "attention_id": {"type": "string"}, "answer": {"type": "string"}
            }, "required": ["attention_id", "answer"], "additionalProperties": false}
        }),
        json!({
            "type": "function", "name": "tend_read_attentions",
            "description": "Read open decisions for this Chat, including options, priorities, and original question message IDs.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
        }),
    ]
}

#[derive(Clone)]
pub struct Config {
    pub db: PathBuf,
    pub cwd: PathBuf,
    pub program: OsString,
    pub args: Vec<OsString>,
    pub effort: Option<String>,
    pub model: Option<String>,
    pub name: Option<String>,
    pub discussion: std::sync::Arc<crate::discussion::Service>,
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
                            let result = Store::open(&config.db).and_then(|store| {
                                if store.is_discussion(&chat)? {
                                    config.discussion.consume(store, &chat)
                                } else {
                                    consume(&config, &chat, store)
                                }
                            });
                            if let Err(error) = result {
                                eprintln!("Chat {chat} is pending: {error:#}");
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

fn consume(config: &Config, chat: &str, mut store: Store) -> Result<()> {
    let mut command = Command::new(&config.program);
    command
        .args(&config.args)
        .current_dir(&config.cwd)
        .env_remove(protocol::TOKEN_ENV)
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN");
    let mut client = Client::connect(&mut command)?;
    let process = client.process_identity()?;
    let mut loaded_thread = None;
    let mut tools = vec![
        dispatch_tool(
            "tend_dispatch_issue",
            "Queue a top-level Issue now and return its durable Issue, Task, and operation identities.",
        ),
        dispatch_tool(
            "tend_record_issue",
            "Record an Issue for later without creating an execution Task.",
        ),
        json!({
            "type": "function", "name": "tend_start_issue",
            "description": "Start a recorded Issue from this Chat using its saved scope. Supply the Issue UUID or displayed short number (#N or N).",
            "inputSchema": {"type":"object","properties":{"issue_id":{"type":"string"}},
                           "required":["issue_id"],"additionalProperties":false}
        }),
        read_dispatch_tool(),
        relay_tool(),
        subscribe_tool(),
        json!({
            "type":"function", "name":"tend_list_issues",
            "description":"List the latest Issue and PR statuses, including each Issue short number, for this Chat.",
            "inputSchema":{"type":"object","properties":{},"additionalProperties":false}
        }),
        json!({
            "type":"function", "name":"tend_read_issue",
            "description":"Read an Issue from this Chat, including its latest Task, actual execution, and published delivery files. Supply the Issue UUID or displayed short number (#N or N).",
            "inputSchema":{"type":"object","properties":{"issue_id":{"type":"string"}},
                           "required":["issue_id"],"additionalProperties":false}
        }),
        json!({
            "type":"function", "name":"tend_agent_candidates",
            "description":"Read qualifying Issue Agent configurations by repository and capability.",
            "inputSchema":{"type":"object","properties":{
                "category":{"type":"string","enum":["issue-router"]},
                "repo_ids":{"type":"array","items":{"type":"string"}},
                "required_capabilities":{"type":"array","items":{"type":"string"}}
            },"required":["category"],"additionalProperties":false}
        }),
    ];
    tools.extend(attention_tools());
    while let Some(input) = store.next_input(chat)? {
        let number = if let Some(attempt) = &input.attempt {
            if let Some(turn) = recover(&mut client, &store, attempt)? {
                finish(&mut store, &input, turn)?;
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
            store.thread_id(chat, TOOL_REVISION, config.model.as_deref())?
        } else {
            None
        };
        let mut context = Vec::new();
        let repo_ids = store.registered_repo_ids()?;
        if !repo_ids.is_empty() {
            context.push(format!(
                "Tend registered repository IDs: {}",
                serde_json::to_string(&repo_ids)?
            ));
        }
        let thread_id = match previous_thread {
            Some(id) if loaded_thread.as_ref() == Some(&id) || client.resume_thread(&id)? => id,
            _ => {
                let id = client.start_thread(
                    &config.cwd,
                    ROUTER_INSTRUCTIONS,
                    &tools,
                    config.model.as_deref(),
                )?;
                store.save_thread(chat, &id, TOOL_REVISION, config.model.as_deref())?;
                context.extend(store.recent_context(
                    chat,
                    if input.report_id.is_some()
                        || input.attention_id.is_some()
                        || input.external_batch.is_some()
                    {
                        i64::MAX
                    } else {
                        input.message_seq
                    },
                )?);
                id
            }
        };
        context.extend(input.reply_context.iter().cloned());
        if number > 1 {
            context.push(replay_context(&store, &input, number)?);
        }
        let context = (!context.is_empty()).then(|| context.join("\n\n"));
        loaded_thread = Some(thread_id.clone());
        let attempt_id = store.start_attempt(input.seq, number, &thread_id, &process)?;
        let turn = client.run_turn(
            TurnInput {
                thread_id: &thread_id,
                client_id: &attempt_id,
                text: &input.text,
                context: context.as_deref(),
                effort: config.effort.as_deref(),
                discussion_scope: None,
            },
            &mut DispatchHandler {
                store: &mut store,
                attempt_id: &attempt_id,
                dispatcher_name: config.name.as_deref(),
            },
        )?;
        finish(&mut store, &input, turn)?;
    }
    Ok(())
}

fn replay_context(store: &Store, input: &Input, attempt_number: i64) -> Result<String> {
    let record = json!({
        "kind": "replay",
        "source_message_id": input.message_id,
        "report_id": input.report_id,
        "attention_id": input.attention_id,
        "external_batch": input.external_batch,
        "attempt_number": attempt_number,
        "committed_dispatches": store.dispatches_for_input(input.seq)?,
    });
    let kind = if input.attention_id.is_some() {
        "Attention"
    } else if input.report_id.is_some() {
        "Issue report"
    } else if input.external_batch.is_some() {
        "external facts batch"
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
    dispatcher_name: Option<&'a str>,
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
            "tend_dispatch_issue" | "tend_record_issue" => self.dispatch(&call),
            "tend_list_issues" | "tend_read_issue" | "tend_start_issue" => {
                let Some(chat) = self.store.router_chat(self.attempt_id, &call)? else {
                    return Ok(tool_error("Cannot verify the current Chat"));
                };
                if call.tool == "tend_list_issues" {
                    let page = self.store.issue_board(Some(&chat), None, false)?;
                    return Ok(ToolOutput {
                        success: true,
                        body: serde_json::to_value(page)?,
                    });
                }
                let reference = call.arguments["issue_id"].as_str().unwrap_or_default();
                let Some(id) = self.store.resolve_issue(reference)? else {
                    return Ok(tool_error(if call.tool == "tend_start_issue" {
                        "No startable recorded Issue with that ID belongs to this Chat"
                    } else {
                        "That Issue is not in this Chat"
                    }));
                };
                if call.tool == "tend_start_issue" {
                    return tool_receipt(
                        self.store.start_recorded_issue(&id, Some(&chat))?,
                        "No startable recorded Issue with that ID belongs to this Chat",
                    );
                }
                match self.store.issue(&id)? {
                    Some(issue) if issue.chat_id() == chat => Ok(ToolOutput {
                        success: true,
                        body: serde_json::to_value(issue)?,
                    }),
                    _ => Ok(tool_error("That Issue is not in this Chat")),
                }
            }
            "tend_agent_candidates" => {
                let Ok(query) = serde_json::from_value::<AgentQuery>(call.arguments.clone()) else {
                    return Ok(tool_error(
                        "Provide category and any repository or capability requirements",
                    ));
                };
                if query.category != "issue-router" || !query.valid() {
                    return Ok(tool_error("Provide a valid issue-router query"));
                }
                Ok(ToolOutput {
                    success: true,
                    body: json!({"candidates": self.store.agent_candidates(&query)?}),
                })
            }
            "tend_subscribe" => {
                let Ok(mut condition) =
                    serde_json::from_value::<ExternalCondition>(call.arguments.clone())
                else {
                    return Ok(tool_error("Use a specific GitHub PR or CI condition"));
                };
                if !condition.normalize() {
                    return Ok(tool_error("Use a specific GitHub PR or CI condition"));
                }
                Ok(
                    match self
                        .store
                        .router_subscribe(self.attempt_id, &call, &condition)?
                    {
                        Some(id) => ToolOutput {
                            success: true,
                            body: json!({"watch_id": id}),
                        },
                        None => tool_error("Cannot verify the subscription's original topic"),
                    },
                )
            }
            "tend_relay_issue_message" => {
                let reference = call.arguments["issue_id"].as_str().unwrap_or_default();
                let Some(issue) = self.store.resolve_issue(reference)? else {
                    return Ok(tool_error(
                        "This user message cannot be relayed to that Issue",
                    ));
                };
                Ok(
                    match self.store.relay_to_issue(self.attempt_id, &call, &issue)? {
                        Some(receipt) => ToolOutput {
                            success: true,
                            body: serde_json::to_value(receipt)?,
                        },
                        None => tool_error("This user message cannot be relayed to that Issue"),
                    },
                )
            }
            "tend_read_attentions" => Ok(ToolOutput {
                success: true,
                body: serde_json::to_value(self.store.router_attentions(self.attempt_id, &call)?)?,
            }),
            "tend_publish_attention" => self.attention(&call, "text"),
            "tend_answer_attention" => self.attention(&call, "answer"),
            _ => Ok(tool_error("Unknown Router tool")),
        }
    }
}

impl DispatchHandler<'_> {
    fn attention(&mut self, call: &ToolCall, field: &str) -> Result<ToolOutput> {
        let Some(id) = call.arguments["attention_id"]
            .as_str()
            .filter(|id| protocol::valid_id(id))
        else {
            return Ok(tool_error("A valid Attention ID is required"));
        };
        let Some(text) = call.arguments[field]
            .as_str()
            .filter(|text| !text.trim().is_empty())
        else {
            return Ok(tool_error("Specify the question or the user's decision"));
        };
        let receipt = if field == "text" {
            self.store
                .publish_attention(self.attempt_id, call, id, text)?
        } else {
            self.store
                .answer_attention(self.attempt_id, call, id, text)?
        };
        Ok(match receipt {
            Some(attention) => ToolOutput {
                success: true,
                body: serde_json::to_value(attention)?,
            },
            None => tool_error("There is no matching decision for this Chat input"),
        })
    }
    fn dispatch(&mut self, call: &ToolCall) -> Result<ToolOutput> {
        let work: DispatchIssue = match serde_json::from_value(call.arguments.clone()) {
            Ok(work) => work,
            Err(error) => return Ok(tool_error(&format!("Invalid dispatch request ({error})"))),
        };
        if work.title.trim().is_empty()
            || work.objective.trim().is_empty()
            || work
                .agent
                .as_ref()
                .is_some_and(|agent| !agent.valid() || agent.runner_id.is_none())
            || work.base_refs.iter().any(|(repo, reference)| {
                !work.repo_ids.contains(repo) || reference.trim().is_empty()
            })
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
            self.store.dispatch_issue(
                self.attempt_id,
                call,
                &work,
                call.tool == "tend_record_issue",
                self.dispatcher_name,
            )?,
            "Dispatch call does not match its saved origin or intent",
        )
    }
}

fn tool_receipt(receipt: Option<impl serde::Serialize>, missing: &str) -> Result<ToolOutput> {
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

pub(crate) fn recover(
    client: &mut Client,
    store: &Store,
    attempt: &Attempt,
) -> Result<Option<Turn>> {
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

pub(crate) fn finish(store: &mut Store, input: &Input, turn: Turn) -> Result<()> {
    match turn.status {
        TurnStatus::Completed if input.attention_id.is_some() => store.finish(input.seq, &[], None),
        TurnStatus::Completed => {
            let mut answers = turn.answers()?;
            if input.external_batch.is_some() {
                answers.retain(|answer| answer.text.trim() != "TEND_NO_UPDATE");
            }
            store.finish(input.seq, &answers, None)
        }
        TurnStatus::Failed | TurnStatus::Interrupted => {
            let error = turn
                .error
                .as_ref()
                .map_or("Codex turn ended early", |error| &error.message);
            store.finish(input.seq, &[], Some(error))
        }
        TurnStatus::InProgress => unreachable!("only a terminal turn can finish an input"),
    }
}
