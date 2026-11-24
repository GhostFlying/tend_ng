use anyhow::{Result, ensure};
use codex_app::Answer;
use protocol::{Author, Message, MessagePage, Receipt, RouterState, SendMessage};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::{path::Path, time::Duration};

const PAGE_SIZE: usize = 50;

pub struct Store {
    db: Connection,
}

pub struct Input {
    pub seq: i64,
    pub text: String,
    pub attempt: Option<Attempt>,
}

pub struct Attempt {
    pub id: String,
    pub thread_id: String,
    pub turn_id: Option<String>,
}

pub enum Accepted {
    Received(Receipt),
    Conflict,
}

impl Store {
    pub fn initialize(path: &Path) -> Result<Self> {
        let store = Self::open(path)?;
        store.db.pragma_update(None, "journal_mode", "WAL")?;
        store.db.execute_batch(
            "BEGIN;
             CREATE TABLE IF NOT EXISTS chat_messages (
                 seq INTEGER PRIMARY KEY,
                 id TEXT NOT NULL UNIQUE,
                 chat_id TEXT NOT NULL,
                 text TEXT NOT NULL,
                 created_at INTEGER NOT NULL DEFAULT (unixepoch()),
                 operation_id TEXT UNIQUE,
                 reply_to TEXT REFERENCES chat_messages(id),
                 native_item_id TEXT,
                 UNIQUE(reply_to, native_item_id)
             );
             CREATE INDEX IF NOT EXISTS chat_history ON chat_messages(chat_id, seq);
             CREATE TABLE IF NOT EXISTS router_inputs (
                 seq INTEGER PRIMARY KEY REFERENCES chat_messages(seq),
                 chat_id TEXT NOT NULL,
                 outcome TEXT CHECK (outcome IN ('completed', 'failed')),
                 error TEXT
             );
             CREATE INDEX IF NOT EXISTS pending_inputs
                 ON router_inputs(chat_id, seq) WHERE outcome IS NULL;
             CREATE TABLE IF NOT EXISTS router_threads (
                 chat_id TEXT PRIMARY KEY,
                 thread_id TEXT NOT NULL UNIQUE
             );
             CREATE TABLE IF NOT EXISTS router_attempts (
                 seq INTEGER PRIMARY KEY REFERENCES router_inputs(seq),
                 id TEXT NOT NULL UNIQUE,
                 thread_id TEXT NOT NULL,
                 turn_id TEXT
             );
             COMMIT;",
        )?;
        Ok(store)
    }

