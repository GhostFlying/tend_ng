use anyhow::{Context, Result, bail, ensure};
use protocol::{DEFAULT_LISTEN, DISCUSSIONS_PATH, TOKEN_ENV, valid_id};
use serde_json::{Value, json};
use std::{env, time::Duration};

const USAGE: &str = "Usage:
  tend discussion list [--archived]
  tend discussion create [title] [--from-issue <issue>] [--from-main <message>] [--operation <id>]
  tend discussion read <chat>
  tend discussion rename <chat> <title>
  tend discussion archive <chat>
  tend discussion restore <chat>
  tend discussion candidates [search] [--target]
  tend discussion workspaces <issue>
  tend discussion attach <chat> repo <repo-id>
  tend discussion attach <chat> issue <issue-id-or-number>
  tend discussion attach <chat> pull <url>
  tend discussion attach <chat> worktree <issue-id> <task-id> <repo-id>
  tend discussion detach <chat> <reference-id>
  tend discussion draft <chat> [operation-id]
  tend discussion draft-manual <chat> [operation-id]
  tend discussion draft-read <chat> [draft-id]
  tend discussion handoff-now <chat> [operation-id]
  tend discussion handoff-status <chat> [operation-id]
  tend discussion handoff-retry <chat> <operation-id> <failed-generation>
  tend discussion handoff <chat> <approved-json> [operation-id]

Read or send discussion messages with tend chat read/send <chat>.
No explicit title: use the first user prompt, or 未命名讨论 until then.
handoff-now queues a summary in the original discussion and sends it on success.
It shares context with Main Chat and is not new implementation authorization.
Reuse the operation ID after an uncertain response. handoff-status reports the
receipt or failed generation; handoff-retry replays only that failed generation.
For the optional reviewed handoff, approved JSON fields: draft_id, goal,
decisions, pending_items (optional), constraints, authorization_end, optional
reference_ids and target_issue_id.";

