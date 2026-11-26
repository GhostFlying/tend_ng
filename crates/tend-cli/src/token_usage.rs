use anyhow::{Context, Result, bail, ensure};
use protocol::{
    DEFAULT_LISTEN, StartTokenBackfill, TOKEN_BACKFILL_PATH, TOKEN_ENV, TOKEN_OVERVIEW_PATH,
    TOKEN_REBUILD_PATH, TokenConflictPolicy, TokenOverviewCursor, TokenUsageScope,
};
use std::{env, io, time::Duration};

const HELP: &str = "Usage:
  tend token-usage overview [--parent ID|#NUMBER] [--before CURSOR]
  tend token-usage backfill <scope> [--policy quarantine|prefer-native|prefer-server] [--job ID]
  tend token-usage status JOB [--after CURSOR]
  tend token-usage audit JOB [--after CURSOR]
  tend token-usage rebuild <scope>

Scope (choose one): --all | --run ID | --task ID | --issue ID|#NUMBER | --runner ID | --host ID
Overview lists top-level Issues and global usage. --parent lists immediate children instead.
Use next_before from the response to read another page of the same level.
The Issue scope includes its descendants. Runner and Host refer to original execution.
Backfill runs asynchronously and defaults to quarantining conflicting values.
TEND_URL sets the Server address. TEND_TOKEN authenticates the request.";

pub fn run(args: &[String]) -> Result<()> {
    if args.is_empty() || args == ["--help"] {
        println!("{HELP}");
        return Ok(());
    }
    let [command, rest @ ..] = args else {
        unreachable!()
    };
    let base = env::var("TEND_URL").unwrap_or_else(|_| format!("http://{DEFAULT_LISTEN}"));
    let base = base.trim_end_matches('/');
    let token = env::var(TOKEN_ENV).context("set TEND_TOKEN before calling Tend")?;
    ensure!(!token.is_empty(), "TEND_TOKEN is empty");
    let auth = format!("Bearer {token}");
    let timeout = if command == "rebuild" { 600 } else { 30 };
    let http = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(timeout))
        .build();
    let response = match command.as_str() {
        "overview" => {
            let mut request = http
                .get(&format!("{base}{TOKEN_OVERVIEW_PATH}"))
                .set("Authorization", &auth);
            let (mut parent, mut before) = (false, false);
            let mut words = rest.iter();
            while let Some(flag) = words.next() {
                let value = words
                    .next()
                    .with_context(|| format!("{flag} needs a value"))?;
                match flag.as_str() {
                    "--parent" if !parent => {
                        ensure!(
                            protocol::valid_id(value)
                                || protocol::parse_issue_number(value).is_some(),
                            "invalid parent Issue reference"
                        );
                        request = request.query("parent", value);
                        parent = true;
                    }
                    "--before" if !before => {
                        ensure!(
                            TokenOverviewCursor::decode(value).is_some(),
                            "invalid Token usage cursor"
                        );
                        request = request.query("before", value);
                        before = true;
                    }
                    _ => bail!("{HELP}"),
                }
            }
            super::checked(request.call())?
        }
        "backfill" | "rebuild" => {
            let input = submission(rest, command == "backfill")?;
            if command == "backfill" {
                super::send(
                    &http,
                    &format!("{base}{TOKEN_BACKFILL_PATH}"),
                    &auth,
                    &serde_json::to_value(&input)?,
                )
                .with_context(|| format!("submit backfill job {}", input.job_id))?
            } else {
                super::send(
                    &http,
                    &format!("{base}{TOKEN_REBUILD_PATH}"),
                    &auth,
                    &serde_json::to_value(&input.scope)?,
                )?
            }
        }
        "status" | "audit" => {
            let (id, after) = match rest {
                [id] => (id, None),
                [id, flag, after] if flag == "--after" => (id, Some(after)),
                _ => bail!("{HELP}"),
            };
            ensure!(protocol::valid_id(id), "invalid backfill job ID");
            let suffix = if command == "audit" { "/audit" } else { "" };
            let mut request = http
                .get(&format!("{base}{TOKEN_BACKFILL_PATH}/{id}{suffix}"))
                .set("Authorization", &auth);
            if let Some(after) = after {
                let cursor = after.parse::<i64>().context("invalid after cursor")?;
                ensure!(cursor >= 0, "invalid after cursor");
                request = request.query("after", &cursor.to_string());
            }
            super::checked(request.call())?
        }
        _ => bail!("{HELP}"),
    };
    io::copy(&mut response.into_reader(), &mut io::stdout().lock())?;
    Ok(())
}

fn submission(args: &[String], backfill: bool) -> Result<StartTokenBackfill> {
    let (mut scope, mut job_id, mut policy) = (None, None, None);
    let mut words = args.iter();
    while let Some(flag) = words.next() {
        let selected = match flag.as_str() {
            "--all" => TokenUsageScope::All,
            "--run" | "--task" | "--issue" | "--runner" | "--host" => {
                let id = words.next().context("scope needs an ID")?.clone();
                match flag.as_str() {
                    "--run" => TokenUsageScope::Run(id),
                    "--task" => TokenUsageScope::Task(id),
                    "--issue" => TokenUsageScope::Issue(id),
                    "--runner" => TokenUsageScope::Runner(id),
                    _ => TokenUsageScope::Host(id),
                }
            }
            "--policy" if backfill && policy.is_none() => {
                policy = match words.next().map(String::as_str) {
                    Some("quarantine") => Some(TokenConflictPolicy::Quarantine),
                    Some("prefer-native") => Some(TokenConflictPolicy::PreferNative),
                    Some("prefer-server") => Some(TokenConflictPolicy::PreferServer),
                    _ => bail!("invalid backfill conflict policy"),
                };
                continue;
            }
            "--job" if backfill && job_id.is_none() => {
                let id = words.next().context("--job needs an ID")?;
                ensure!(protocol::valid_id(id), "invalid backfill job ID");
                job_id = Some(id.clone());
                continue;
            }
            _ => bail!("{HELP}"),
        };
        ensure!(
            scope.is_none() && selected.valid(),
            "choose exactly one valid scope"
        );
        scope = Some(selected);
    }
    Ok(StartTokenBackfill {
        job_id: job_id.unwrap_or_else(protocol::new_id),
        scope: scope.context("choose exactly one scope (use --all to select every Run)")?,
        policy: policy.unwrap_or_default(),
    })
}
