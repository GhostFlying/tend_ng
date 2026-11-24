use crate::store::run::LocalRun;
use anyhow::{Result, bail, ensure};
use codex_app::ToolCall;
use protocol::{DispatchIssue, WORK_CATEGORIES, WorkAction};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const DELEGATE: &str = "tend_delegate_work";
const CLOSE: &str = "tend_close_issue";
const SUBMIT: &str = "tend_submit_artifact";
const REPORT: &str = "tend_report_result";

pub fn catalogue(coordinator: bool) -> (&'static str, Vec<Value>) {
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
        (
            "Coordinate the Tend Issue through child Issues. After delegating, end the current turn when ready to let other work run. Child results will return to the same coordination Task. Use tend_report_result to return an agreed result and available delivery IDs to the Router. Close the Issue when its objective is settled.",
            vec![delegate, close, report],
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
            "Own this Tend work Issue. Use the assigned workspace and follow repository instructions. Use tend_submit_artifact for files the user should receive. Use tend_close_issue to record your decision when the Issue is done.",
            vec![close, submit],
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

pub fn action(
    run: &LocalRun,
    call: &ToolCall,
    cwd: &Path,
    artifacts: &Path,
    operation: &str,
) -> Result<WorkAction> {
    match call.tool.as_str() {
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
        _ => bail!("This tool is unavailable for the current Agent"),
    }
}
