use crate::store::{Attempt, Store};
use anyhow::{Result, ensure};
use codex_app::{Client, Turn, TurnStatus};
use std::{
    collections::HashMap,
    ffi::OsString,
    path::PathBuf,
    process::Command,
    thread::{self, JoinHandle},
    time::Duration,
};

const MAX_ATTEMPTS: i64 = 2;

#[derive(Clone)]
pub struct Config {
    pub db: PathBuf,
    pub cwd: PathBuf,
    pub program: OsString,
    pub args: Vec<OsString>,
}

pub fn spawn(config: Config) {
    thread::spawn(move || {
        if let Err(error) = dispatch(config) {
            eprintln!("Router scheduler failed: {error:#}");
            std::process::exit(1);
        }
    });
}

fn dispatch(config: Config) -> Result<()> {
    let store = Store::open(&config.db)?;
    let mut workers: HashMap<String, JoinHandle<()>> = HashMap::new();
    loop {
        workers.retain(|_, worker| !worker.is_finished());
        match store.pending_chats() {
            Ok(chats) => {
                for chat in chats {
                    workers.entry(chat.clone()).or_insert_with(|| {
                        let config = config.clone();
                        thread::spawn(move || {
                            if let Err(error) = consume(&config, &chat) {
                                eprintln!("Router chat {chat} is pending: {error:#}");
                                thread::sleep(Duration::from_secs(2));
                            }
                        })
                    });
                }
            }
            Err(error) => eprintln!("Query pending Router input: {error:#}"),
        }
        thread::sleep(Duration::from_millis(200));
    }
}

fn consume(config: &Config, chat: &str) -> Result<()> {
    let mut store = Store::open(&config.db)?;
    let mut command = Command::new(&config.program);
    command
        .args(&config.args)
        .current_dir(&config.cwd)
        .env_remove(protocol::TOKEN_ENV);
    let mut client = Client::connect(&mut command)?;
    let process = client.process_identity()?;
    let mut loaded_thread = None;
    while let Some(input) = store.next_input(chat)? {
        let number = if let Some(attempt) = input.attempt {
            if let Some(turn) = recover(&mut client, &store, &attempt)? {
                finish(&mut store, input.seq, turn)?;
                continue;
            }
            if attempt.number >= MAX_ATTEMPTS {
                store.clear_thread(chat, &attempt.thread_id)?;
                store.finish(
                    input.seq,
                    &[],
                    Some("Could not recover the Router response after two attempts. You can send the request again."),
                )?;
                continue;
            }
            attempt.number + 1
        } else {
            1
        };

        let previous_thread = if number == 1 {
            store.thread_id(chat)?
        } else {
            None
        };
        let (thread_id, context) = match previous_thread {
            Some(id) if loaded_thread.as_ref() == Some(&id) || client.resume_thread(&id)? => {
                (id, None)
            }
            _ => {
                let id = client.start_thread(&config.cwd)?;
                store.save_thread(chat, &id)?;
                (id, store.recent_context(chat, input.seq)?)
            }
        };
        loaded_thread = Some(thread_id.clone());
        let attempt_id = store.start_attempt(input.seq, number, &thread_id, &process)?;
        let turn_id =
            client.start_turn(&thread_id, &attempt_id, &input.text, context.as_deref())?;
        store.save_turn_id(&attempt_id, &turn_id)?;
        let turn = client.await_turn(&thread_id, &turn_id, &attempt_id)?;
        finish(&mut store, input.seq, turn)?;
    }
    Ok(())
}

fn recover(client: &mut Client, store: &Store, attempt: &Attempt) -> Result<Option<Turn>> {
    if let Some(process) = &attempt.process {
        ensure!(
            process.is_stopped()?,
            "App Server for attempt {} is still running",
            attempt.id,
        );
    }
    let turn = match client.find_turn(&attempt.thread_id, attempt.turn_id.as_deref(), &attempt.id) {
        Ok(turn) => turn,
        Err(error) if attempt.process.is_some() => {
            eprintln!(
                "Could not read history for stopped attempt {}: {error:#}",
                attempt.id
            );
            None
        }
        Err(error) => return Err(error),
    };
    if let Some(turn) = turn
        && turn.status != TurnStatus::InProgress
    {
        if attempt.turn_id.is_none() {
            store.save_turn_id(&attempt.id, &turn.id)?;
        }
        return Ok(Some(turn));
    }
    ensure!(
        attempt.process.is_some(),
        "missing stop evidence for the original attempt {}",
        attempt.id,
    );
    Ok(None)
}

fn finish(store: &mut Store, seq: i64, turn: Turn) -> Result<()> {
    match turn.status {
        TurnStatus::Completed => store.finish(seq, &turn.answers()?, None),
        TurnStatus::Failed | TurnStatus::Interrupted => {
            let error = turn
                .error
                .as_ref()
                .map_or("Codex turn ended early", |error| &error.message);
            store.finish(seq, &[], Some(error))
        }
        TurnStatus::InProgress => unreachable!("only a terminal turn can finish an input"),
    }
}
