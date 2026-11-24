use crate::store::run::LocalRun;
use anyhow::{Result, bail, ensure};
use codex_app::ToolCall;
use protocol::{AttentionRequest, DispatchIssue, WORK_CATEGORIES, WorkAction};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const DELEGATE: &str = "tend_delegate_work";
const CLOSE: &str = "tend_close_issue";
const SUBMIT: &str = "tend_submit_artifact";
const REPORT: &str = "tend_report_result";
const ASK: &str = "tend_request_attention";
const ESCALATE: &str = "tend_escalate_attention";
const ANSWER: &str = "tend_answer_child_attention";
const FORWARD: &str = "tend_forward_issue_message";
const POST: &str = "tend_post_issue_message";

pub fn catalogue(coordinator: bool) -> (&'static str, Vec<Value>) {
    let post = json!({
        "type":"function", "name":POST,
        "description":"Publish a complete message to the current Issue. When answering a message, use its original ID as reply_to.",
        "inputSchema":{"type":"object","properties":{
            "text":{"type":"string"},"reply_to":{"type":"string"}
        },"required":["text"],"additionalProperties":false}
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
    if coordinator {
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
                    "repo_ids": { "type": "array", "items": { "type": "string" } },
                    "required_capabilities": { "type": "array", "items": { "type": "string" } }
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
        (
            "Coordinate the Tend Issue through child Issues. End the current turn when ready to let other work run. Child results will return to the same coordination Task. Decide a child question within your authority or escalate its original ID to the Router. Use tend_request_attention for your questions, tend_report_result for agreed results, and tend_post_issue_message for visible Issue discussion. Reply to direct user messages with their original IDs. Use tend_forward_issue_message when a user supplement belongs to existing child work. Close the Issue when its objective is settled.",
            vec![
                delegate, close, report, ask, escalate, answer, post, forward,
            ],
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
            "Own this Tend work Issue. Use the assigned workspace and follow repository instructions. Use tend_request_attention when a decision exceeds your authority. Finish independent work and yield the turn when waiting. Use tend_post_issue_message with the original message ID when replying in the Issue discussion. Use tend_submit_artifact for deliveries and tend_close_issue when the Issue is done.",
            vec![close, submit, ask, post],
        )
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Delegation {
    title: String,
    objective: String,
    category: String,
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
) -> Result<WorkAction> {
    match call.tool.as_str() {
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
                !args.title.trim().is_empty() && !args.objective.trim().is_empty(),
                "Specify the title and work objective"
            );
            Ok(WorkAction::Delegate {
                issue_id: protocol::new_id(),
                task_id: protocol::new_id(),
                category: args.category,
                work: DispatchIssue {
                    title: args.title,
                    objective: args.objective,
                    repo_ids: args
                        .repo_ids
                        .unwrap_or_else(|| run.assignment.work.repo_ids.clone()),
                    required_capabilities: args.required_capabilities,
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
        ASK => {
            let request: AttentionRequest = serde_json::from_value(call.arguments.clone())?;
            ensure!(
                !request.question.trim().is_empty(),
                "Specify the decision needed"
            );
            Ok(WorkAction::AskAttention { request })
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
