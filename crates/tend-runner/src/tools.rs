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
const RENAME: &str = "tend_rename_issue";
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

pub fn catalogue(
    assignment: &protocol::TaskAssignment,
    proposal_approval: bool,
    stage_attention: bool,
    issue_rename: bool,
) -> (&'static str, Vec<Value>) {
    let category = assignment.placement.agent.category.as_str();
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
        "description": "Save a GitHub PR or Codebase MR URL against this Issue. The receipt means the Runner accepted the link for synchronization.",
        "inputSchema": {"type": "object", "properties": {"url": {"type": "string"}},
                        "required": ["url"], "additionalProperties": false}
    });
    let find_pull = json!({
        "type": "function",
        "name": FIND_PULL,
        "description": "Queue a GitHub PR or Codebase MR lookup for a recorded work branch in this Issue tree. Defaults to the current Task.",
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
    let mut ask = json!({
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
    if assignment.owns_root() && stage_attention {
        ask["inputSchema"]["properties"]["after_report_id"] = json!({
            "type":"string",
            "description":"For a decision that must follow a stage delivery, use the operation ID returned by this Task's tend_report_result. Tend publishes the question only after that report reaches the original Chat."
        });
    }
    let review_handoff = if category == "review" {
        json!({
            "type": "function", "name": DELIVER_REVIEW,
            "description": "Return the conclusion and evidence for this Run's bound request and candidate to the exact author Task. Retries return the original receipt.",
            "inputSchema": {"type":"object", "properties":{
                "conclusion":{"type":"string"}, "evidence":{"type":"string"}
            }, "required":["conclusion","evidence"], "additionalProperties":false}
        })
    } else {
        json!({
            "type": "function", "name": REQUEST_REVIEW,
            "description": "Request independent Tend review in your existing workspace. You may finish the turn before yielding; the reviewer starts after you stop. Use artifact:<file_id> or exact Git commit base_refs. For a new candidate after a result, pass its child_task_id as review_task_id to reuse the same Review Issue, Task and Reviewer. Omit agent_id on a re-review.",
            "inputSchema": {"type":"object", "properties":{
                "candidate":{"type":"string"}, "scope":{"type":"string"},
                "agent_id":{"type":"string"},
                "review_task_id":{"type":"string"},
                "base_refs":{"type":"object","additionalProperties":{"type":"string"}}
            }, "required":["candidate","scope"], "additionalProperties":false}
        })
    };
    let mut catalogue = vec![
        close,
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
    ];
    if issue_rename {
        catalogue.push(json!({
            "type": "function", "name": RENAME,
            "description": format!(
                "Rename only the Issue you currently own. Give its concrete work and expected outcome in at most {} UTF-8 bytes. This receipt is Runner-local acceptance, not a Server-applied title or a scope change.",
                protocol::MAX_ISSUE_TITLE_BYTES,
            ),
            "inputSchema": {"type":"object","properties":{"title":{"type":"string"}},
                            "required":["title"],"additionalProperties":false}
        }));
    }
    if category != "issue-router" {
        catalogue.push(json!({
            "type": "function", "name": SUBMIT,
            "description": "Save an immutable delivery copy of a regular file in this workspace. Returns an operation_id that is also the file ID.",
            "inputSchema": {
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"],
                "additionalProperties": false
            }
        }));
    }
    if assignment.owns_root() {
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
            "description": "Return a result to the Router in the original Chat. Reference immutable delivery file IDs submitted by this Issue or received in child inputs.",
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
        catalogue.extend([delegate, candidates, report, escalate, answer, forward]);
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
    }
    let instructions = match category {
        "issue-router" => include_str!("../../../docs/roles/issue-agent.md"),
        "review" => include_str!("../../../docs/roles/review-agent.md"),
        _ if assignment.owns_root() => concat!(
            include_str!("../../../docs/roles/work-agent.md"),
            "\n",
            include_str!("../../../docs/roles/direct-owner.md"),
        ),
        _ => include_str!("../../../docs/roles/work-agent.md"),
    };
    (instructions, catalogue)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewRequest {
    candidate: String,
    scope: String,
    agent_id: Option<String>,
    review_task_id: Option<String>,
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
    stage_attention: bool,
) -> Result<WorkAction> {
    match call.tool.as_str() {
        REQUEST_REVIEW if run.assignment.placement.agent.category != "review" => {
            let args: ReviewRequest = serde_json::from_value(call.arguments.clone())?;
            let rereview = args.review_task_id.is_some();
            ensure!(
                args.review_task_id
                    .as_deref()
                    .is_none_or(protocol::valid_id)
                    && (!rereview || args.agent_id.is_none()),
                "Use the original review_task_id without a new agent_id for re-review"
            );
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
                    && protocol::valid_shared_review_target(&args.candidate, &work)
                    && work
                        .repo_ids
                        .iter()
                        .all(|id| run.assignment.work.repo_ids.contains(id)),
                "Supply the candidate and scope, and pin any assigned repositories by exact commit"
            );
            Ok(WorkAction::RequestReview {
                issue_id: if rereview {
                    String::new()
                } else {
                    protocol::new_id()
                },
                task_id: args.review_task_id.unwrap_or_else(protocol::new_id),
                candidate: args.candidate,
                scope: args.scope,
                work,
                rereview,
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
            Ok(WorkAction::DeliverReview {
                result,
                request_id: run
                    .assignment
                    .review
                    .as_ref()
                    .map(|review| review.request_id.clone()),
            })
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
                .and_then(protocol::PullRef::parse)
                .ok_or_else(|| anyhow::anyhow!("Use a GitHub PR or Codebase MR URL"))?;
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
        FORWARD if run.assignment.owns_root() => {
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
        DELEGATE if run.assignment.owns_root() => {
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
        RENAME if call.namespace.is_none() => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Rename {
                title: String,
            }
            let args: Rename = serde_json::from_value(call.arguments.clone())?;
            let title = protocol::normalize_issue_title(&args.title).ok_or_else(|| {
                anyhow::anyhow!(
                    "Provide a nonempty single-line Issue title of at most {} UTF-8 bytes",
                    protocol::MAX_ISSUE_TITLE_BYTES,
                )
            })?;
            Ok(WorkAction::RenameIssue {
                issue_id: run.assignment.issue_id.clone(),
                title: title.to_owned(),
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
        REPORT if run.assignment.owns_root() => {
            let mut args: Report = serde_json::from_value(call.arguments.clone())?;
            ensure!(!args.summary.trim().is_empty(), "Summarize the result");
            args.artifact_ids.sort();
            args.artifact_ids.dedup();
            Ok(WorkAction::ReportIssue {
                summary: args.summary,
                artifact_ids: args.artifact_ids,
            })
        }
        PROPOSE if run.assignment.owns_root() => {
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
            let mut arguments = call.arguments.clone();
            let after_report_id: Option<String> = arguments
                .as_object_mut()
                .and_then(|fields| fields.remove("after_report_id"))
                .map(serde_json::from_value)
                .transpose()?;
            ensure!(
                after_report_id
                    .as_deref()
                    .is_none_or(|id| { run.assignment.owns_root() && protocol::valid_id(id) }),
                "A stage Attention needs this top-level Task's original report operation ID"
            );
            ensure!(
                after_report_id.is_none() || stage_attention,
                "The connected Server does not support report-bound Attention. Do not replace it with an independent question."
            );
            let request: AttentionRequest = serde_json::from_value(arguments)?;
            ensure!(
                !request.question.trim().is_empty(),
                "Specify the decision needed"
            );
            Ok(match after_report_id {
                Some(report_id) => WorkAction::AskStageAttention { request, report_id },
                None => WorkAction::AskAttention {
                    request,
                    proposal: None,
                },
            })
        }
        ESCALATE if run.assignment.owns_root() => {
            let args: Escalation = serde_json::from_value(call.arguments.clone())?;
            ensure!(
                protocol::valid_id(&args.attention_id),
                "A valid question ID is required"
            );
            Ok(WorkAction::EscalateAttention {
                attention_id: args.attention_id,
            })
        }
        ANSWER if run.assignment.owns_root() => {
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
