use crate::{
    config::{AppServer, Config},
    store::{Store, run::LocalRun},
};
use crate::{tools, workspace};
use anyhow::{Context, Result};
use codex_app::{
    Client, ToolCall, ToolOutput, Turn, TurnHandler, TurnInput, TurnStatus, WorkThreadResume,
};
use protocol::{ControlAction, ControlState, NativeExecutionStamp, RunOutcome, TOKEN_ENV};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    ffi::OsString,
    hash::{DefaultHasher, Hash, Hasher},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Clone)]
pub struct Runtime {
    pub proposal_approval: Arc<AtomicBool>,
    pub stage_attention: Arc<AtomicBool>,
    pub root: PathBuf,
    pub artifacts: crate::artifacts::Transfer,
    pub program: OsString,
    pub args: Vec<OsString>,
    pub codex_home: Option<PathBuf>,
}

pub fn serve(mut store: Store, db: PathBuf, config: Arc<Config>, runtime: Runtime) {
    let mut workers: HashMap<String, JoinHandle<()>> = HashMap::new();
    loop {
        workers.retain(|_, worker| !worker.is_finished());
        let result = (|| {
            let active = store.active_runs()?;
            let mut candidates = Vec::new();
            for id in &active {
                if !workers.contains_key(id) {
                    let run = store.load_run(id)?;
                    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
                    if run.resume_retry_at <= now
                        && run
                            .process
                            .as_ref()
                            .map(|process| process.is_stopped())
                            .transpose()?
                            .unwrap_or(true)
                    {
                        candidates.push(run);
                    }
                }
            }
            for _ in active.len()..config.slot_limit as usize {
                let Some(run) = store.next_queued_run(config.slot_limit)? else {
                    break;
                };
                candidates.push(run);
            }
            for run in candidates {
                let local = Store::open(&db, &config)?;
                let runtime = runtime.clone();
                let config = config.clone();
                workers.insert(
                    run.id.clone(),
                    thread::spawn(move || {
                        if let Err(error) = advance(local, &runtime, &run, &config) {
                            eprintln!("Runner execution {}: {error:#}", run.id);
                        }
                    }),
                );
            }
            Ok::<_, anyhow::Error>(())
        })();
        if let Err(error) = result {
            eprintln!("Runner execution scheduler: {error:#}");
        }
        thread::sleep(Duration::from_millis(200));
    }
}

fn advance(mut store: Store, runtime: &Runtime, run: &LocalRun, config: &Config) -> Result<()> {
    let cwd = workspace::directory(&runtime.root, run);
    if run.process.is_none()
        && run.resume_guard.is_none()
        && store.has_stop(&run.assignment.placement.assignment_id)?
    {
        return settle(
            &mut store,
            run,
            &cwd,
            Err(anyhow::anyhow!("Task cancelled before launch")),
            false,
        );
    }
    if let Some(process) = &run.process
        && run.thread_id.is_none()
    {
        // The scheduler confirmed the exact allocation process has exited.
        // This phase could never submit a turn, including before a restart.
        return store.release_empty_thread(&run.id, process);
    }
    if run.process.is_some() {
        let result = if run.assignment.review.is_some() {
            workspace::existing_author_directory(&runtime.root, run)
                .and_then(|path| recover(runtime, run, &path))
        } else {
            recover(runtime, run, &cwd)
        };
        if let Err(error) = &result
            && run.native_execution.is_some()
            && !store.has_stop(&run.assignment.placement.assignment_id)?
        {
            eprintln!("Native turn for Run {} still unresolved: {error:#}", run.id);
            return store.retry_native_turn(&run.id);
        }
        settle(&mut store, run, &cwd, result, true)?;
    } else {
        launch(&mut store, runtime, run, config)?;
    }
    Ok(())
}

pub(crate) fn client(
    runtime: &Runtime,
    cwd: &Path,
    app_server: Option<&AppServer>,
) -> Result<Client> {
    let mut command = match app_server {
        Some(app) => {
            let mut command = Command::new(&app.program);
            command.args(&app.args);
            command
        }
        None => {
            let mut command = Command::new(&runtime.program);
            command.args(&runtime.args);
            command
        }
    };
    let path = configured_home(
        runtime,
        app_server.and_then(|app| app.codex_home.as_deref()),
    )?;
    let home = codex_app::validate_home(path).context("open original Runner Codex home")?;
    command
        .current_dir(cwd)
        .env("CODEX_HOME", home)
        .env_remove(TOKEN_ENV);
    Client::connect(&mut command)
}

