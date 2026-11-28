use anyhow::{Context, Result, bail, ensure};
use protocol::{DEFAULT_LISTEN, MAINTENANCE_SUGGESTIONS_PATH, TOKEN_ENV, valid_id};
use serde_json::{Value, json};
use std::{env, time::Duration};

const USAGE: &str = "Usage:
  tend maintenance-suggestions list [active|history|ignored] [before]
  tend maintenance-suggestions read <suggestion>
  tend maintenance-suggestions ignore <suggestion> <revision>
  tend maintenance-suggestions restore <suggestion> <revision>
  tend maintenance-suggestions discuss <suggestion> <revision> [existing-chat]
  tend maintenance-suggestions handoff <suggestion> <revision>

Discuss and handoff share a snapshot without arranging work or triggering an Agent.";

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
    let root = format!(
        "{}{MAINTENANCE_SUGGESTIONS_PATH}",
        base.trim_end_matches('/')
    );
    let get = |path: &str| http.get(path).set("Authorization", &auth);
    let response = match args.as_slice() {
        ["list", rest @ ..] if rest.len() <= 2 => {
            let mut request = get(&root);
            if let Some(view) = rest.first() {
                request = request.query("view", view);
            }
            if let Some(before) = rest.get(1) {
                request = request.query("before", before);
            }
            super::checked(request.call())?
        }
        ["read", id] if valid_id(id) => super::checked(get(&format!("{root}/{id}")).call())?,
        [command, id, revision, rest @ ..] if valid_id(id) => {
            let version: i64 = revision.parse().context("expected a positive revision")?;
            ensure!(version > 0, "expected a positive revision");
            let (action, body) = match (*command, rest) {
                ("ignore" | "restore", []) => (
                    "ignore",
                    json!({"revision":version,"ignored":*command=="ignore"}),
                ),
                ("discuss", [] | [_]) if rest.first().is_none_or(|chat| valid_id(chat)) => (
                    "discussions",
                    json!({"revision":version,"chat_id":rest.first()}),
                ),
                ("handoff", []) => ("handoff", json!({"revision":version})),
                _ => bail!("{USAGE}"),
            };
            super::send(&http, &format!("{root}/{id}/{action}"), &auth, &body)?
        }
        _ => bail!("{USAGE}"),
    };
    let body: Value = response.into_json()?;
    println!("{}", serde_json::to_string(&body)?);
    Ok(())
}
