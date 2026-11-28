mod artifacts;
mod backfill;
mod config;
mod execution;
mod inspect;
mod reconcile;
mod store;
mod tools;
use codex_app::usage;
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
        stage_attention: Arc::new(AtomicBool::new(false)),
        legacy_token_server: AtomicBool::new(false),
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
        return transfer.upload(sync(&config, &mut store, &server, None)?.0);
    }
    if let Some(task) = task {
        reconcile::task(&config, &mut store, &server, &task)?;
        loop {
            let files = server
                .exchange(
                    &config,
                    &mut store,
                    SyncScope::Task(&task),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                )?
                .uploads;
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
        stage_attention: Arc::clone(&server.stage_attention),
        root: workspace_root,
        artifacts: transfer,
        program,
        args: app_args,
        codex_home: env::var_os("CODEX_HOME").map(PathBuf::from),
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
    let mut backfill = backfill::Worker::start(
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
        match sync(&config, &mut store, &server, Some(&mut backfill)) {
            Ok((files, drain_acknowledged)) => {
                if drain_acknowledged
                    && files.is_empty()
                    && store.drain_finished(!server.legacy_token_server.load(Ordering::Relaxed))?
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
        backfill.wait(Duration::from_secs(1));
    }
}

fn sync(
    config: &Config,
    store: &mut Store,
    server: &Server,
    backfill: Option<&mut backfill::Worker>,
) -> Result<(Vec<String>, bool)> {
    store.prepare_handoffs()?;
    let exchanged = server.exchange_inner(
        config,
        store,
        SyncScope::Runner,
        store.receipts()?,
        store.records()?,
        store.token_usage()?,
        backfill,
    )?;
    Ok((exchanged.uploads, exchanged.drain_acknowledged))
}

struct Server {
    proposal_approval: Arc<AtomicBool>,
    stage_attention: Arc<AtomicBool>,
    legacy_token_server: AtomicBool,
    base: String,
    auth: String,
    http: ureq::Agent,
}

#[derive(Clone, Copy)]
enum SyncScope<'a> {
    Runner,
    Task(&'a str),
}

struct SyncExchange {
    uploads: Vec<String>,
    drain_acknowledged: bool,
    sent_records: usize,
}

impl Server {
    fn exchange(
        &self,
        config: &Config,
        store: &mut Store,
        scope: SyncScope<'_>,
        receipts: Vec<ClaimReceipt>,
        records: Vec<RunnerRecord>,
        token_usage: Vec<protocol::RunTokenUsage>,
    ) -> Result<SyncExchange> {
        self.exchange_inner(config, store, scope, receipts, records, token_usage, None)
    }

    #[allow(clippy::too_many_arguments)]
    fn exchange_inner(
        &self,
        config: &Config,
        store: &mut Store,
        scope: SyncScope<'_>,
        receipts: Vec<ClaimReceipt>,
        records: Vec<RunnerRecord>,
        token_usage: Vec<protocol::RunTokenUsage>,
        backfill: Option<&mut backfill::Worker>,
    ) -> Result<SyncExchange> {
        let running = store.running_slots()?;
        let mut profile = config.profile(store.unfinished()?);
        profile.running_slots = running;
        // Completion can occur between the two reads. Keep the advertised
        // capacity valid for the running count in this snapshot.
        profile.available_for_assignment = profile
            .available_for_assignment
            .min(profile.slot_limit.saturating_sub(running));
        let mut request = RunnerSync {
            protocol: RUNNER_PROTOCOL,
            repair_host_artifacts: true,
            drained: matches!(scope, SyncScope::Runner)
                && store.drain_finished(!self.legacy_token_server.load(Ordering::Relaxed))?,
            drain_active_slots: store.drain_active_slots()?,
            reclaim_receipts: store.reclaim_receipts()?,
            profile,
            receipts,
            runs: Vec::new(),
            records,
            token_usage,
            token_backfill: backfill.as_ref().map(|worker| protocol::TokenBackfillSync {
                page: worker.page.clone(),
            }),
        };
        let bytes = encode_sync(&mut request)?;
        let path = match scope {
            SyncScope::Runner => protocol::runner_sync_path(&config.runner_id),
            SyncScope::Task(id) => protocol::reconcile_path(&config.runner_id, id),
        };
        let mut reply: protocol::RunnerSyncReply = self
            .http
            .post(&format!("{}{path}", self.base))
            .set("Authorization", &self.auth)
            .set("Content-Type", "application/json")
            .send_bytes(&bytes)
            .context("exchange Runner records")?
            .into_json()?;
        self.proposal_approval
            .store(reply.proposal_approval, Ordering::Relaxed);
        let drain_acknowledged = request.drained
            && reply
                .drain_acknowledged
                .unwrap_or(!reply.token_usage_supported);
        let uploads = std::mem::take(&mut reply.uploads);
        // Full reconciliation has its own connection and can finish after a
        // newer heartbeat. Only the ordered incremental stream installs a cap.
        if !matches!(scope, SyncScope::Runner) {
            reply.execution_slot_limit = None;
        }
        let legacy = !reply.token_usage_supported;
        let supports_stage_attention = reply.stage_attention;
        let work = reply.token_backfill.take();
        store.apply_sync(config, reply, matches!(scope, SyncScope::Runner))?;
        if let Some(worker) = backfill {
            worker.accept(work);
        }
        if matches!(scope, SyncScope::Runner) {
            self.stage_attention
                .store(supports_stage_attention, Ordering::Relaxed);
            self.legacy_token_server.store(legacy, Ordering::Relaxed);
        }
        Ok(SyncExchange {
            uploads,
            drain_acknowledged,
            sent_records: request.records.len(),
        })
    }
}

fn encode_sync(request: &mut RunnerSync) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(request)?;
    if bytes.len() > protocol::MAX_REQUEST_BYTES
        && let Some(feature) = &mut request.token_backfill
        && feature.page.take().is_some()
    {
        bytes = serde_json::to_vec(request)?;
    }
    if bytes.len() <= protocol::MAX_REQUEST_BYTES || request.records.is_empty() {
        return Ok(bytes);
    }

    // The existing count page may still exceed the transport byte limit.
    // Remove only a suffix. Keep at least the first record so a single
    // unsendable record is never silently skipped by full reconciliation.
    let mut size = bytes.len();
    while size > protocol::MAX_REQUEST_BYTES && request.records.len() > 1 {
        let tail = request.records.pop().expect("nonempty records");
        size -= serde_json::to_vec(&tail)?.len() + 1; // record and comma
    }
    if size > protocol::MAX_REQUEST_BYTES {
        // Ordinary usage has its own ACK and cannot starve the first record.
        // Full record pages already send without usage.
        request.token_usage.clear();
    }
    Ok(serde_json::to_vec(request)?)
}
