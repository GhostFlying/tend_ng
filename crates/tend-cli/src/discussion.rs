use anyhow::{Context, Result, bail, ensure};
use protocol::{DEFAULT_LISTEN, DISCUSSIONS_PATH, TOKEN_ENV, valid_id};
use serde_json::{Value, json};
use std::{env, time::Duration};

const USAGE: &str = "Usage:
  tend discussion list
  tend discussion create [title] [--from-issue <issue>] [--from-main <message>] [--operation <id>]
  tend discussion read <chat>
  tend discussion rename <chat> <title>
  tend discussion candidates [search] [--target]
  tend discussion workspaces <issue>
  tend discussion attach <chat> repo <repo-id>
  tend discussion attach <chat> issue <issue-id-or-number>
  tend discussion attach <chat> pull <url>
  tend discussion attach <chat> worktree <issue-id> <task-id> <repo-id>
  tend discussion detach <chat> <reference-id>
  tend discussion draft <chat> [operation-id]
  tend discussion draft-read <chat> [draft-id]
  tend discussion handoff <chat> <approved-json> [operation-id]

Read or send discussion messages with tend chat read/send <chat>.
Approved JSON fields: draft_id, goal, decisions, constraints, authorization_end,
optional reference_ids and target_issue_id. Use handoff only after user approval.";

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
        ["list"] => super::checked(http.get(&root).set("Authorization", &auth).call())?,
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
                ("draft", [] | [_]) => {
                    let id = rest
                        .first()
                        .map_or_else(protocol::new_id, |id| (*id).to_owned());
                    ensure!(valid_id(&id), "invalid operation ID");
                    super::send(
                        &http,
                        &format!("{path}/draft"),
                        &auth,
                        &json!({"operation_id":id}),
                    )
                    .with_context(|| format!("discussion draft operation {id}"))?
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
