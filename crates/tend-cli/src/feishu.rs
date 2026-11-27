use anyhow::{Context, Result, bail, ensure};
use protocol::{DEFAULT_LISTEN, FEISHU_SUGGESTIONS_PATH, TOKEN_ENV, valid_id};
use serde_json::{Value, json};
use std::{env, time::Duration};

const USAGE: &str = "Usage:
  tend feishu-suggestions list [before]
  tend feishu-suggestions sync
  tend feishu-suggestions read <suggestion>
  tend feishu-suggestions ignore <suggestion> <revision>
  tend feishu-suggestions restore <suggestion> <revision>
  tend feishu-suggestions discuss <suggestion> <revision> [existing-chat]
  tend feishu-suggestions handoff <suggestion> <revision>
  tend feishu-suggestions handoff-status <suggestion> <revision>
  tend feishu-suggestions handoff-retry <suggestion> <revision> <failed-generation>

Discuss attaches a reference without posting a message.
Handoff shares context with Main Chat without adding implementation authorization.
Keep the suggestion ID and revision to replay or check the original transfer.";

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
    let root = format!("{}{FEISHU_SUGGESTIONS_PATH}", base.trim_end_matches('/'));
    let response = match args.as_slice() {
        ["list", rest @ ..] if rest.len() <= 1 => {
            let request = http.get(&root).set("Authorization", &auth);
            let request = if let Some(before) = rest.first() {
                request.query("before", before)
            } else {
                request
            };
            super::checked(request.call())?
        }
        ["sync"] => super::send(&http, &format!("{root}/sync"), &auth, &json!({}))?,
        ["read", id] if valid_id(id) => super::checked(
            http.get(&format!("{root}/{id}"))
                .set("Authorization", &auth)
                .call(),
        )?,
        [command, id, revision, rest @ ..] if valid_id(id) => {
            let version = positive(revision)?;
            let path = format!("{root}/{id}");
            (match (*command, rest) {
                ("ignore" | "restore", []) => super::send(
                    &http,
                    &format!("{path}/ignore"),
                    &auth,
                    &json!({"revision":version,"ignored":*command=="ignore"}),
                ),
                ("discuss", [] | [_]) if rest.first().is_none_or(|chat| valid_id(chat)) => {
                    super::send(
                        &http,
                        &format!("{path}/discussions"),
                        &auth,
                        &json!({"revision":version,"chat_id":rest.first()}),
                    )
                }
                ("handoff", []) => super::send(
                    &http,
                    &format!("{path}/handoff"),
                    &auth,
                    &json!({"revision":version}),
                ),
                ("handoff-status", []) => super::checked(
                    http.get(&format!("{path}/handoff"))
                        .set("Authorization", &auth)
                        .query("revision", revision)
                        .call(),
                ),
                ("handoff-retry", [generation]) => super::send(
                    &http,
                    &format!("{path}/handoff"),
                    &auth,
                    &json!({"revision":version,"retry_generation":positive(generation)?}),
                ),
                _ => bail!("{USAGE}"),
            })
            .with_context(|| format!("Feishu suggestion {id}, revision {version}"))?
        }
        _ => bail!("{USAGE}"),
    };
    let body: Value = response.into_json()?;
    println!("{}", serde_json::to_string(&body)?);
    Ok(())
}

fn positive(value: &str) -> Result<i64> {
    let value = value
        .parse()
        .context("expected a positive revision or generation")?;
    ensure!(value > 0, "expected a positive revision or generation");
    Ok(value)
}