fn configured_home<'a>(runtime: &'a Runtime, configured: Option<&'a Path>) -> Result<&'a Path> {
    configured
        .or(runtime.codex_home.as_deref())
        .context("set Runner CODEX_HOME or configure a dedicated app_servers.codex_home")
}

fn recover(runtime: &Runtime, run: &LocalRun, cwd: &Path) -> Result<Turn> {
    if let Some(stamp) = &run.native_execution {
        let home = configured_home(
            runtime,
            run.app_server
                .as_ref()
                .and_then(|app| app.codex_home.as_deref()),
        )?;
        anyhow::ensure!(
            workspace::stamp(&cwd.canonicalize()?)? == stamp.workspace
                && workspace::stamp(&codex_app::validate_home(home)?)? == stamp.codex_home,
            "original native Run workspace or home is not available"
        );
    }
    let mut app = client(runtime, cwd, run.app_server.as_ref())?;
    let thread = run
        .thread_id
        .as_deref()
        .context("missing original native thread")?;
    let turn = app
        .find_turn(thread, run.turn_id.as_deref(), &run.id)?
        .filter(|turn| turn.status != TurnStatus::InProgress)
        .context("App Server stopped without a recoverable terminal turn")?;
    app.stop()?;
    Ok(turn)
}

fn frozen_launch(
    runtime: &Runtime,
    configured: Option<&AppServer>,
    cwd: &Path,
) -> Result<AppServer> {
    let mut launch = match configured {
        Some(app) => app.clone(),
        None => AppServer {
            program: runtime
                .program
                .to_str()
                .context("App Server program is not UTF-8")?
                .to_owned(),
            args: runtime
                .args
                .iter()
                .map(|arg| {
                    arg.to_str()
                        .map(str::to_owned)
                        .context("App Server argument is not UTF-8")
                })
                .collect::<Result<_>>()?,
            codex_home: None,
        },
    };
    let program = Path::new(&launch.program);
    let executable = if program.components().count() > 1 {
        Some(cwd.join(program))
    } else {
        use std::os::unix::fs::PermissionsExt;
        std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| cwd.join(dir).join(program))
                .find(|file| {
                    file.metadata()
                        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
                })
        })
    }
    .context("App Server executable was not found")?;
    launch.program = executable
        .to_str()
        .context("App Server path is not UTF-8")?
        .to_owned();
    launch.codex_home = Some(codex_app::prepare_home(configured_home(
        runtime,
        launch.codex_home.as_deref(),
    )?)?);
    Ok(launch)
}

fn native_context(
    runtime: &Runtime,
    run: &LocalRun,
    agent: &protocol::AgentProfile,
    app_server: Option<&AppServer>,
    cwd: &Path,
    instructions: &str,
    tools: &[serde_json::Value],
) -> Result<String> {
    // Preserve the existing resume key. The separate historical launch profile
    // captures the effective program and home used by this Run.
    let (program, args, home) = match app_server {
        Some(app) => (
            OsString::from(&app.program),
            app.args.iter().map(OsString::from).collect::<Vec<_>>(),
            app.codex_home.clone(),
        ),
        None => (runtime.program.clone(), runtime.args.clone(), None),
    };
    let executable = if Path::new(&program).components().count() > 1 {
        cwd.join(&program).canonicalize().ok()
    } else {
        std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .find_map(|dir| cwd.join(dir).join(&program).canonicalize().ok())
        })
    };
    let binary = executable
        .as_ref()
        .and_then(|path| std::fs::metadata(path).ok())
        .map(|metadata| (metadata.len(), metadata.modified().ok()));
    let home = home.or_else(|| runtime.codex_home.clone());
    let home_config = home
        .as_ref()
        .and_then(|home| std::fs::metadata(home.join("config.toml")).ok())
        .map(|metadata| (metadata.len(), metadata.modified().ok()));
    let configuration = serde_json::to_string(&json!({
        "runner": run.assignment.placement.runner_id,
        "binding": run.assignment.workspace_key.as_ref().or(run.assignment.parent_issue_id.as_ref()),
        "cwd": cwd,
        "agent": agent,
        "program": program,
        "executable": executable,
        "binary": binary,
        "args": args,
        "home": home,
        "home_config": home_config,
        "instructions": instructions,
        "tools": tools,
    }))?;
    let mut hash = DefaultHasher::new();
    configuration.hash(&mut hash);
    Ok(format!("{:016x}", hash.finish()))
}

