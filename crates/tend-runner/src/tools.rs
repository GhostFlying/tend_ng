use crate::store::run::LocalRun;
use anyhow::{Result, bail, ensure};
use codex_app::ToolCall;
use protocol::{AttentionRequest, DispatchIssue, WORK_CATEGORIES, WorkAction};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const DELEGATE: &str = "tend_delegate_work";
const REQUEST_REVIEW: &str = "tend_request_review";
const DELIVER_REVIEW: &str = "tend_deliver_review";
const CLOSE: &str = "tend_close_issue";
const SUBMIT: &str = "tend_submit_artifact";
const REPORT: &str = "tend_report_result";
const ASK: &str = "tend_request_attention";
const PROPOSE: &str = "tend_propose_approval";
const ESCALATE: &str = "tend_escalate_attention";
const ANSWER: &str = "tend_answer_child_attention";
const FORWARD: &str = "tend_forward_issue_message";
const FIND_PULL: &str = "tend_find_pull_request";
const LINK_PULL: &str = "tend_link_pull_request";
const POST: &str = "tend_post_issue_message";

pub fn catalogue(category: &str, proposal_approval: bool) -> (&'static str, Vec<Value>) {
    let post = json!({
        "type":"function", "name":POST,
        "description":"Publish a complete message to the current Issue. When answering a message, use its original ID as reply_to.",
        "inputSchema":{"type":"object","properties":{
            "text":{"type":"string"},"reply_to":{"type":"string"}
        },"required":["text"],"additionalProperties":false}
    });
    let link_pull = json!({
        "type": "function",
        "name": LINK_PULL,
        "description": "Save a GitHub pull request URL against this Issue. The receipt means the Runner accepted the link for synchronization.",
        "inputSchema": {"type": "object", "properties": {"url": {"type": "string"}},
                        "required": ["url"], "additionalProperties": false}
    });
    let find_pull = json!({
        "type": "function",
        "name": FIND_PULL,
        "description": "Queue a GitHub pull request lookup for a recorded work branch in this Issue tree. Defaults to the current Task.",
        "inputSchema": {"type": "object", "properties": {"task_id": {"type": "string"}},
                        "additionalProperties": false}
    });
    let condition_schema = json!({
        "url": {"type":"string"}, "event":{"type":"string","enum":["pull","ci"]},
        "state":{"type":"string"}, "head_sha":{"type":"string"}, "check":{"type":"string"},
        "run_id":{"type":"string"}
    });
    let waitfor = json!({
        "type":"function", "name":"tend_waitfor",
        "description":"Persist a GitHub condition and yield this Task after the turn stops. A match resumes the same Task. Set state to terminal for any terminal CI conclusion.",
        "inputSchema":{"type":"object","properties":condition_schema,
                       "required":["url","event","state"],"additionalProperties":false}
    });
    let mut subscribe_properties = condition_schema;
    subscribe_properties["required"] = json!({"type":"boolean"});
    let subscribe = json!({
        "type":"function", "name":"tend_subscribe",
        "description":"Subscribe to a GitHub condition while continuing this turn. Use required for facts that must be durably delivered to this Owner.",
        "inputSchema":{"type":"object","properties":subscribe_properties,
                       "required":["url","event","state"],"additionalProperties":false}
    });
    let clear_waits = json!({
        "type":"function", "name":"tend_clear_waits",
        "description":"Withdraw this Task's outstanding GitHub waits when the objective has changed.",
        "inputSchema":{"type":"object","properties":{},"additionalProperties":false}
    });
    let stop_task = json!({
        "type":"function", "name":"tend_stop_task",
        "description":"Request stop for a Task in your own Issue tree or an independent review requested by this Task. Its Runner confirms actual stop.",
        "inputSchema":{"type":"object","properties":{"task_id":{"type":"string"}},
                       "required":["task_id"],"additionalProperties":false}
    });
    let issue_state = json!({
        "type":"function", "name":"tend_set_issue_state",
        "description":"Record your Issue as cancelled or explicitly reopen it. For cancellation, coordinate child work and request the affected Task stops.",
        "inputSchema":{"type":"object","properties":{"state":{"type":"string","enum":["open","cancelled"]}, "reason":{"type":"string"}},
                       "required":["state","reason"],"additionalProperties":false}
    });
    let record_review = json!({
        "type":"function", "name":"tend_record_review",
        "description":"Save independent review evidence from a Codex child thread already observed in this work Issue.",
        "inputSchema":{"type":"object","properties":{
            "child_thread_id":{"type":"string"},"candidate":{"type":"string"},
            "scope":{"type":"string"},"conclusion":{"type":"string"}},
            "required":["child_thread_id","candidate","scope","conclusion"],"additionalProperties":false}
    });
    let close = json!({
        "type": "function",
        "name": CLOSE,
        "description": "Record the Issue owner's decision to close this Issue and summarize the outcome.",
        "inputSchema": {
            "type": "object",
            "properties": { "summary": { "type": "string" } },
            "required": ["summary"],
            "additionalProperties": false
        }
    });
    let ask = json!({
        "type": "function",
        "name": ASK,
        "description": "Record a decision needed to continue this Task. Return the durable question ID and yield the slot when independent work is done.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "question": {"type": "string"},
                "context": {"type": "string"},
                "options": {"type": "array", "items": {"type": "string"}},
                "priority": {"type": "string"},
                "due_at": {"type": "string"}
            },
            "required": ["question"],
            "additionalProperties": false
        }
    });
    let review_handoff = if category == "review" {
        json!({
            "type": "function", "name": DELIVER_REVIEW,
            "description": "Return the conclusion and evidence for this bound review to the exact author Task.",
            "inputSchema": {"type":"object", "properties":{
                "conclusion":{"type":"string"}, "evidence":{"type":"string"}
            }, "required":["conclusion","evidence"], "additionalProperties":false}
        })
    } else {
        json!({
            "type": "function", "name": REQUEST_REVIEW,
            "description": "Request optional independent Tend review of an immutable candidate. End the turn when ready to yield; the result resumes this exact author Task. Use artifact:<file_id> for a submitted file, or exact Git commit base_refs to mount code in a separate workspace.",
            "inputSchema": {"type":"object", "properties":{
                "candidate":{"type":"string"}, "scope":{"type":"string"},
                "agent_id":{"type":"string"},
                "base_refs":{"type":"object","additionalProperties":{"type":"string"}}
            }, "required":["candidate","scope"], "additionalProperties":false}
        })
    };
    if category == "issue-router" {
        let delegate = json!({
            "type": "function",
            "name": DELEGATE,
            "description": "Delegate a distinct work objective to a child Issue on this Runner. The durable acceptance can be completed while Tend Server is offline.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": { "type": "string" },
                    "objective": { "type": "string" },
                    "category": { "type": "string", "enum": WORK_CATEGORIES },
                    "agent_id": { "type": "string" },
                    "repo_ids": { "type": "array", "items": { "type": "string" } },
                    "required_capabilities": { "type": "array", "items": { "type": "string" } },
                    "isolated_workspace": { "type": "boolean" },
                    "base_refs": { "type":"object", "additionalProperties": {"type":"string"} }
                },
                "required": ["title", "objective", "category"],
                "additionalProperties": false
            }
        });
        let report = json!({
            "type": "function",
            "name": REPORT,
            "description": "Return a result to the Router in the original Chat. Reference delivery file IDs received in the child inputs.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "summary": { "type": "string" },
                    "artifact_ids": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["summary"],
                "additionalProperties": false
            }
        });
        let candidates = json!({
            "type":"function", "name":"tend_agent_candidates",
            "description":"List matching work configurations on this Runner. Omitted repositories use the current Issue requirements.",
            "inputSchema":{"type":"object","properties":{
                "category":{"type":"string","enum":WORK_CATEGORIES},
                "repo_ids":{"type":"array","items":{"type":"string"}},
                "required_capabilities":{"type":"array","items":{"type":"string"}}
            },"required":["category"],"additionalProperties":false}
        });
        let escalate = json!({
            "type": "function",
            "name": ESCALATE,
            "description": "Forward the original child question to the Router when the user needs to decide.",
            "inputSchema": {
                "type": "object",
                "properties": {"attention_id": {"type": "string"}},
                "required": ["attention_id"],
                "additionalProperties": false
            }
        });
        let answer = json!({
            "type": "function",
            "name": ANSWER,
            "description": "Send a decision to the original child Task when you can decide it within your authority.",
            "inputSchema": {
                "type": "object",
                "properties": {"attention_id": {"type": "string"}, "answer": {"type": "string"}},
                "required": ["attention_id", "answer"],
                "additionalProperties": false
            }
        });
        let forward = json!({
            "type": "function",
            "name": FORWARD,
            "description": "Forward a user message received by this Issue to one of your direct work children, keeping the original wording.",
            "inputSchema": {"type":"object","properties":{
                "child_issue_id":{"type":"string"},"message_id":{"type":"string"}
            },"required":["child_issue_id","message_id"],"additionalProperties":false}
        });
        let mut catalogue = vec![
            delegate,
            candidates,
            close,
            report,
            ask,
            escalate,
            answer,
            post,
            forward,
            link_pull,
            find_pull,
            waitfor,
            subscribe,
            clear_waits,
            stop_task,
            issue_state,
            record_review,
            review_handoff,
        ];
        if proposal_approval {
            catalogue.push(json!({
                "type":"function", "name": PROPOSE,
                "description": "Ask the user to approve one exact version of a delivered report, only when the user asked this Issue to wait for approval. Reference the tend_report_result operation ID and one immutable Markdown attachment from it. Use version 1 for a new proposal. To revise the same unresolved choice, reference the original Attention ID in replaces and use the next version and a new report and Markdown original. Put the exact version and the concrete next action in question and action. The receipt is local acceptance. Do not create a separate Attention for revisions.",
                "inputSchema": {"type":"object","properties":{
                    "question":{"type":"string"}, "version":{"type":"integer","minimum":1},
                    "report_id":{"type":"string"}, "artifact_id":{"type":"string"},
                    "title":{"type":"string"}, "highlights":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":6},
                    "action":{"type":"string"}, "replaces":{"type":"string"}
                },"required":["question","version","report_id","artifact_id","title","highlights","action"],"additionalProperties":false}
            }));
        }
        (
            include_str!("../../../docs/roles/issue-agent.md"),
            catalogue,
        )
    } else {
        let submit = json!({
            "type": "function",
            "name": SUBMIT,
            "description": "Save an immutable delivery copy of a regular file in this workspace. Returns an operation_id that is also the file ID.",
            "inputSchema": {
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"],
                "additionalProperties": false
            }
        });
        (
            if category == "review" {
                include_str!("../../../docs/roles/review-agent.md")
            } else {
                include_str!("../../../docs/roles/work-agent.md")
            },
            vec![
                close,
                submit,
                ask,
                post,
                link_pull,
                find_pull,
                waitfor,
                subscribe,
                clear_waits,
                stop_task,
                issue_state,
                record_review,
                review_handoff,
            ],
        )
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewRequest {
    candidate: String,
    scope: String,
    agent_id: Option<String>,
    #[serde(default)]
    base_refs: std::collections::BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Delegation {
    #[serde(default)]
    isolated_workspace: bool,
    #[serde(default)]
    base_refs: std::collections::BTreeMap<String, String>,
    title: String,
    objective: String,
    category: String,
    agent_id: Option<String>,
    repo_ids: Option<Vec<String>>,
    #[serde(default)]
    required_capabilities: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Closure {
    summary: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Submission {
    path: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Report {
    summary: String,
    #[serde(default)]
    artifact_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Escalation {
    attention_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChildAnswer {
    attention_id: String,
    answer: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Post {
    text: String,
    reply_to: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Forward {
    child_issue_id: String,
    message_id: String,
}

pub fn action(
    run: &LocalRun,
    call: &ToolCall,
    cwd: &Path,
    artifacts: &Path,
    operation: &str,
    proposal_approval: bool,
) -> Result<WorkAction> {
    match call.tool.as_str() {
        REQUEST_REVIEW if run.assignment.placement.agent.category != "review" => {
            let args: ReviewRequest = serde_json::from_value(call.arguments.clone())?;
            let work = DispatchIssue {
                title: format!("Independent review of {}", run.assignment.work.title),
                objective: format!(
                    "Independently review this immutable candidate: {}. Scope: {}. Deliver findings to the author Task with tend_deliver_review.",
                    args.candidate, args.scope,
                ),
                repo_ids: args.base_refs.keys().cloned().collect(),
                base_refs: args.base_refs,
                required_capabilities: Vec::new(),
                agent: args.agent_id.map(|id| protocol::AgentPin {
                    id,
                    runner_id: None,
                }),
            };
            ensure!(
                protocol::valid_tend_review(&args.candidate, &args.scope, &work)
                    && work
                        .repo_ids
                        .iter()
                        .all(|id| run.assignment.work.repo_ids.contains(id)),
                "Supply the candidate and scope, and pin any assigned repositories by exact commit"
            );
            Ok(WorkAction::RequestReview {
                issue_id: protocol::new_id(),
                task_id: protocol::new_id(),
                candidate: args.candidate,
                scope: args.scope,
                work,
            })
        }
        DELIVER_REVIEW
            if run.assignment.placement.agent.category == "review"
                && run.assignment.review.is_some() =>
        {
            let result: protocol::TendReviewConclusion =
                serde_json::from_value(call.arguments.clone())?;
            ensure!(
                result.valid(),
                "Provide the actual conclusion and associated evidence"
            );
            Ok(WorkAction::DeliverReview { result })
        }
        "tend_record_review" => {
            let review: protocol::ReviewConclusion =
                serde_json::from_value(call.arguments.clone())?;
            ensure!(
                protocol::valid_id(&review.child_thread_id)
                    && [&review.candidate, &review.scope, &review.conclusion]
                        .iter()
                        .all(|value| !value.trim().is_empty()),
                "Use the observed child thread, candidate, scope, and review conclusion"
            );
            Ok(WorkAction::RecordReview { review })
        }
        "tend_stop_task" => {
            let task_id = call.arguments["task_id"]
                .as_str()
                .filter(|id| protocol::valid_id(id))
                .ok_or_else(|| anyhow::anyhow!("Use the exact Task ID"))?;
            Ok(WorkAction::StopTask {
                task_id: task_id.to_owned(),
            })
        }
        "tend_set_issue_state" => {
            let state = call.arguments["state"]
                .as_str()
                .filter(|state| matches!(*state, "open" | "cancelled"))
                .ok_or_else(|| anyhow::anyhow!("Use open or cancelled"))?;
            let reason = call.arguments["reason"]
                .as_str()
                .filter(|reason| !reason.trim().is_empty())
                .ok_or_else(|| anyhow::anyhow!("Provide the reason for this change"))?;
            Ok(WorkAction::SetIssueState {
                state: state.to_owned(),
                reason: reason.to_owned(),
            })
        }
        "tend_waitfor" | "tend_subscribe" => {
            let mut condition: protocol::ExternalCondition =
                serde_json::from_value(call.arguments.clone())?;
            ensure!(
                condition.normalize(),
                "Use a specific GitHub PR or CI condition"
            );
            let mode = match call.tool.as_str() {
                "tend_waitfor" => protocol::WatchMode::Wait,
                _ if call.arguments["required"] == true => protocol::WatchMode::Required,
                _ => protocol::WatchMode::Optional,
            };
            Ok(WorkAction::WatchExternal { condition, mode })
        }
        "tend_clear_waits" => Ok(WorkAction::ClearExternalWaits),
        FIND_PULL => {
            let supplied: Option<String> = call
                .arguments
                .get("task_id")
                .map(|id| serde_json::from_value(id.clone()))
                .transpose()?;
            let task_id = supplied.unwrap_or_else(|| run.assignment.task_id.clone());
            ensure!(protocol::valid_id(&task_id), "Use a recorded Task ID");
            Ok(WorkAction::FindPull { task_id })
        }
        LINK_PULL => {
            let pull = call.arguments["url"]
                .as_str()
                .and_then(protocol::GitHubPull::parse)
                .ok_or_else(|| anyhow::anyhow!("Use a GitHub pull request URL"))?;
            Ok(WorkAction::LinkPull { url: pull.url() })
        }
        POST => {
            let post: Post = serde_json::from_value(call.arguments.clone())?;
            ensure!(
                !post.text.trim().is_empty(),
                "Write the complete Issue message"
            );
            ensure!(
                post.reply_to
                    .as_ref()
                    .is_none_or(|id| protocol::valid_id(id)),
                "Use the original reply ID"
            );
            Ok(WorkAction::PostIssueMessage {
                text: post.text,
                reply_to: post.reply_to,
            })
        }
        FORWARD if run.assignment.placement.agent.category == "issue-router" => {
            let forward: Forward = serde_json::from_value(call.arguments.clone())?;
            ensure!(
                protocol::valid_id(&forward.child_issue_id)
                    && protocol::valid_id(&forward.message_id),
                "Use the original child Issue and message IDs"
            );
            Ok(WorkAction::ForwardIssueMessage {
                child_issue_id: forward.child_issue_id,
                message_id: forward.message_id,
            })
        }
        DELEGATE if run.assignment.placement.agent.category == "issue-router" => {
            let args: Delegation = serde_json::from_value(call.arguments.clone())?;
            ensure!(
                WORK_CATEGORIES.contains(&args.category.as_str()),
                "Unknown work category"
            );
            ensure!(
                !args.title.trim().is_empty()
                    && !args.objective.trim().is_empty()
                    && args.agent_id.as_deref().is_none_or(protocol::valid_id),
                "Specify the title and work objective, with a valid Agent ID when naming one"
            );
            let repo_ids = args
                .repo_ids
                .unwrap_or_else(|| run.assignment.work.repo_ids.clone());
            ensure!(
                args.base_refs.iter().all(
                    |(repo, reference)| repo_ids.contains(repo) && !reference.trim().is_empty()
                ),
                "A base reference selects a requested repository and a branch or commit"
            );
            let mut base_refs = run.assignment.work.base_refs.clone();
            base_refs.retain(|repo, _| repo_ids.contains(repo));
            base_refs.extend(args.base_refs);
            Ok(WorkAction::Delegate {
                issue_id: protocol::new_id(),
                task_id: protocol::new_id(),
                category: args.category,
                isolated_workspace: args.isolated_workspace,
                work: DispatchIssue {
                    title: args.title,
                    objective: args.objective,
                    repo_ids,
                    base_refs,
                    required_capabilities: args.required_capabilities,
                    agent: args.agent_id.map(|id| protocol::AgentPin {
                        id,
                        runner_id: None,
                    }),
                },
            })
        }
        CLOSE => {
            let args: Closure = serde_json::from_value(call.arguments.clone())?;
            ensure!(
                !args.summary.trim().is_empty(),
                "Summarize the Issue outcome"
            );
            Ok(WorkAction::CloseIssue {
                summary: args.summary,
            })
        }
        SUBMIT if run.assignment.placement.agent.category != "issue-router" => {
            let args: Submission = serde_json::from_value(call.arguments.clone())?;
            crate::artifacts::snapshot(artifacts, cwd, &args.path, operation)
        }
        REPORT if run.assignment.placement.agent.category == "issue-router" => {
            let mut args: Report = serde_json::from_value(call.arguments.clone())?;
            ensure!(!args.summary.trim().is_empty(), "Summarize the result");
            args.artifact_ids.sort();
            args.artifact_ids.dedup();
            Ok(WorkAction::ReportIssue {
                summary: args.summary,
                artifact_ids: args.artifact_ids,
            })
        }
        PROPOSE
            if run.assignment.parent_issue_id.is_none()
                && run.assignment.placement.agent.category == "issue-router" =>
        {
            ensure!(
                proposal_approval,
                "The connected Server does not advertise proposal approval. Ask a concrete question in the original Chat instead."
            );
            let mut metadata = call.arguments.clone();
            let question = metadata
                .as_object_mut()
                .and_then(|fields| fields.remove("question"))
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default();
            let proposal: protocol::ProposalRequest = serde_json::from_value(metadata)?;
            ensure!(
                proposal.valid()
                    && !question.trim().is_empty()
                    && question.chars().count() <= 300
                    && !question.contains(['\n', '\r']),
                "Provide one concrete question and an exact proposal version"
            );
            Ok(WorkAction::AskAttention {
                request: AttentionRequest {
                    question,
                    context: String::new(),
                    options: Vec::new(),
                    priority: None,
                    due_at: None,
                },
                proposal: Some(proposal),
            })
        }
        ASK => {
            let request: AttentionRequest = serde_json::from_value(call.arguments.clone())?;
            ensure!(
                !request.question.trim().is_empty(),
                "Specify the decision needed"
            );
            Ok(WorkAction::AskAttention {
                request,
                proposal: None,
            })
        }
        ESCALATE if run.assignment.placement.agent.category == "issue-router" => {
            let args: Escalation = serde_json::from_value(call.arguments.clone())?;
            ensure!(
                protocol::valid_id(&args.attention_id),
                "A valid question ID is required"
            );
            Ok(WorkAction::EscalateAttention {
                attention_id: args.attention_id,
            })
        }
        ANSWER if run.assignment.placement.agent.category == "issue-router" => {
            let args: ChildAnswer = serde_json::from_value(call.arguments.clone())?;
            ensure!(
                protocol::valid_id(&args.attention_id) && !args.answer.trim().is_empty(),
                "Specify a question ID and its answer"
            );
            Ok(WorkAction::AnswerChildAttention {
                attention_id: args.attention_id,
                answer: args.answer,
            })
        }
        _ => bail!("This tool is unavailable for the current Agent"),
    }
}
