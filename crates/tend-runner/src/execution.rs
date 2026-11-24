use crate::{
    config::Config,
    store::{Store, run::LocalRun},
};
use crate::{tools, workspace};
use anyhow::{Context, Result};
use codex_app::{Client, ToolCall, ToolOutput, Turn, TurnHandler, TurnInput, TurnStatus};
use protocol::{RunOutcome, TOKEN_ENV};
use serde_json::json;
use std::{
    collections::HashMap,
    ffi::OsString,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    thread::{self, JoinHandle},
    time::Duration,
};

#[derive(Clone)]
pub struct Runtime {
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
                    if run
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
                let Some(run) = store.next_queued_run()? else {
                    break;
                };
                candidates.push(run);
            }
            for run in candidates {
                let local = Store::open(&db, &config)?;
                let runtime = runtime.clone();
                workers.insert(
                    run.id.clone(),
                    thread::spawn(move || {
                        if let Err(error) = advance(local, &runtime, &run) {
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

fn advance(mut store: Store, runtime: &Runtime, run: &LocalRun) -> Result<()> {
    let cwd = workspace::directory(&runtime.root, run);
    if run.process.is_some() {
        let result = recover(runtime, run, &cwd);
        settle(&mut store, run, &cwd, result, true)?;
    } else {
        launch(&mut store, runtime, run)?;
    }
    Ok(())
}

pub(crate) fn client(runtime: &Runtime, cwd: &Path) -> Result<Client> {
    let mut command = Command::new(&runtime.program);
    command
        .args(&runtime.args)
        .current_dir(cwd)
        .env_remove(TOKEN_ENV);
    Client::connect(&mut command)
}

fn recover(runtime: &Runtime, run: &LocalRun, cwd: &Path) -> Result<Turn> {
    let mut app = client(runtime, cwd)?;
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

fn launch(store: &mut Store, runtime: &Runtime, run: &LocalRun) -> Result<()> {
    let planned_cwd = workspace::directory(&runtime.root, run);
    let started = workspace::prepare(&runtime.root, run)
        .context("prepare Tend workspace")
        .and_then(|directory| Ok((client(runtime, &directory)?, directory)));
    let (mut app, cwd) = match started {
        Ok(started) => started,
        Err(error) => return settle(store, run, &planned_cwd, Err(error), false),
    };
    let branches = workspace::branches(run, &cwd);
    let result = (|| {
        let agent = &run.assignment.placement.agent;
        let coordinator = agent.category == "issue-router";
        let (instructions, tools) = tools::catalogue(coordinator);
        let thread_id = app.start_thread(&cwd, instructions, &tools, agent.model.as_deref())?;
        store.attach_process(&run.id, &thread_id, &app.process_identity()?)?;
        let input = serde_json::to_string(&run.assignment)?;
        let context = store.coordination_context(run)?;
        app.run_turn(
            TurnInput {
                thread_id: &thread_id,
                client_id: &run.id,
                text: &input,
                context: context.as_deref(),
                effort: agent.reasoning_effort.as_deref(),
            },
            &mut Handler {
                store,
                run,
                cwd: &cwd,
                artifacts: &runtime.artifacts,
                branches: &branches,
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
                store.finish(run, RunOutcome::Completed, None, observed, &branches)
            } else {
                let reason = turn.error.map_or_else(
                    || format!("Codex turn ended with {:?}", turn.status),
                    |error| error.message,
                );
                store.finish(run, RunOutcome::Failed, Some(reason), observed, &branches)
            }
        }
        Err(error) => store.finish(
            run,
            RunOutcome::Failed,
            Some(format!("{error:#}")),
            None,
            &branches,
        ),
    }
}

struct Handler<'a> {
    store: &'a mut Store,
    run: &'a LocalRun,
    cwd: &'a Path,
    artifacts: &'a Path,
    branches: &'a [protocol::WorkBranch],
}

impl TurnHandler for Handler<'_> {
    fn started(&mut self, turn_id: &str) -> Result<()> {
        self.store.mark_started(self.run, turn_id, self.branches)
    }

    fn tool(&mut self, call: ToolCall) -> Result<ToolOutput> {
        match self.store.accept_operation(self.run, &call, |id| {
            tools::action(self.run, &call, self.cwd, self.artifacts, id)
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