fn execution_stamp(
    run: &LocalRun,
    agent: &protocol::AgentProfile,
    launch: &AppServer,
    cwd: &Path,
    instructions: &str,
    tools: &[serde_json::Value],
) -> Result<NativeExecutionStamp> {
    let home = launch
        .codex_home
        .as_deref()
        .context("missing effective Codex home")?;
    let executable = Path::new(&launch.program).canonicalize()?;
    let binary = executable.metadata()?;
    let home_config = match std::fs::read(home.join("config.toml")) {
        Ok(bytes) => Some(format!("{:x}", Sha256::digest(bytes))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).context("read effective Codex home configuration"),
    };
    let compatibility = serde_json::to_vec(&json!({
        "agent": agent,
        "agent_role": run.assignment.role(),
        "workspace": run.assignment.workspace_path(),
        "repositories": run.repos,
        "program": launch.program,
        "executable": executable,
        "binary": (binary.dev(), binary.ino(), binary.len(), binary.modified()?),
        "args": launch.args,
        "home_config": home_config,
        "instructions": instructions,
        "tools": tools,
    }))?;
    Ok(NativeExecutionStamp {
        host_id: run.assignment.placement.host_id.clone(),
        workspace: workspace::stamp(cwd)?,
        codex_home: workspace::stamp(home)?,
        compatibility: format!("v1:{:x}", Sha256::digest(compatibility)),
    })
}

fn incompatible_handoff(
    origin: &NativeExecutionStamp,
    current: &NativeExecutionStamp,
) -> Option<&'static str> {
    if origin.host_id != current.host_id {
        Some("host changed")
    } else if origin.workspace != current.workspace {
        Some("workspace identity changed")
    } else if origin.codex_home != current.codex_home {
        Some("isolated Codex home identity changed")
    } else if origin.compatibility != current.compatibility {
        Some("named Agent or effective execution context changed")
    } else {
        None
    }
}

fn log_handoff_fallback(run: &LocalRun, original_run: &str, reason: &str) {
    eprintln!(
        "Native thread handoff fallback for Task {}, Run {} (original Run {original_run}): {reason}",
        run.assignment.task_id, run.id,
    );
}

