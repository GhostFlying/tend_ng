mod artifacts;
mod config;
mod execution;
mod inspect;
mod reconcile;
mod store;
mod tools;
mod workspace;

use anyhow::{Context, Result, bail, ensure};
use config::Config;
use protocol::{
    ClaimReceipt, DEFAULT_LISTEN, RUNNER_PROTOCOL, RunnerRecord, RunnerSync, TOKEN_ENV,
};
use std::{env, ffi::OsString, path::PathBuf, sync::Arc, thread, time::Duration};
use store::Store;

const USAGE: &str = "Usage: tend-runner --config <json-file> --db <sqlite-file> [--once | --reconcile <task-id>] [--app-server <program> <args>...]\n\nTEND_URL sets the Server address. TEND_TOKEN authenticates Runner sync.";

fn main() -> Result<()> {
    let mut args = env::args_os().skip(1);
    let (mut config_path, mut db_path, mut once) = (None, None, false);
    let mut task = None;
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
            Some("--reconcile") => {
                let id = args
                    .next()
                    .context("--reconcile needs a Task ID")?
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("invalid Task ID"))?;
                ensure!(protocol::valid_id(&id), "invalid Task ID");
                task = Some(id);
            }
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
    ensure!(!once || task.is_none(), "{USAGE}");
    let config = Arc::new(Config::load(&config_path.context(USAGE)?)?);
    let db_path = db_path.context(USAGE)?;
    let mut store = Store::open(&db_path, &config)?;
    let base = env::var("TEND_URL").unwrap_or_else(|_| format!("http://{DEFAULT_LISTEN}"));
    let token = env::var(TOKEN_ENV).context("set TEND_TOKEN before starting Runner")?;
    ensure!(!token.is_empty(), "TEND_TOKEN is empty");
    let auth = format!("Bearer {token}");
    let server = Arc::new(Server {
        base: base.trim_end_matches('/').to_owned(),
        auth: auth.clone(),
        http: ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(5))
            .build(),
    });
    let transfer = artifacts::Transfer {
        root: db_path.with_extension("artifacts"),
        base: base.trim_end_matches('/').to_owned(),
        runner: config.runner_id.clone(),
        auth: auth.clone(),
    };
    if once {
        return transfer.upload(sync(&config, &mut store, &server)?);
    }
    if let Some(task) = task {
        let path = protocol::reconcile_path(&config.runner_id, &task);
        reconcile::task(&config, &mut store, &server, &task, &path)?;
        loop {
            let files = server.exchange(&config, &mut store, &path, Vec::new(), Vec::new())?;
            if files.is_empty() {
                return Ok(());
            }
            transfer.upload(files)?;
        }
    }
    let uploads = transfer.start();
    let reconciliation = reconcile::start(
        Arc::clone(&config),
        Store::open(&db_path, &config)?,
        Arc::clone(&server),
    );
    let runtime = execution::Runtime {
        root: config
            .workspace_root
            .clone()
            .unwrap_or_else(|| db_path.with_extension("runs")),
        artifacts: db_path.with_extension("artifacts"),
        program,
        args: app_args,
    };
    if let Some(inspection) = &config.inspect {
        inspect::serve(
            inspection.listen,
            Store::open(&db_path, &config)?,
            runtime.clone(),
            config.runner_id.clone(),
            auth,
        )?;
    }
    if config.slot_limit > 0 {
        let local = Store::open(&db_path, &config)?;
        let execution_config = Arc::clone(&config);
        thread::spawn(move || execution::serve(local, db_path, execution_config, runtime));
    }

    let mut reconnecting = true;
    loop {
        let was_drained = store.drain_finished()?;
        match sync(&config, &mut store, &server) {
            Ok(files) => {
                if files.is_empty() && was_drained && store.drain_finished()? {
                    eprintln!("Runner {} drained and acknowledged", config.runner_id);
                    return Ok(());
                }
                if reconnecting && reconciliation.try_send(()).is_ok() {
                    reconnecting = false;
                }
                if !files.is_empty() {
                    let _ = uploads.try_send(files);
                }
            }
            Err(error) => {
                reconnecting = true;
                if matches!(
                    error.downcast_ref::<ureq::Error>(),
                    Some(ureq::Error::Status(409, _))
                ) {
                    let _ = reconciliation.try_send(());
                }
                eprintln!("Runner {} sync: {error:#}", config.runner_id);
            }
        };
        thread::sleep(Duration::from_secs(1));
    }
}

fn sync(config: &Config, store: &mut Store, server: &Server) -> Result<Vec<String>> {
    store.prepare_handoffs()?;
    server.exchange(
        config,
        store,
        &protocol::runner_sync_path(&config.runner_id),
        store.receipts()?,
        store.records()?,
    )
}

struct Server {
    base: String,
    auth: String,
    http: ureq::Agent,
}

impl Server {
    fn exchange(
        &self,
        config: &Config,
        store: &mut Store,
        path: &str,
        receipts: Vec<ClaimReceipt>,
        records: Vec<RunnerRecord>,
    ) -> Result<Vec<String>> {
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
            drained: store.drain_finished()?,
            profile,
            receipts,
            runs: Vec::new(),
            records,
        };
        let mut reply: protocol::RunnerSyncReply = self
            .http
            .post(&format!("{}{path}", self.base))
            .set("Authorization", &self.auth)
            .send_json(&request)
            .context("exchange Runner records")?
            .into_json()?;
        let uploads = std::mem::take(&mut reply.uploads);
        store.apply_sync(config, reply)?;
        Ok(uploads)
    }
}
