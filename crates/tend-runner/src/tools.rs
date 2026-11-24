use crate::store::run::LocalRun;
use anyhow::{Result, bail, ensure};
use codex_app::ToolCall;
use protocol::{DispatchIssue, WORK_CATEGORIES, WorkAction};
use serde::Deserialize;
use serde_json::{Value, json};

const DELEGATE: &str = "tend_delegate_work";
const CLOSE: &str = "tend_close_issue";

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
        (
            "Coordinate the Tend Issue through child Issues. After delegating, end the current turn when ready to let other work run. Child results will return to the same coordination Task. Consider the outcomes and close the Issue when its objective is settled.",
            vec![delegate, close],
        )
    } else {
        (
            "Own this Tend work Issue. Use the assigned workspace and follow the repository instructions. Complete the objective and use tend_close_issue to record the outcome when you decide the Issue is done.",
            vec![close],
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

pub fn action(run: &LocalRun, call: &ToolCall) -> Result<WorkAction> {
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
        _ => bail!("This tool is unavailable for the current Agent"),
    }
}