fn launch(store: &mut Store, runtime: &Runtime, run: &LocalRun, config: &Config) -> Result<()> {
    let planned_cwd = workspace::directory(&runtime.root, run);
    let is_review = run.assignment.review.is_some();
    let handoff = store.incoming_thread(run)?;
    let incoming = handoff.is_some();
    let started = (|| {
        let agent = &run.assignment.placement.agent;
        let app_server = run.app_server.as_ref();
        anyhow::ensure!(run.assignment.role().is_some(), "unknown frozen Agent role");
        anyhow::ensure!(
            agent.app_server.is_none() || app_server.is_some(),
            "missing frozen Agent launcher"
        );
        let directory = if is_review {
            workspace::verify_review(&runtime.root, run, &config.repos)
        } else {
            workspace::prepare(&runtime.root, run)
        }
        .context("prepare Tend workspace")?;
        Ok((agent, app_server, directory))
    })();
    let started = started.and_then(|(agent, app_server, directory)| {
        let artifact = run
            .assignment
            .review
            .as_ref()
            .and_then(|review| review.candidate.strip_prefix("artifact:"))
            .map(|id| runtime.artifacts.review_file(id))
            .transpose()?;
        Ok((directory, agent, app_server, artifact))
    });
    let (cwd, agent, app_server, artifact) = match started {
        Ok((_, _, _, Some(None))) => return store.retry_resume(&run.id),
        Ok((cwd, agent, app_server, artifact)) => (cwd, agent, app_server, artifact.flatten()),
        Err(error) => {
            if let Some(thread) = &run.resume_guard {
                return store.defer_active_resume(&run.id, thread);
            }
            if handoff.is_some() {
                eprintln!("Retry preparation for handed-off Run {}: {error:#}", run.id);
                return store.retry_resume(&run.id);
            }
            return settle(store, run, &planned_cwd, Err(error), false);
        }
    };
    let branches = workspace::branches(run, &cwd);
    let (instructions, tools) = tools::catalogue(
        &run.assignment,
        runtime.proposal_approval.load(Ordering::Relaxed),
        runtime.stage_attention.load(Ordering::Relaxed),
        store.issue_rename_supported()?,
    );
    let native_context =
        native_context(runtime, run, agent, app_server, &cwd, instructions, &tools)?;
    let local = store.reusable_thread(run, &native_context)?;
    let launch = frozen_launch(runtime, app_server, &cwd).and_then(|launch| {
        let execution = execution_stamp(run, agent, &launch, &cwd, instructions, &tools)?;
        Ok((launch, execution))
    });
    let (launch, execution) = match launch {
        Ok(pair) => pair,
        Err(error)
            if handoff.is_some()
                || (is_review && local.is_some())
                || run.resume_guard.is_some() =>
        {
            eprintln!(
                "Retry native launch preparation for Run {}: {error:#}",
                run.id
            );
            return store.retry_resume(&run.id);
        }
        Err(error) => return settle(store, run, &cwd, Err(error), false),
    };
    let handoff = handoff.filter(|binding| {
        if let Some(reason) = incompatible_handoff(&binding.execution, &execution) {
            if run.resume_guard.is_none() {
                log_handoff_fallback(run, &binding.run_id, reason);
            }
            false
        } else {
            true
        }
    });
    let previous = handoff.map(|binding| binding.thread_id.clone()).or(local);
    if let Some(guarded) = &run.resume_guard
        && previous.as_ref() != Some(guarded)
    {
        // Configuration drift cannot clear an unresolved active-thread fence.
        return store.defer_active_resume(&run.id, guarded);
    }
    let app_server = Some(&launch);
    let mut app = match client(runtime, &cwd, app_server) {
        Ok(app) => app,
        Err(error)
            if incoming || (is_review && previous.is_some()) || run.resume_guard.is_some() =>
        {
            eprintln!("Retry native App Server for Run {}: {error:#}", run.id);
            return store.retry_resume(&run.id);
        }
        Err(error) => return settle(store, run, &cwd, Err(error), false),
    };
    let reused = if let Some(thread) = &previous {
        match app.resume_work_thread(
            thread,
            &cwd,
            agent.model.as_deref(),
            run.resume_guard.is_some(),
            is_review || handoff.is_some(),
        ) {
            Ok(WorkThreadResume::Ready) => {
                if let Some(binding) = handoff
                    && let Err(error) = app.confirm_work_frontier(thread, &binding.turn_id)
                {
                    app.stop()?;
                    eprintln!(
                        "Retry native frontier for handed-off Run {}: {error:#}",
                        run.id
                    );
                    // The exact thread was verified idle. An unreadable
                    // frontier fences new turns, not a pre-launch Stop.
                    return store.retry_idle_frontier(&run.id, thread);
                }
                true
            }
            Ok(WorkThreadResume::Active) => {
                store.defer_active_resume(&run.id, thread)?;
                app.stop()?;
                if run.resume_guard.is_none() {
                    eprintln!(
                        "Task {} still has an active native thread; holding the same Run and workspace",
                        run.assignment.task_id
                    );
                }
                return Ok(());
            }
            _ if run.resume_guard.is_some() => {
                // Neither loss of the rollout nor a transport error proves a
                // previously reported active turn has exited.
                app.stop()?;
                return store.defer_active_resume(&run.id, thread);
            }
            Ok(WorkThreadResume::Unavailable) => {
                if let Some(binding) = handoff {
                    // A negative lookup with a surviving original (or an
                    // unreadable store) is not evidence of permanent loss.
                    match codex_app::usage::rollout_index(&execution.codex_home.path) {
                        Ok(rollouts) if !rollouts.contains_key(thread) => {
                            log_handoff_fallback(
                                run,
                                &binding.run_id,
                                "original native thread no longer exists",
                            );
                        }
                        _ => {
                            app.stop()?;
                            eprintln!(
                                "Native lookup for Run {} disagrees with the original rollout store",
                                run.id
                            );
                            return store.retry_resume(&run.id);
                        }
                    }
                }
                false
            }
            Err(error) if is_review || handoff.is_some() => {
                app.stop()?;
                eprintln!(
                    "Retry original native thread for Task {}: {error:#}",
                    run.assignment.task_id
                );
                return store.retry_resume(&run.id);
            }
            Err(error) => {
                // Resume has not submitted any turn. Fence this App Server
                // before trying a fresh session, unlike an unknown turn/start.
                app.stop()
                    .context("confirm stopped App Server after resume error")?;
                eprintln!(
                    "Cannot load native thread for Task {}: {error:#}",
                    run.assignment.task_id
                );
                match client(runtime, &cwd, app_server) {
                    Ok(fresh) => app = fresh,
                    Err(error) => return settle(store, run, &cwd, Err(error), false),
                }
                false
            }
        }
    } else {
        false
    };
    let mut may_have_started = false;
    let mut empty_thread_process = None;
    let result = (|| {
        if store.has_stop(&run.assignment.placement.assignment_id)? {
            return Err(StopRequested(None).into());
        }
        let process = app.process_identity()?;
        if !reused && incoming {
            store.begin_empty_thread(&run.id, &process)?;
            empty_thread_process = Some(process);
        }
        let thread_id = if reused {
            previous.expect("reused thread was selected")
        } else {
            app.start_thread(&cwd, instructions, &tools, agent.model.as_deref())?
        };
        let mut input = serde_json::to_value(&run.assignment)?;
        // Native provenance authorizes execution, not a model-facing action.
        if let Some(handoff) = input
            .get_mut("handoff")
            .and_then(serde_json::Value::as_object_mut)
        {
            handoff.remove("native_thread");
        }
        let input = serde_json::to_string(&input)?;
        let context = store.coordination_context(run, reused, artifact.as_deref())?;
        let collector = app.work_rollout(&thread_id).and_then(|path| {
            match crate::usage::Collector::new(path, &thread_id, reused) {
                Ok(collector) => collector,
                Err(error) => {
                    eprintln!("Open native usage evidence for Run {}: {error}", run.id);
                    None
                }
            }
        });
        // Persist the exact process and thread before sending a turn/start.
        // Subsequent failures are reconciled by Run ID, never resubmitted.
        anyhow::ensure!(
            execution_stamp(run, agent, &launch, &cwd, instructions, &tools)? == execution,
            "native execution context changed before turn submission"
        );
        if store.has_stop(&run.assignment.placement.assignment_id)? {
            return Err(StopRequested(None).into());
        }
        store.attach_process(
            &run.id,
            &thread_id,
            &app.process_identity()?,
            &native_context,
            app_server,
            &execution,
        )?;
        may_have_started = true;
        let mut handler = Handler {
            store,
            run,
            cwd: &cwd,
            artifacts: &runtime.artifacts.root,
            proposal_approval: &runtime.proposal_approval,
            stage_attention: &runtime.stage_attention,
            branches: &branches,
            config,
            collector,
            usage_turn: None,
        };
        let result = app.run_turn(
            TurnInput {
                thread_id: &thread_id,
                client_id: &run.id,
                text: &input,
                context: context.as_deref(),
                effort: agent.reasoning_effort.as_deref(),
                discussion_scope: None,
            },
            &mut handler,
        );
        handler.flush_usage(&thread_id);
        let turn = result?;
        anyhow::ensure!(
            turn.status != TurnStatus::InProgress,
            "native turn is not terminal"
        );
        Ok(turn)
    })();
    // A terminal notification alone does not prove the managed process has
    // exited. Leave the active record intact if stop cannot be verified.
    app.stop()?;
    if !may_have_started && let Some(process) = &empty_thread_process {
        store.release_empty_thread(&run.id, process)?;
    }
    if let Err(error) = &result
        && !error.is::<StopRequested>()
    {
        if may_have_started && !codex_app::rejected_turn_start(error) {
            eprintln!(
                "Reconciling original native turn for Run {}: {error:#}",
                run.id
            );
            return store.retry_native_turn(&run.id);
        }
        if !may_have_started && incoming {
            eprintln!("Retry preparation for handed-off Run {}: {error:#}", run.id);
            return store.retry_resume(&run.id);
        }
    }
    settle(store, run, &cwd, result, false)
}

