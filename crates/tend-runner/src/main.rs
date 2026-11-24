mod config;
mod execution;
mod store;

use anyhow::{Context, Result, bail, ensure};
use config::Config;
use protocol::{DEFAULT_LISTEN, RUNNER_PROTOCOL, RunnerSync, TOKEN_ENV};
use std::{env, ffi::OsString, path::PathBuf, thread, time::Duration};
use store::Store;

const USAGE: &str = "Usage: tend-runner --config <json-file> --db <sqlite-file> [--once] [--app-server <program> <args>...]\n\nTEND_URL sets the Server address. TEND_TOKEN authenticates Runner sync.";

fn main() -> Result<()> {
    let mut args = env::args_os().skip(1);
    let (mut config_path, mut db_path, mut once) = (None, None, false);
    let mut program = OsString::from("codex");
    let mut app_args = ["app-server", "--listen", "stdio://"]
        .map(OsString::from)
        .to_vec();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--config") => {
                config_path = Some(PathBuf::from(args.next().context("--config needs a path")?))
            }
            Some("--db") => {
                db_path = Some(PathBuf::from(args.next().context("--db needs a path")?))
            }
            Some("--once") => once = true,
            Some("--app-server") => {
                program = args.next().context("--app-server needs a program")?;
                app_args = args.collect();
                break;
            }
            Some("--help") => {
                println!("{USAGE}");
                return Ok(());
            }
            _ => bail!("{USAGE}"),
        }
    }
    let config = Config::load(&config_path.context(USAGE)?)?;
    let db_path = db_path.context(USAGE)?;
    let mut store = Store::open(&db_path, &config)?;
    let base = env::var("TEND_URL").unwrap_or_else(|_| format!("http://{DEFAULT_LISTEN}"));
    let url = format!(
        "{}{}",
        base.trim_end_matches('/'),
        protocol::runner_sync_path(&config.runner_id)
    );
    let token = env::var(TOKEN_ENV).context("set TEND_TOKEN before starting Runner")?;
    ensure!(!token.is_empty(), "TEND_TOKEN is empty");
    let auth = format!("Bearer {token}");
    let http = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(5))
        .build();
    if !once && config.slot_limit > 0 {
        let local = Store::open(&db_path, &config)?;
        let runtime = execution::Runtime {
            root: db_path.with_extension("runs"),
            program,
            args: app_args,
        };
        thread::spawn(move || execution::serve(local, runtime));
    }

    loop {
        let result = sync(&config, &mut store, &http, &url, &auth);
        if once {
            return result;
        }
        if let Err(error) = result {
            eprintln!("Runner {} sync: {error:#}", config.runner_id);
        }
        thread::sleep(Duration::from_secs(1));
    }
}

fn sync(
    config: &Config,
    store: &mut Store,
    http: &ureq::Agent,
    url: &str,
    auth: &str,
) -> Result<()> {
    let running = store.running_slots()?;
    let mut profile = config.profile(store.unfinished()?);
    profile.running_slots = running;
    // Completion can occur between the two reads. Keep the advertised
    // capacity valid for the running count in this snapshot.
    profile.available_for_assignment = profile
        .available_for_assignment
        .min(profile.slot_limit.saturating_sub(running));
    let request = RunnerSync {
        protocol: RUNNER_PROTOCOL,
        profile,
        receipts: store.receipts()?,
        runs: store.run_events()?,
    };
    let reply = http
        .post(url)
        .set("Authorization", auth)
        .send_json(&request)
        .context("exchange Runner records")?
        .into_json()?;
    store.apply_sync(config, reply)?;
    Ok(())
}