    pub fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        db.busy_timeout(Duration::from_secs(5))?;
        db.pragma_update(None, "foreign_keys", true)?;
        db.pragma_update(None, "synchronous", "FULL")?;
        Ok(Self { db })
    }

    pub fn accept(&mut self, chat_id: &str, request: &SendMessage) -> Result<Accepted> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<(String, String, String)> = tx
            .query_row(
                "SELECT id, chat_id, text FROM chat_messages WHERE operation_id = ?1",
                [&request.operation_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if let Some((message_id, original_chat, original_text)) = existing {
            return Ok(
                if original_chat == chat_id && original_text == request.text {
                    Accepted::Received(Receipt { message_id })
                } else {
                    Accepted::Conflict
                },
            );
        }

        let message_id = protocol::new_id();
        tx.execute(
            "INSERT INTO chat_messages (id, chat_id, text, operation_id)
             VALUES (?1, ?2, ?3, ?4)",
            params![message_id, chat_id, request.text, request.operation_id],
        )?;
        tx.execute(
            "INSERT INTO router_inputs (seq, chat_id) VALUES (?1, ?2)",
            params![tx.last_insert_rowid(), chat_id],
        )?;
        tx.commit()?;
        Ok(Accepted::Received(Receipt { message_id }))
    }

    pub fn messages(&self, chat_id: &str, before: i64) -> Result<MessagePage> {
        let mut statement = self.db.prepare(
            "SELECT m.seq, m.id, m.text, m.created_at, m.reply_to, i.seq,
                    i.outcome, i.error, a.id
               FROM chat_messages m
               LEFT JOIN router_inputs i ON i.seq = m.seq
               LEFT JOIN router_attempts a ON a.seq = m.seq
              WHERE m.chat_id = ?1 AND m.seq < ?2
              ORDER BY m.seq DESC LIMIT ?3",
        )?;
        let rows = statement.query_map(params![chat_id, before, PAGE_SIZE + 1], |row| {
            let user = row.get::<_, Option<i64>>(5)?.is_some();
            let router_state = if user {
                let outcome: Option<String> = row.get(6)?;
                let attempted = row.get::<_, Option<String>>(8)?.is_some();
                Some(match outcome.as_deref() {
                    Some("completed") => RouterState::Completed,
                    Some("failed") => RouterState::Failed,
                    _ if attempted => RouterState::AwaitingTurn,
                    _ => RouterState::Queued,
                })
            } else {
                None
            };
            Ok((
                row.get::<_, i64>(0)?,
                Message {
                    id: row.get(1)?,
                    author: if user { Author::User } else { Author::Router },
                    text: row.get(2)?,
                    created_at: row.get(3)?,
                    reply_to: row.get(4)?,
                    router_state,
                    router_error: row.get(7)?,
                },
            ))
        })?;
        let mut page = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        let next_before = if page.len() > PAGE_SIZE {
            page.pop();
            page.last().map(|(seq, _)| *seq)
        } else {
            None
        };
        let messages = page.into_iter().rev().map(|(_, message)| message).collect();
        Ok(MessagePage {
            messages,
            next_before,
        })
    }

    pub fn pending_chats(&self) -> Result<Vec<String>> {
        let mut statement = self
            .db
            .prepare("SELECT DISTINCT chat_id FROM router_inputs WHERE outcome IS NULL")?;
        Ok(statement
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn next_input(&self, chat_id: &str) -> Result<Option<Input>> {
        Ok(self
            .db
            .query_row(
                "SELECT i.seq, m.text, a.id, a.thread_id, a.turn_id
                 FROM router_inputs i
                 JOIN chat_messages m ON m.seq = i.seq
                 LEFT JOIN router_attempts a ON a.seq = i.seq
                 WHERE i.chat_id = ?1 AND i.outcome IS NULL
                 ORDER BY i.seq LIMIT 1",
                [chat_id],
                |row| {
                    let attempt = match row.get::<_, Option<String>>(2)? {
                        Some(id) => Some(Attempt {
                            id,
                            thread_id: row.get(3)?,
                            turn_id: row.get(4)?,
                        }),
                        None => None,
                    };
                    Ok(Input {
                        seq: row.get(0)?,
                        text: row.get(1)?,
                        attempt,
                    })
                },
            )
            .optional()?)
    }

    pub fn thread_id(&self, chat_id: &str) -> Result<Option<String>> {
        Ok(self
            .db
            .query_row(
                "SELECT thread_id FROM router_threads WHERE chat_id = ?1",
                [chat_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn save_thread(&self, chat_id: &str, thread_id: &str) -> Result<()> {
        self.db.execute(
            "INSERT INTO router_threads (chat_id, thread_id) VALUES (?1, ?2)",
            [chat_id, thread_id],
        )?;
        Ok(())
    }

    pub fn start_attempt(&self, seq: i64, thread_id: &str) -> Result<Attempt> {
        let attempt = Attempt {
            id: protocol::new_id(),
            thread_id: thread_id.to_owned(),
            turn_id: None,
        };
        self.db.execute(
            "INSERT INTO router_attempts (seq, id, thread_id) VALUES (?1, ?2, ?3)",
            params![seq, attempt.id, thread_id],
        )?;
        Ok(attempt)
    }

    pub fn save_turn_id(&self, attempt: &str, turn: &str) -> Result<()> {
        ensure!(
            self.db.execute(
                "UPDATE router_attempts SET turn_id = ?2
                 WHERE id = ?1 AND (turn_id IS NULL OR turn_id = ?2)",
                [attempt, turn],
            )? == 1,
            "turn does not match the saved attempt"
        );
        Ok(())
    }

    pub fn finish(&mut self, seq: i64, answers: &[Answer<'_>], error: Option<&str>) -> Result<()> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let outcome = if error.is_some() {
            "failed"
        } else {
            "completed"
        };
        let updated = tx.execute(
            "UPDATE router_inputs SET outcome = ?2, error = ?3
             WHERE seq = ?1 AND outcome IS NULL",
            params![seq, outcome, error],
        )?;
        if updated == 0 {
            return Ok(());
        }
        for answer in answers {
            tx.execute(
                "INSERT INTO chat_messages (id, chat_id, text, reply_to, native_item_id)
                 SELECT ?2, chat_id, ?3, id, ?4 FROM chat_messages WHERE seq = ?1",
                params![seq, protocol::new_id(), answer.text, answer.item_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}