fn settle(
    store: &mut Store,
    run: &LocalRun,
    cwd: &Path,
    result: Result<Turn>,
    recovered: bool,
) -> Result<()> {
    let branches = workspace::branches(run, cwd);
    match result {
        Ok(turn) => {
            let observed = (recovered && run.turn_id.is_none()).then_some(turn.id.as_str());
            if turn.status == TurnStatus::Completed {
                store.finish(
                    run,
                    RunOutcome::Completed,
                    None,
                    observed,
                    &branches,
                    Some(&turn.items),
                )
            } else {
                let reason = turn.error.map_or_else(
                    || format!("Codex turn ended with {:?}", turn.status),
                    |error| error.message,
                );
                store.finish(
                    run,
                    RunOutcome::Failed,
                    Some(reason),
                    observed,
                    &branches,
                    Some(&turn.items),
                )
            }
        }
        Err(error) => store.finish(
            run,
            RunOutcome::Failed,
            Some(format!("{error:#}")),
            None,
            &branches,
            None,
        ),
    }
}

#[derive(Debug)]
struct StopRequested(Option<String>);

impl std::fmt::Display for StopRequested {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            Some(control) => write!(f, "Task cancelled by stop control {control}"),
            None => f.write_str("Task cancelled before launch"),
        }
    }
}

