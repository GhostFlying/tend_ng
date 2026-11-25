use anyhow::{Context, Result, bail, ensure};
use protocol::{
    ApiError, Attention, BackfillReportRevision, DEFAULT_LISTEN, FindPull, IssueDetail,
    IssueMessagePage, LinkPull, MessagePage, NotificationPage, PullDiscovery, PullLink,
    PullLinkReceipt, REPORT_REVISIONS_PATH, Receipt, SendMessage, TOKEN_ENV,
};
use std::{env, io, time::Duration};

const USAGE: &str = "Usage:\n  tend chat send <chat-id> <text> [operation-id] [--reply-to <message-id>]\n  tend chat read <chat-id> [before]\n  tend attention list <chat-id>\n  tend attention approve <chat-id> <attention-id> <version>\n  tend notification list <chat-id> [before]\n  tend issue list\n  tend issue list --scope=all|--scope=roots [board-cursor]\n  tend issue list <chat-id> [board-cursor]\n  tend issue read <issue-id|#number>\n  tend report backfill <old-report-id> <new-report-id> [operation-id] [--old-message <message-id>] [--new-message <message-id>]\n  tend issue start <issue-id|#number>\n  tend issue reviews <issue-id|#number>\n  tend issue cancel <issue-id|#number> <reason> [operation-id]\n  tend issue reopen <issue-id|#number> <reason> [operation-id]\n  tend pr link <issue-id|#number> <url> [operation-id]\n  tend pr list <issue-id|#number>\n  tend pr watches <issue-id|#number>\n  tend pr require <message-id> <condition-json> [operation-id]\n  tend pr find <issue-id|#number> <task-id> [operation-id]\n  tend pr discoveries <issue-id|#number> [operation-id]\n  tend issue messages <issue-id|#number> [before]\n  tend issue message <issue-id|#number> <text> [operation-id] [--reply-to <message-id>]\n  tend agent list\n  tend agent candidates <category> [requirements-json]\n  tend runner list\n  tend runner replace <predecessor-id> <successor-id>\n  tend runner retire <runner-id>\n  tend server upgrade-check <target-protocol>\n  tend workspace reclaim <issue-id|#number> [operation-id]\n  tend workspace status <operation-id>\n  tend task stop <task-id> [operation-id]\n  tend run steer <run-id> <text> [operation-id]\n  tend control read <operation-id>\n  tend control queue <operation-id>\n  tend run rollout <run-id>\n  tend artifact download <file-id>\n\nTEND_URL sets the Server address. TEND_TOKEN authenticates the request.";