pub fn run(args: &[String]) -> Result<()> {
    let args = args.iter().map(String::as_str).collect::<Vec<_>>();
    if matches!(args.as_slice(), [] | ["--help"]) {
        println!("{USAGE}");
        return Ok(());
    }
    let base = env::var("TEND_URL").unwrap_or_else(|_| format!("http://{DEFAULT_LISTEN}"));
    let token = env::var(TOKEN_ENV).context("set TEND_TOKEN before calling Tend")?;
    ensure!(!token.is_empty(), "TEND_TOKEN is empty");
    let http = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(20))
        .build();
    let auth = format!("Bearer {token}");
    let root = format!("{}{DISCUSSIONS_PATH}", base.trim_end_matches('/'));
    let results = match args.as_slice() {
        ["list"] | ["list", "--archived"] => {
            let request = http.get(&root).set("Authorization", &auth);
            let request = if args.len() == 2 {
                request.query("archived", "true")
            } else {
                request
            };
            super::checked(request.call())?
        }
        ["create", rest @ ..] => {
            let mut rest = rest.iter().copied().peekable();
            let title = if rest.peek().is_some_and(|value| !value.starts_with("--")) {
                rest.next()
            } else {
                None
            };
            let mut id = None;
            let mut origin = None;
            while let Some(flag) = rest.next() {
                match (flag, rest.next()) {
                    ("--from-issue", Some(issue)) if origin.is_none() => {
                        origin = Some(json!({"kind":"issue","issue_id":issue}))
                    }
                    ("--from-main", Some(message)) if origin.is_none() && valid_id(message) => {
                        origin = Some(json!({"kind":"chat","chat_id":"main","message_id":message}))
                    }
                    ("--operation", Some(op)) if id.is_none() && valid_id(op) => id = Some(op),
                    _ => bail!("{USAGE}"),
                }
            }
            let id = id.map_or_else(protocol::new_id, str::to_owned);
            super::send(
                &http,
                &root,
                &auth,
                &json!({"operation_id":id,"title":title,"origin":origin}),
            )
            .with_context(|| format!("discussion operation {id}"))?
        }
        ["candidates", rest @ ..] if rest.len() <= 2 => {
            let target = rest.contains(&"--target");
            let mut terms = rest.iter().filter(|term| **term != "--target");
            let query = terms.next().copied().unwrap_or("");
            ensure!(terms.next().is_none(), "{USAGE}");
            super::checked(
                http.get(&format!("{root}/candidates"))
                    .set("Authorization", &auth)
                    .query("q", query)
                    .query("target", if target { "true" } else { "false" })
                    .call(),
            )?
        }
        ["workspaces", issue] => super::checked(
            http.get(&format!("{root}/workspaces"))
                .set("Authorization", &auth)
                .query("issue", issue)
                .call(),
        )?,
        [command, chat, rest @ ..] if valid_id(chat) => {
            let path = format!("{root}/{chat}");
            match (*command, rest) {
                ("read", []) => super::checked(http.get(&path).set("Authorization", &auth).call())?,
                ("rename", [title]) => super::checked(
                    http.request("PATCH", &path)
                        .set("Authorization", &auth)
                        .send_json(json!({"title":title})),
                )?,
                ("archive" | "restore", []) => super::checked(
                    http.request("PATCH", &path)
                        .set("Authorization", &auth)
                        .send_json(json!({"archived":*command == "archive"})),
                )?,
                ("attach", args) => {
                    let body = match args {
                        ["repo", repo] => json!({"kind":"repo","repo_id":repo}),
                        ["issue", issue] => json!({"kind":"issue","issue_id":issue}),
                        ["pull", url] => json!({"kind":"pull","url":url}),
                        ["worktree", issue, task, repo] => {
                            json!({"kind":"worktree","issue_id":issue,"task_id":task,"repo_id":repo})
                        }
                        _ => bail!("{USAGE}"),
                    };
                    super::send(&http, &format!("{path}/references"), &auth, &body)?
                }
                ("detach", [id]) => super::checked(
                    http.delete(&format!("{path}/references"))
                        .set("Authorization", &auth)
                        .query("id", id)
                        .call(),
                )?,
                ("draft-read", [] | [_]) => {
                    let req = http
                        .get(&format!("{path}/draft"))
                        .set("Authorization", &auth);
                    let req = rest
                        .first()
                        .map_or_else(|| req.clone(), |id| req.clone().query("id", id));
                    super::checked(req.call())?
                }
                ("draft" | "draft-manual", [] | [_]) => {
                    let id = rest
                        .first()
                        .map_or_else(protocol::new_id, |id| (*id).to_owned());
                    ensure!(valid_id(&id), "invalid operation ID");
                    super::send(
                        &http,
                        &format!("{path}/draft"),
                        &auth,
                        &json!({"operation_id":id,"manual":*command == "draft-manual"}),
                    )
                    .with_context(|| format!("discussion draft operation {id}"))?
                }
                ("handoff-status" | "handoff-now", [] | [_]) => {
                    let endpoint = format!("{path}/handoff");
                    let operation = rest.first().copied();
                    ensure!(operation.is_none_or(valid_id), "invalid operation ID");
                    let get = || http.get(&endpoint).set("Authorization", &auth);
                    if *command == "handoff-status" {
                        let req = match operation {
                            Some(id) => get().query("operation_id", id),
                            None => get(),
                        };
                        super::checked(req.call())?
                    } else {
                        let id = match operation {
                            Some(id) => id.to_owned(),
                            None => {
                                super::checked(get().call())?
                                    .into_json::<protocol::DiscussionQuickHandoff>()?
                                    .operation_id
                            }
                        };
                        return print_quick_receipt(
                            super::send(&http, &endpoint, &auth, &json!({"operation_id":id})),
                            &id,
                        );
                    }
                }
                ("handoff-retry", [id, generation]) if valid_id(id) => {
                    let generation: i64 =
                        generation.parse().context("expected failed generation")?;
                    ensure!(generation > 0, "failed generation must be positive");
                    return print_quick_receipt(
                        super::send(
                            &http,
                            &format!("{path}/handoff"),
                            &auth,
                            &json!({"operation_id":id, "retry_generation":generation}),
                        ),
                        id,
                    );
                }
                ("handoff", [approved, extra @ ..]) if extra.len() <= 1 => {
                    let mut body: Value =
                        serde_json::from_str(approved).context("parse approved draft JSON")?;
                    let object = body
                        .as_object_mut()
                        .context("expected an approved draft object")?;
                    let id = extra
                        .first()
                        .copied()
                        .or_else(|| object.get("operation_id").and_then(Value::as_str))
                        .map_or_else(protocol::new_id, str::to_owned);
                    ensure!(valid_id(&id), "invalid operation ID");
                    object.insert("operation_id".into(), json!(id));
                    super::send(&http, &format!("{path}/handoffs"), &auth, &body)
                        .with_context(|| format!("discussion handoff operation {id}"))?
                }
                _ => bail!("{USAGE}"),
            }
        }
        _ => bail!("{USAGE}"),
    };
    let body: Value = results.into_json()?;
    println!("{}", serde_json::to_string(&body)?);
    Ok(())
}

fn print_quick_receipt(response: Result<ureq::Response>, operation: &str) -> Result<()> {
    let body = response
        .and_then(|response| Ok(response.into_json::<protocol::DiscussionQuickHandoff>()?))
        .with_context(|| format!("quick handoff operation {operation}"))?;
    ensure!(
        body.operation_id == operation,
        "uncertain response for quick handoff operation {operation}"
    );
    println!("{}", serde_json::to_string(&body)?);
    Ok(())
}
