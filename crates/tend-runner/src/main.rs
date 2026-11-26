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
use std::{
    env,
    ffi::OsString,
    fs::OpenOptions,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};
use store::Store;

const USAGE: &str = "Usage: tend-runner --config <json-file> --db <sqlite-file> [--once | --reconcile <task-id>] [--ready-file <path>] [--app-server <program> <args>...]\n       tend-runner --default-workspace-root <sqlite-file>\n\nTEND_URL sets the Server address. TEND_TOKEN authenticates Runner sync.";

fn default_workspace_root(database: &Path) -> PathBuf {
    database.with_extension("runs")
}

pub(crate) fn host_data_root(workspace: &Path, host_id: &str) -> PathBuf {
    workspace.join(".tend-host").join(host_id)
}

fn canonical_database(path: &Path) -> Result<PathBuf> {
    let path = match path.canonicalize() {
        Ok(path) => path,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            // A dangling link cannot safely reserve the target's lock name.
            ensure!(
                !path.symlink_metadata().is_ok_and(|meta| meta.is_symlink()),
                "Runner database is a dangling symbolic link"
            );
            let absolute = std::path::absolute(path)?;
            absolute
                .parent()
                .context("Runner database needs a parent directory")?
                .canonicalize()?
                .join(
                    path.file_name()
                        .context("Runner database needs a file name")?,
                )
        }
        Err(error) => return Err(error.into()),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            !path.metadata().is_ok_and(|meta| meta.nlink() > 1),
            "Runner database must not have hard links"
        );
    }
    Ok(path)
}

fn main() -> Result<()> {
    let mut args = env::args_os().skip(1).peekable();
    if args
        .next_if(|arg| arg == "--default-workspace-root")
        .is_some()
    {
        let database = PathBuf::from(args.next().context(USAGE)?);
        ensure!(args.next().is_none(), "{USAGE}");
        let root = std::path::absolute(default_workspace_root(&database))?;
        println!("{}", serde_json::to_string(&root)?);
        return Ok(());
    }
    let (mut config_path, mut db_path, mut once) = (None, None, false);
    let mut task = None;
    let mut ready_file = None;
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
            Some("--ready-file") => {
                ready_file = Some(PathBuf::from(
                    args.next().context("--ready-file needs a path")?,
                ))
            }
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
    ensure!(ready_file.is_none() || (!once && task.is_none()), "{USAGE}");
    let config = Arc::new(Config::load(&config_path.context(USAGE)?)?);
    let db_path = db_path.context(USAGE)?;
    let database = canonical_database(&db_path).context("resolve Runner database")?;
    // Forward recovery and upgrade retries can race to launch the same
    // generation. Only one execution scheduler may own a local ledger.
    let _instance_lock = if task.is_none() {
        let mut lock_path = database.as_os_str().to_owned();
        lock_path.push(".lock");
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)?;
        file.try_lock()
            .context("another Runner is already using this database")?;
        Some(file)
    } else {
        None
    };
    let mut store = Store::open(&database, &config)?;
    let base = env::var("TEND_URL").unwrap_or_else(|_| format!("http://{DEFAULT_LISTEN}"));
    let token = env::var(TOKEN_ENV).context("set TEND_TOKEN before starting Runner")?;
    ensure!(!token.is_empty(), "TEND_TOKEN is empty");
    let auth = format!("Bearer {token}");
    let server = Arc::new(Server {
        proposal_approval: Arc::new(AtomicBool::new(false)),
        base: base.trim_end_matches('/').to_owned(),
        auth: auth.clone(),
        http: ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(5))
            .build(),
    });
    let workspace_root = config
        .workspace_root
        .clone()
        .unwrap_or_else(|| default_workspace_root(&db_path));
    let transfer = artifacts::Transfer {
        root: host_data_root(&workspace_root, &config.host_id).join("artifacts"),
        base: base.trim_end_matches('/').to_owned(),
        runner: config.runner_id.clone(),
        auth: auth.clone(),
    };
    if once {
        return transfer.upload(sync(&config, &mut store, &server)?.0);
    }
    if let Some(task) = task {
        let path = protocol::reconcile_path(&config.runner_id, &task);
        reconcile::task(&config, &mut store, &server, &task, &path)?;
        loop {
            let (files, _) = server.exchange(&config, &mut store, &path, Vec::new(), Vec::new())?;
            if files.is_empty() {
                return Ok(());
            }
            transfer.upload(files)?;
        }
    }
    let uploads = transfer.clone().start();
    let reconciliation = reconcile::start(
        Arc::clone(&config),
        Store::open(&database, &config)?,
        Arc::clone(&server),
    );
    let runtime = execution::Runtime {
        proposal_approval: Arc::clone(&server.proposal_approval),
        root: workspace_root,
        artifacts: transfer,
        program,
        args: app_args,
    };
    if let Some(inspection) = &config.inspect {
        inspect::serve(
            inspection.listen,
            Store::open(&database, &config)?,
            runtime.clone(),
            Arc::clone(&config),
            auth,
        )?;
    }
    workspace::serve_reclaims(
        Store::open(&database, &config)?,
        Arc::clone(&config),
        runtime.root.clone(),
    );
    if config.slot_limit > 0 {
        let local = Store::open(&database, &config)?;
        let execution_config = Arc::clone(&config);
        thread::spawn(move || execution::serve(local, database, execution_config, runtime));
    }

    let mut reconnecting = true;
    loop {
        let was_drained = store.drain_finished()?;
        match sync(&config, &mut store, &server) {
            Ok((files, drain_acknowledged)) => {
                if drain_acknowledged
                    && files.is_empty()
                    && was_drained
                    && store.drain_finished()?
                {
                    eprintln!("Runner {} drained and acknowledged", config.runner_id);
                    return Ok(());
                }
                if let Some(path) = &ready_file {
                    match std::fs::write(path, format!("{}\n", std::process::id())) {
                        Ok(()) => ready_file = None,
                        Err(error) => eprintln!("Runner readiness file: {error}"),
                    }
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

fn sync(config: &Config, store: &mut Store, server: &Server) -> Result<(Vec<String>, bool)> {
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
    proposal_approval: Arc<AtomicBool>,
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
    ) -> Result<(Vec<String>, bool)> {
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
            repair_host_artifacts: true,
            drained: store.drain_finished()?,
            drain_active_slots: store.drain_active_slots()?,
            reclaim_receipts: store.reclaim_receipts()?,
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
        self.proposal_approval
            .store(reply.proposal_approval, Ordering::Relaxed);
        let uploads = std::mem::take(&mut reply.uploads);
        let drain_acknowledged = reply.drain_acknowledged.unwrap_or(true);
        // Full reconciliation has its own connection and can finish after a
        // newer heartbeat. Only the ordered incremental stream installs a cap.
        if path != protocol::runner_sync_path(&config.runner_id) {
            reply.execution_slot_limit = None;
        }
        store.apply_sync(config, reply)?;
        Ok((uploads, drain_acknowledged))
    }
}