fn main() -> Result<()> {
    let mut args: Vec<_> = env::args().skip(1).collect();
    if args == ["runner", "list"] || args == ["agent", "list"] {
        args.push("all".to_owned());
    }
    if args == ["issue", "list"] {
        args.push("--scope=all".to_owned());
    }
    if args.is_empty() || args == ["--help"] {
        println!("{USAGE}");
        return Ok(());
    }
    let [namespace, command, id, rest @ ..] = args.as_slice() else {
        bail!("{USAGE}");
    };
    let issue_scope = namespace == "issue" && command == "list";
    let all_issues = issue_scope && id == "--scope=all";
    let root_issues = issue_scope && id == "--scope=roots";
    let issue_addressed = matches!(
        (namespace.as_str(), command.as_str()),
        (
            "issue",
            "read" | "start" | "reviews" | "messages" | "message" | "cancel" | "reopen"
        ) | ("pr", "link" | "list" | "watches" | "find" | "discoveries")
            | ("workspace", "reclaim")
    );
    let id = match id.strip_prefix('#') {
        Some(digits) if issue_addressed && protocol::parse_issue_number(id).is_some() => digits,
        _ => id.as_str(),
    };
    ensure!(
        all_issues || root_issues || protocol::valid_id(id),
        "{USAGE}"
    );

    let base = env::var("TEND_URL").unwrap_or_else(|_| format!("http://{DEFAULT_LISTEN}"));
    let endpoint = match (namespace.as_str(), command.as_str()) {
        ("chat", _) => protocol::messages_path(id),
        ("issue", "read") => format!("{}?include=reports", protocol::issue_path(id)),
        ("report", "backfill") => REPORT_REVISIONS_PATH.to_owned(),
        ("issue", "start") => format!("/api/v1/issues/{id}/start"),
        ("issue", "reviews") => format!("/api/v1/issues/{id}/reviews"),
        ("issue", "list") if all_issues => protocol::ISSUE_BOARD_PATH.to_owned(),
        ("issue", "list") if root_issues => protocol::TOP_LEVEL_BOARD_PATH.to_owned(),
        ("issue", "list") => protocol::issue_board_path(id),
        ("pr", "require") => format!("/api/v1/messages/{id}/required-facts"),
        ("pr", "watches") => format!("/api/v1/issues/{id}/watches"),
        ("pr", "link" | "list") => protocol::issue_pulls_path(id),
        ("pr", "find" | "discoveries") => protocol::pull_discoveries_path(id),
        ("issue", "messages" | "message") => protocol::issue_messages_path(id),
        ("agent", "list") => "/api/v1/agents".to_owned(),
        ("agent", "candidates") => "/api/v1/agent-candidates".to_owned(),
        ("runner", "list") => "/api/v1/runners".to_owned(),
        ("runner", "replace" | "retire") => format!("/api/v1/runners/{id}/{command}"),
        ("server", "upgrade-check") => format!("/api/v1/upgrade-check/{id}"),
        ("workspace", "reclaim") => format!("/api/v1/issues/{id}/reclaim"),
        ("workspace", "status") => format!("/api/v1/workspace-reclaims/{id}"),
        ("task", "stop") => format!("/api/v1/tasks/{id}/stop"),
        ("run", "steer") => format!("/api/v1/runs/{id}/steer"),
        ("control", "read") => format!("/api/v1/controls/{id}"),
        ("control", "queue") => format!("/api/v1/controls/{id}/queue"),
        ("issue", "cancel" | "reopen") => format!("/api/v1/issues/{id}/state"),
        ("run", "rollout") => protocol::rollout_path(id),
        ("artifact", _) => protocol::artifact_path(id),
        ("attention", _) => protocol::attentions_path(id),
        ("notification", "list") => protocol::notifications_path(id),
        _ => bail!("{USAGE}"),
    };
    let path = format!("{}{}", base.trim_end_matches('/'), endpoint);
    let token = env::var(TOKEN_ENV).context("set TEND_TOKEN before calling Tend")?;
    ensure!(!token.is_empty(), "TEND_TOKEN is empty");
    let auth = format!("Bearer {token}");
    let http = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(5))
        .build();

    match (namespace.as_str(), command.as_str(), rest) {
        ("chat", "send", [text, arguments @ ..]) | ("issue", "message", [text, arguments @ ..]) => {
            let (operation, reply) = match arguments {
                [] => (None, None),
                [operation] => (Some(operation), None),
                [flag, reply] if flag == "--reply-to" => (None, Some(reply)),
                [operation, flag, reply] if flag == "--reply-to" => (Some(operation), Some(reply)),
                _ => bail!("{USAGE}"),
            };
            let operation_id = operation.cloned().unwrap_or_else(protocol::new_id);
            ensure!(protocol::valid_id(&operation_id), "invalid operation ID");
            ensure!(
                reply.is_none_or(|id| protocol::valid_id(id)),
                "invalid reply message ID"
            );
            let request = SendMessage {
                operation_id,
                text: text.clone(),
                reply_to: reply.cloned(),
            };
            let receipt: Receipt = send(&http, &path, &auth, &serde_json::to_value(&request)?)
                .and_then(|response| Ok(response.into_json()?))
                .with_context(|| format!("send operation {}", request.operation_id))?;
            println!("{}", serde_json::to_string(&receipt)?);
        }
        ("chat", "read", [] | [_])
        | ("issue", "messages" | "list", [] | [_])
        | ("notification", "list", [] | [_]) => {
            let mut request = http.get(&path).set("Authorization", &auth);
            if let Some(before) = rest.first() {
                if issue_scope {
                    // Also pass through legacy numeric cursors when talking to an older Server.
                    ensure!(
                        protocol::IssueBoardCursor::decode(before).is_some()
                            || before.parse::<i64>().is_ok_and(|value| value > 0),
                        "invalid Issue board cursor"
                    );
                    request = request.query("before", before);
                } else {
                    request = request.query("before", &before.parse::<i64>()?.to_string());
                }
            }
            let response = checked(request.call())?;
            let page = match command.as_str() {
                "messages" => serde_json::to_string(&response.into_json::<IssueMessagePage>()?)?,
                "list" if namespace == "issue" => {
                    // Preserve the Server's page shape across a client-only upgrade.
                    serde_json::to_string(&response.into_json::<serde_json::Value>()?)?
                }
                "list" => serde_json::to_string(&response.into_json::<NotificationPage>()?)?,
                _ => serde_json::to_string(&response.into_json::<MessagePage>()?)?,
            };
            println!("{page}");
        }
        ("agent" | "runner", "list", [])
        | ("server", "upgrade-check", [])
        | ("workspace", "status", []) => {
            let response = checked(http.get(&path).set("Authorization", &auth).call())?;
            io::copy(&mut response.into_reader(), &mut io::stdout().lock())?;
        }
        ("runner", "replace", [successor]) => {
            ensure!(protocol::valid_id(successor), "invalid successor Runner ID");
            let response = send(
                &http,
                &path,
                &auth,
                &serde_json::json!({"successor":successor}),
            )?;
            io::copy(&mut response.into_reader(), &mut io::stdout().lock())?;
        }
        ("runner", "retire", []) => {
            let response = send(&http, &path, &auth, &serde_json::Value::Null)?;
            io::copy(&mut response.into_reader(), &mut io::stdout().lock())?;
        }
        ("task", "stop", [] | [_]) | ("workspace", "reclaim", [] | [_]) => {
            let id = rest.first().cloned().unwrap_or_else(protocol::new_id);
            ensure!(protocol::valid_id(&id), "invalid operation ID");
            let body = serde_json::json!({"operation_id": id});
            let response = send(&http, &path, &auth, &body)
                .with_context(|| format!("control operation {id}"))?;
            io::copy(&mut response.into_reader(), &mut io::stdout().lock())?;
        }
        ("run", "steer", [text, operation @ ..])
        | ("issue", "cancel" | "reopen", [text, operation @ ..])
            if operation.len() <= 1 =>
        {
            let id = operation.first().cloned().unwrap_or_else(protocol::new_id);
            ensure!(protocol::valid_id(&id), "invalid operation ID");
            let body = if command == "steer" {
                serde_json::json!({"operation_id": id, "text": text})
            } else {
                let state = if command == "reopen" {
                    "open"
                } else {
                    "cancelled"
                };
                serde_json::json!({"operation_id": id, "reason": text, "state": state})
            };
            let response = send(&http, &path, &auth, &body)
                .with_context(|| format!("control operation {id}"))?;
            io::copy(&mut response.into_reader(), &mut io::stdout().lock())?;
        }
        ("control", "read" | "queue", []) => {
            let response = if command == "read" {
                checked(http.get(&path).set("Authorization", &auth).call())?
            } else {
                send(&http, &path, &auth, &serde_json::Value::Null)?
            };
            io::copy(&mut response.into_reader(), &mut io::stdout().lock())?;
        }
        ("pr", "link", [url, operation @ ..]) if operation.len() <= 1 => {
            let operation_id = operation.first().cloned().unwrap_or_else(protocol::new_id);
            ensure!(protocol::valid_id(&operation_id), "invalid operation ID");
            let request = LinkPull {
                operation_id,
                url: url.clone(),
            };
            let receipt: PullLinkReceipt =
                send(&http, &path, &auth, &serde_json::to_value(&request)?)
                    .and_then(|response| Ok(response.into_json()?))
                    .with_context(|| format!("link operation {}", request.operation_id))?;
            println!("{}", serde_json::to_string(&receipt)?);
        }
        ("pr", "find", [task_id, operation @ ..]) if operation.len() <= 1 => {
            let operation_id = operation.first().cloned().unwrap_or_else(protocol::new_id);
            ensure!(
                protocol::valid_id(task_id) && protocol::valid_id(&operation_id),
                "invalid Task or operation ID"
            );
            let request = FindPull {
                operation_id,
                task_id: task_id.clone(),
            };
            let receipt: FindPull = send(&http, &path, &auth, &serde_json::to_value(&request)?)
                .and_then(|response| Ok(response.into_json()?))
                .with_context(|| format!("discover operation {}", request.operation_id))?;
            println!("{}", serde_json::to_string(&receipt)?);
        }
        ("pr", "discoveries", [] | [_]) => {
            let mut request = http.get(&path).set("Authorization", &auth);
            if let Some(operation) = rest.first() {
                ensure!(protocol::valid_id(operation), "invalid operation ID");
                request = request.query("operation", operation);
            }
            let jobs: Vec<PullDiscovery> = checked(request.call())?.into_json()?;
            println!("{}", serde_json::to_string(&jobs)?);
        }
        ("pr", "require", [condition] | [condition, _]) => {
            let operation_id = rest.get(1).cloned().unwrap_or_else(protocol::new_id);
            ensure!(protocol::valid_id(&operation_id), "invalid operation ID");
            let mut condition: protocol::ExternalCondition = serde_json::from_str(condition)?;
            ensure!(condition.normalize(), "invalid GitHub condition");
            let request = protocol::RequireExternal {
                operation_id,
                condition,
            };
            let mut response =
                send(&http, &path, &auth, &serde_json::to_value(&request)?)?.into_reader();
            io::copy(&mut response, &mut io::stdout().lock())?;
        }
        ("agent", "candidates", [] | [_]) => {
            let mut requirements = rest
                .first()
                .map(|value| serde_json::from_str::<serde_json::Value>(value))
                .transpose()?
                .unwrap_or_else(|| serde_json::json!({}));
            ensure!(
                requirements.is_object(),
                "requirements must be a JSON object"
            );
            requirements["category"] = serde_json::json!(id);
            let mut response = send(&http, &path, &auth, &requirements)?.into_reader();
            io::copy(&mut response, &mut io::stdout().lock())?;
        }
        ("pr", "watches", []) | ("issue", "reviews", []) => {
            let response = checked(http.get(&path).set("Authorization", &auth).call())?;
            io::copy(&mut response.into_reader(), &mut io::stdout().lock())?;
        }
        ("pr", "list", []) => {
            let links: Vec<PullLink> =
                checked(http.get(&path).set("Authorization", &auth).call())?.into_json()?;
            println!("{}", serde_json::to_string(&links)?);
        }
        ("issue", "read", []) => {
            let response = checked(http.get(&path).set("Authorization", &auth).call())?;
            io::copy(&mut response.into_reader(), &mut io::stdout().lock())?;
            println!();
        }
        ("report", "backfill", [new, arguments @ ..]) => {
            let (operation, flags) = match arguments {
                [operation, flags @ ..] if !operation.starts_with("--") => {
                    (operation.clone(), flags)
                }
                _ => (protocol::new_id(), arguments),
            };
            ensure!(protocol::valid_id(new), "invalid new report ID");
            ensure!(protocol::valid_id(&operation), "invalid operation ID");
            let mut input = BackfillReportRevision {
                operation_id: operation,
                old_report_id: id.to_owned(),
                new_report_id: new.clone(),
                old_message_id: None,
                new_message_id: None,
            };
            let mut flags = flags;
            while let [flag, message, rest @ ..] = flags {
                ensure!(protocol::valid_id(message), "invalid message ID");
                let value = match flag.as_str() {
                    "--old-message" => &mut input.old_message_id,
                    "--new-message" => &mut input.new_message_id,
                    _ => bail!("{USAGE}"),
                };
                ensure!(value.is_none(), "duplicate {flag}");
                *value = Some(message.clone());
                flags = rest;
            }
            ensure!(flags.is_empty(), "{USAGE}");
            let receipt: protocol::ReportRevisionReceipt =
                send(&http, &path, &auth, &serde_json::to_value(&input)?)
                    .with_context(|| format!("backfill operation {}", input.operation_id))?
                    .into_json()?;
            println!("{}", serde_json::to_string(&receipt)?);
        }
        ("issue", "start", []) => {
            let issue: IssueDetail =
                send(&http, &path, &auth, &serde_json::json!({}))?.into_json()?;
            println!("{}", serde_json::to_string(&issue)?);
        }
        ("artifact", "download", []) | ("run", "rollout", []) => {
            let response = checked(http.get(&path).set("Authorization", &auth).call())?;
            io::copy(&mut response.into_reader(), &mut io::stdout().lock())?;
        }
        ("attention", "approve", [attention_id, version]) => {
            ensure!(protocol::valid_id(attention_id), "invalid Attention ID");
            let version = version
                .strip_prefix('v')
                .unwrap_or(version)
                .parse::<u32>()?;
            ensure!(version > 0, "invalid proposal version");
            let path = format!(
                "{}{}",
                base.trim_end_matches('/'),
                protocol::approve_proposal_path(id, attention_id)
            );
            let response = send(
                &http,
                &path,
                &auth,
                &serde_json::to_value(protocol::ApproveProposal {
                    version,
                    from_issue: false,
                })?,
            )?;
            io::copy(&mut response.into_reader(), &mut io::stdout().lock())?;
        }
        ("attention", "list", []) => {
            let attentions: Vec<Attention> =
                checked(http.get(&path).set("Authorization", &auth).call())?.into_json()?;
            println!("{}", serde_json::to_string(&attentions)?);
        }
        _ => bail!("{USAGE}"),
    }

    Ok(())
}

fn send(
    http: &ureq::Agent,
    path: &str,
    auth: &str,
    body: &serde_json::Value,
) -> Result<ureq::Response> {
    let post = || http.post(path).set("Authorization", auth).send_json(body);
    checked(match post() {
        Err(ureq::Error::Transport(_)) => post(),
        result => result,
    })
}

fn checked(result: Result<ureq::Response, ureq::Error>) -> Result<ureq::Response> {
    match result {
        Ok(response) => Ok(response),
        Err(ureq::Error::Status(status, response)) => {
            let message = response
                .into_json::<ApiError>()
                .map(|error| error.error)
                .unwrap_or_else(|_| "request failed".to_owned());
            bail!("Server returned HTTP {status}: {message}");
        }
        Err(error) => Err(error.into()),
    }
}
