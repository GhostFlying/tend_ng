use crate::{
    config::{AppServer, Config},
    store::{Store, run::LocalRun},
};
use crate::{tools, workspace};
use anyhow::{Context, Result, bail};
use codex_app::{
    Client, ToolCall, ToolOutput, Turn, TurnHandler, TurnInput, TurnStatus, WorkThreadResume,
};
use protocol::{ControlAction, ControlState, RunOutcome, TOKEN_ENV};
use serde_json::json;
use std::{
    collections::HashMap,
    ffi::OsString,
    hash::{DefaultHasher, Hash, Hasher},
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
    pub root: PathBuf,
    pub artifacts: PathBuf,
    pub program: OsString,
    pub args: Vec<OsString>,
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
            let limit = if store.is_draining()? {
                0
            } else {
                config.slot_limit as usize
            };
            for _ in active.len()..limit {
                let Some(run) = store.next_queued_run()? else {
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
    if run.process.is_some() {
        let result = if run.assignment.review.is_some() {
            workspace::existing_author_directory(&runtime.root, run)
                .and_then(|path| recover(runtime, run, &path))
        } else {
            recover(runtime, run, &cwd)
        };
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
            if let Some(home) = &app.codex_home {
                command.env("CODEX_HOME", home);
            }
            command
        }
        None => {
            let mut command = Command::new(&runtime.program);
            command.args(&runtime.args);
            command
        }
    };
    command.current_dir(cwd).env_remove(TOKEN_ENV);
    Client::connect(&mut command)
}

fn recover(runtime: &Runtime, run: &LocalRun, cwd: &Path) -> Result<Turn> {
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

fn native_context(
    runtime: &Runtime,
    run: &LocalRun,
    agent: &protocol::AgentProfile,
    app_server: Option<&AppServer>,
    cwd: &Path,
    instructions: &str,
    tools: &[serde_json::Value],
) -> Result<String> {
    // Compare the resolved execution configuration without adding cleartext
    // invocation arguments to the Run ledger.
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
    let home = home
        .or_else(|| std::env::var_os("CODEX_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|path| PathBuf::from(path).join(".codex")));
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

fn launch(store: &mut Store, runtime: &Runtime, run: &LocalRun, config: &Config) -> Result<()> {
    let planned_cwd = workspace::directory(&runtime.root, run);
    let is_review = run.assignment.review.is_some();
    let started = config
        .native_agent(&run.assignment)
        .and_then(|agent| {
            let app_server = agent.app_server.as_ref().map(|id| &config.app_servers[id]);
            let directory = if is_review {
                workspace::verify_review(&runtime.root, run, &config.repos)
            } else {
                workspace::prepare(&runtime.root, run)
            }
            .context("prepare Tend workspace")?;
            Ok((agent, app_server, directory))
        })
        .and_then(|(agent, app_server, directory)| {
            let artifact = if let Some(file) = run
                .assignment
                .review
                .as_ref()
                .and_then(|review| review.candidate.strip_prefix("artifact:"))
            {
                anyhow::ensure!(protocol::valid_id(file), "invalid review artifact");
                let path = runtime.artifacts.canonicalize()?.join(file);
                anyhow::ensure!(
                    path.canonicalize().ok().as_ref() == Some(&path) && path.is_file(),
                    "immutable review artifact is missing from this Runner"
                );
                Some(path)
            } else {
                None
            };
            Ok((directory, agent, app_server, artifact))
        });
    let (cwd, agent, app_server, artifact) = match started {
        Ok(started) => started,
        Err(error) => {
            if let Some(thread) = &run.resume_guard {
                return store.defer_active_resume(&run.id, thread);
            }
            return settle(store, run, &planned_cwd, Err(error), false);
        }
    };
    let branches = workspace::branches(run, &cwd);
    let (instructions, tools) = tools::catalogue(
        &agent.category,
        runtime.proposal_approval.load(Ordering::Relaxed)
            && run.assignment.parent_issue_id.is_none(),
    );
    let native_context =
        native_context(runtime, run, agent, app_server, &cwd, instructions, &tools)?;
    let previous = store.reusable_thread(run, &native_context)?;
    if let Some(guarded) = &run.resume_guard
        && previous.as_ref() != Some(guarded)
    {
        // Configuration drift cannot clear an unresolved active-thread fence.
        return store.defer_active_resume(&run.id, guarded);
    }
    let mut app = match client(runtime, &cwd, app_server) {
        Ok(app) => app,
        Err(error) if is_review && previous.is_some() => {
            eprintln!(
                "Retry native App Server for Review Task {}: {error:#}",
                run.assignment.task_id
            );
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
            is_review,
        ) {
            Ok(WorkThreadResume::Ready) => true,
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
            Ok(WorkThreadResume::Unavailable) => false,
            Err(error) if is_review => {
                app.stop()?;
                eprintln!(
                    "Retry native thread for Review Task {}: {error:#}",
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
    let result = (|| {
        if store.has_stop(&run.assignment.placement.assignment_id)? {
            bail!("Task cancelled before launch");
        }
        let thread_id = if reused {
            previous.expect("reused thread was selected")
        } else {
            app.start_thread(&cwd, instructions, &tools, agent.model.as_deref())?
        };
        store.attach_process(
            &run.id,
            &thread_id,
            &app.process_identity()?,
            &native_context,
            app_server,
            &workspace::stamp(&cwd)?,
        )?;
        let input = serde_json::to_string(&run.assignment)?;
        let context = store.coordination_context(run, reused, artifact.as_deref())?;
        app.run_turn(
            TurnInput {
                thread_id: &thread_id,
                client_id: &run.id,
                text: &input,
                context: context.as_deref(),
                effort: agent.reasoning_effort.as_deref(),
                discussion_scope: None,
            },
            &mut Handler {
                store,
                run,
                cwd: &cwd,
                artifacts: &runtime.artifacts,
                proposal_approval: &runtime.proposal_approval,
                branches: &branches,
                config,
            },
        )
    })();
    // A terminal notification alone does not prove the managed process has
    // exited. Leave the active record intact if stop cannot be verified.
    app.stop()?;
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
                    &turn.items,
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
                    &turn.items,
                )
            }
        }
        Err(error) => store.finish(
            run,
            RunOutcome::Failed,
            Some(format!("{error:#}")),
            None,
            &branches,
            &[],
        ),
    }
}

struct Handler<'a> {
    store: &'a mut Store,
    run: &'a LocalRun,
    cwd: &'a Path,
    artifacts: &'a Path,
    proposal_approval: &'a AtomicBool,
    branches: &'a [protocol::WorkBranch],
    config: &'a Config,
}

impl TurnHandler for Handler<'_> {
    fn started(&mut self, turn_id: &str) -> Result<()> {
        self.store.mark_started(self.run, turn_id, self.branches)
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
        self.store.accept_operation(self.run, &call, |_| {
            Ok(protocol::WorkAction::ObserveSubagent { observation })
        })?;
        Ok(())
    }

    fn observed_user_message(&mut self, client_id: &str) -> Result<()> {
        self.store.observe_steer(&self.run.id, client_id)
    }

    fn controls(&mut self, client: &mut Client, thread: &str, turn: &str) -> Result<()> {
        let assignment = &self.run.assignment.placement.assignment_id;
        let Some(control) = self.store.next_control(assignment, &self.run.id)? else {
            return Ok(());
        };
        self.store.sending_control(&control.id)?;
        match &control.action {
            ControlAction::Stop => {
                let _ = client.interrupt(thread, turn);
                bail!("Task cancelled by stop control {}", control.id);
            }
            ControlAction::Steer { text } => {
                let state = match client.steer(thread, turn, &control.id, text) {
                    Ok(true) => ControlState::Delivered,
                    Ok(false) => ControlState::Undelivered,
                    Err(_) => return Ok(()),
                };
                self.store.finish_control(&control, state)?;
            }
        }
        Ok(())
    }

    fn tool(&mut self, call: ToolCall) -> Result<ToolOutput> {
        if call.tool == "tend_agent_candidates"
            && call.namespace.is_none()
            && self.run.assignment.placement.agent.category == "issue-router"
        {
            let query = serde_json::from_value::<protocol::AgentQuery>(call.arguments.clone());
            if let Ok(query) = query
                && query.valid()
                && query.category != "issue-router"
            {
                let repos = if call.arguments.get("repo_ids").is_some() {
                    &query.repo_ids
                } else {
                    &self.run.assignment.work.repo_ids
                };
                let profile = self.config.profile(0);
                let candidates: Vec<_> = protocol::matching_agents(
                    &profile,
                    &query.category,
                    repos,
                    &query.required_capabilities,
                )
                .collect();
                return Ok(ToolOutput {
                    success: true,
                    body: json!({"runner_id": profile.runner_id, "candidates": candidates}),
                });
            }
            return Ok(ToolOutput {
                success: false,
                body: json!({"error": "Provide a valid work category and task requirements"}),
            });
        }
        match self.store.accept_operation(self.run, &call, |id| {
            tools::action(
                self.run,
                &call,
                self.cwd,
                self.artifacts,
                id,
                self.proposal_approval.load(Ordering::Relaxed),
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