impl std::error::Error for StopRequested {}

struct Handler<'a> {
    store: &'a mut Store,
    run: &'a LocalRun,
    cwd: &'a Path,
    artifacts: &'a Path,
    proposal_approval: &'a AtomicBool,
    stage_attention: &'a AtomicBool,
    branches: &'a [protocol::WorkBranch],
    config: &'a Config,
    collector: Option<crate::usage::Collector>,
    usage_turn: Option<String>,
}

impl Handler<'_> {
    fn flush_usage(&mut self, thread: &str) {
        let (Some(collector), Some(turn)) = (&mut self.collector, &self.usage_turn) else {
            return;
        };
        let result = (|| -> Result<()> {
            for (id, last) in collector.matched(thread, turn)? {
                self.store
                    .persist_token_usage(&self.run.id, thread, turn, &id, last)?;
            }
            Ok(())
        })();
        // This is observational and cannot abort the work turn.
        if let Err(error) = result {
            eprintln!("Persist token usage for Run {}: {error:#}", self.run.id);
        }
    }
}

impl TurnHandler for Handler<'_> {
    fn started(&mut self, turn_id: &str) -> Result<()> {
        self.store.mark_started(self.run, turn_id, self.branches)?;
        self.usage_turn = Some(turn_id.to_owned());
        Ok(())
    }

    fn usage(&mut self, thread: &str, _turn: &str, usage: &serde_json::Value) -> Result<()> {
        if let Some(collector) = &mut self.collector {
            collector.notice(usage);
            self.flush_usage(thread);
        }
        Ok(())
    }

    fn request_usage(&mut self, thread: &str, turn: &str, response: crate::usage::Response) {
        if let Err(error) =
            self.store
                .persist_token_usage(&self.run.id, thread, turn, &response.id, response.meter)
        {
            eprintln!(
                "Persist response identity for Run {}: {error:#}",
                self.run.id
            );
        }
    }

    fn child_request_usage(
        &mut self,
        root_thread: &str,
        root_turn: &str,
        thread: &str,
        turn: &str,
        response: crate::usage::Response,
    ) {
        if let Err(error) = self.store.persist_child_token_usage(
            &self.run.id,
            (root_thread, root_turn),
            (thread, turn),
            &response.id,
            response.meter,
        ) {
            eprintln!("Persist child response for Run {}: {error:#}", self.run.id);
        }
    }

    fn checks_controls(&self) -> bool {
        true
    }

    fn native_item(&mut self, thread: &str, turn: &str, item: &serde_json::Value) -> Result<()> {
        let Ok(observation) = serde_json::from_value::<protocol::SubagentObservation>(item.clone())
        else {
            return Ok(());
        };
        if observation.sender_thread_id != thread {
            return Ok(());
        }
        let call = ToolCall {
            thread_id: thread.to_owned(),
            turn_id: turn.to_owned(),
            call_id: format!("native-{}-{}", observation.id, observation.status),
            tool: "observe_native_subagent".to_owned(),
            namespace: None,
            arguments: serde_json::to_value(&observation)?,
        };
        self.store
            .accept_operation(self.run, &call, self.config, |_| {
                Ok(protocol::WorkAction::ObserveSubagent { observation })
            })?;
        Ok(())
    }

    fn observed_user_message(&mut self, client_id: &str) -> Result<()> {
        self.store.observe_steer(&self.run.id, client_id)
    }

    fn controls(&mut self, client: &mut Client, thread: &str, turn: &str) -> Result<()> {
        let assignment = &self.run.assignment.placement.assignment_id;
        if let Some(control) = self.store.next_control(assignment, &self.run.id)? {
            self.store.sending_control(&control.id)?;
            match &control.action {
                ControlAction::Stop => {
                    let _ = client.interrupt(thread, turn);
                    return Err(StopRequested(Some(control.id)).into());
                }
                ControlAction::Steer { text } => {
                    if let Ok(delivered) = client.steer(thread, turn, &control.id, text) {
                        let state = if delivered {
                            ControlState::Delivered
                        } else {
                            ControlState::Undelivered
                        };
                        self.store.finish_control(&control, state)?;
                    }
                }
            }
        }
        // Rollout writes can trail their App Server notice. Recheck only
        // pending observations at the existing active-turn control cadence.
        if self
            .collector
            .as_ref()
            .is_some_and(crate::usage::Collector::has_unmatched)
        {
            self.flush_usage(thread);
        }
        Ok(())
    }

    fn tool(&mut self, call: ToolCall) -> Result<ToolOutput> {
        if call.tool == "tend_agent_candidates"
            && call.namespace.is_none()
            && self.run.assignment.owns_root()
        {
            let catalogue = self.store.agent_categories()?;
            if call
                .arguments
                .as_object()
                .is_some_and(|args| args.is_empty())
            {
                let categories: Vec<_> = catalogue
                    .into_iter()
                    .filter(|entry| entry.delegated)
                    .collect();
                return Ok(ToolOutput {
                    success: true,
                    body: json!({
                        "runner_id": self.config.runner_id, "categories": categories,
                    }),
                });
            }
            let query = serde_json::from_value::<protocol::AgentQuery>(call.arguments.clone());
            let error = if let Ok(query) = query
                && query.valid()
            {
                match protocol::check_agent_category(&catalogue, &query.category, Some(false)) {
                    Ok(category) => {
                        let repos = if call.arguments.get("repo_ids").is_some() {
                            &query.repo_ids
                        } else {
                            &self.run.assignment.work.repo_ids
                        };
                        let profile = self.config.profile(0);
                        let candidates: Vec<_> = protocol::matching_agents(
                            &profile,
                            category,
                            &catalogue,
                            repos,
                            &query.required_capabilities,
                        )
                        .collect();
                        let reason = candidates.is_empty().then_some(
                            "This Runner has no configuration matching the category, repositories and capabilities"
                        );
                        return Ok(ToolOutput {
                            success: true,
                            body: json!({
                                "runner_id": profile.runner_id, "category": category,
                                "candidates": candidates, "reason": reason,
                            }),
                        });
                    }
                    Err(error) => error,
                }
            } else {
                "Provide a valid category ID and task requirements".to_owned()
            };
            return Ok(ToolOutput {
                success: false,
                body: json!({"error": error}),
            });
        }
        match self
            .store
            .accept_operation(self.run, &call, self.config, |id| {
                tools::action(
                    self.run,
                    &call,
                    self.cwd,
                    self.artifacts,
                    id,
                    self.proposal_approval.load(Ordering::Relaxed),
                    self.stage_attention.load(Ordering::Relaxed),
                )
            }) {
            Ok(Some(operation)) => Ok(ToolOutput {
                success: true,
                body: serde_json::to_value(operation)?,
            }),
            Ok(None) => Ok(ToolOutput {
                success: false,
                body: json!({"error": "The original tool call has different arguments"}),
            }),
            Err(error) => Ok(ToolOutput {
                success: false,
                body: json!({"error": format!("{error:#}")}),
            }),
        }
    }
}
