use crate::{
    config::Config,
    store::{Store, run::LocalRun},
};
use crate::{tools, workspace};
use anyhow::{Context, Result, bail};
use codex_app::{Client, ToolCall, ToolOutput, Turn, TurnHandler, TurnInput, TurnStatus};
use protocol::{ControlAction, ControlState, RunOutcome, TOKEN_ENV};
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
    if run.process.is_none() && store.has_stop(&run.assignment.placement.assignment_id)? {
        return settle(
            &mut store,
            run,
            &cwd,
            Err(anyhow::anyhow!("Task cancelled before launch")),
            false,
        );
    }
    if run.process.is_some() {
        let result = recover(runtime, run, &cwd);
        settle(&mut store, run, &cwd, result, true)?;
    } else {
        launch(&mut store, runtime, run, config)?;
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

fn launch(store: &mut Store, runtime: &Runtime, run: &LocalRun, config: &Config) -> Result<()> {
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
        let (instructions, tools) = tools::catalogue(&agent.category);
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
