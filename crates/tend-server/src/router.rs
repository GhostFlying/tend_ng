use crate::store::{Attempt, Store};
use anyhow::{Result, bail};
use codex_app::{Client, Turn, TurnStatus};
use std::{
    collections::HashMap,
    ffi::OsString,
    path::PathBuf,
    process::Command,
    thread::{self, JoinHandle},
    time::Duration,
};

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
    let mut loaded_thread = None;
    while let Some(input) = store.next_input(chat)? {
        let turn = if let Some(attempt) = input.attempt {
            recover(&mut client, &store, attempt)?
        } else {
            let thread_id = match store.thread_id(chat)? {
                Some(id) => {
                    if loaded_thread.as_ref() != Some(&id) {
                        client.resume_thread(&id)?;
                    }
                    id
                }
                None => {
                    let id = client.start_thread(&config.cwd)?;
                    store.save_thread(chat, &id)?;
                    id
                }
            };
            loaded_thread = Some(thread_id.clone());
            let attempt = store.start_attempt(input.seq, &thread_id)?;
            let turn_id = client.start_turn(&thread_id, &attempt.id, &input.text)?;
            store.save_turn_id(&attempt.id, &turn_id)?;
            client.await_turn(&thread_id, &turn_id, &attempt.id)?
        };
        finish(&mut store, input.seq, turn)?;
    }
    Ok(())
}

fn recover(client: &mut Client, store: &Store, mut attempt: Attempt) -> Result<Turn> {
    loop {
        let Some(turn) =
            client.find_turn(&attempt.thread_id, attempt.turn_id.as_deref(), &attempt.id)?
        else {
            bail!("no native turn evidence for attempt {}", attempt.id);
        };
        if attempt.turn_id.is_none() {
            store.save_turn_id(&attempt.id, &turn.id)?;
            attempt.turn_id = Some(turn.id.clone());
        }
        if turn.status != TurnStatus::InProgress {
            return Ok(turn);
        }
        thread::sleep(Duration::from_secs(2));
    }
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
