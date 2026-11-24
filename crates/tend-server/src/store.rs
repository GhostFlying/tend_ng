mod runner;
mod work;

use anyhow::{Result, ensure};
use codex_app::{Answer, ProcessIdentity};
use protocol::{Author, Message, MessagePage, Receipt, RouterState, SendMessage};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::json;
use std::{collections::HashMap, path::Path, time::Duration};

const PAGE_SIZE: usize = 50;

pub struct Store {
    db: Connection,
}

pub struct Input {
    pub seq: i64,
    pub message_id: String,
    pub text: String,
    pub attempt: Option<Attempt>,
}

pub struct Attempt {
    pub id: String,
    pub number: i64,
    pub thread_id: String,
    pub turn_id: Option<String>,
    pub process: Option<ProcessIdentity>,
}

pub enum Accepted {
    Received(Receipt),
    Conflict,
}

impl Store {
    pub fn initialize(path: &Path) -> Result<Self> {
        let mut store = Self::open(path)?;
        store.db.pragma_update(None, "journal_mode", "WAL")?;
        let tx = store.db.transaction()?;
        let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
        ensure!(
            version <= 3,
            "unsupported Server database version {version}"
        );
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS chat_messages (
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
             CREATE INDEX IF NOT EXISTS settled_inputs
                 ON router_inputs(chat_id, seq) WHERE outcome IS NOT NULL;
             CREATE TABLE IF NOT EXISTS router_threads (
                 chat_id TEXT PRIMARY KEY,
                 thread_id TEXT NOT NULL UNIQUE
             );",
        )?;
        if version == 0 {
            let legacy: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = 'router_attempts')",
                [],
                |row| row.get(0),
            )?;
            if legacy {
                tx.execute_batch("ALTER TABLE router_attempts RENAME TO old_router_attempts;")?;
            }
            tx.execute_batch(
                "CREATE TABLE router_attempts (
                     seq INTEGER NOT NULL REFERENCES router_inputs(seq),
                     number INTEGER NOT NULL,
                     id TEXT NOT NULL UNIQUE,
                     thread_id TEXT NOT NULL,
                     turn_id TEXT,
                     process_pid INTEGER,
                     process_start TEXT,
                     PRIMARY KEY (seq, number)
                 );",
            )?;
            if legacy {
                tx.execute_batch(
                    "INSERT INTO router_attempts (seq, number, id, thread_id, turn_id)
                     SELECT seq, 1, id, thread_id, turn_id FROM old_router_attempts;
                     DROP TABLE old_router_attempts;",
                )?;
            }
        }
        if version < 2 {
            tx.execute_batch(
                "ALTER TABLE router_threads ADD COLUMN tool_revision INTEGER NOT NULL DEFAULT 0;",
            )?;
            work::create_tables(&tx)?;
        }
        if version < 3 {
            runner::create_tables(&tx)?;
            tx.pragma_update(None, "user_version", 3)?;
        }
        tx.commit()?;
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
               LEFT JOIN router_attempts a ON a.seq = m.seq AND a.number = 1
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
                    issue_ids: Vec::new(),
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
        if let Some((oldest, _)) = page.last() {
            let mut statement = self.db.prepare(
                "SELECT m.seq, i.id FROM chat_messages m
                 JOIN issues i ON i.source_seq = m.seq
                 WHERE m.chat_id = ?1 AND m.seq >= ?2 AND m.seq < ?3
                 ORDER BY m.seq, i.rowid",
            )?;
            let mut issues: HashMap<i64, Vec<String>> = HashMap::new();
            for row in statement.query_map(params![chat_id, oldest, before], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })? {
                let (seq, id) = row?;
                issues.entry(seq).or_default().push(id);
            }
            for (seq, message) in &mut page {
                if let Some(ids) = issues.remove(seq) {
                    message.issue_ids = ids;
                }
            }
        }
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
                "SELECT i.seq, m.text, a.id, a.thread_id, a.turn_id,
                        a.number, a.process_pid, a.process_start, m.id
                 FROM router_inputs i
                 JOIN chat_messages m ON m.seq = i.seq
                 LEFT JOIN router_attempts a ON a.seq = i.seq AND a.number = (
                     SELECT MAX(number) FROM router_attempts WHERE seq = i.seq
                 )
                 WHERE i.chat_id = ?1 AND i.outcome IS NULL
                 ORDER BY i.seq LIMIT 1",
                [chat_id],
                |row| {
                    let attempt = match row.get::<_, Option<String>>(2)? {
                        Some(id) => Some(Attempt {
                            id,
                            thread_id: row.get(3)?,
                            turn_id: row.get(4)?,
                            number: row.get(5)?,
                            process: match row.get::<_, Option<u32>>(6)? {
                                Some(pid) => Some(ProcessIdentity {
                                    pid,
                                    started_at: row.get(7)?,
                                }),
                                None => None,
                            },
                        }),
                        None => None,
                    };
                    Ok(Input {
                        seq: row.get(0)?,
                        message_id: row.get(8)?,
                        text: row.get(1)?,
                        attempt,
                    })
                },
            )
            .optional()?)
    }

    pub fn thread_id(&self, chat_id: &str, revision: i64) -> Result<Option<String>> {
        Ok(self
            .db
            .query_row(
                "SELECT thread_id FROM router_threads WHERE chat_id = ?1 AND tool_revision = ?2",
                params![chat_id, revision],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn save_thread(&self, chat_id: &str, thread_id: &str, revision: i64) -> Result<()> {
        self.db.execute(
            "INSERT INTO router_threads (chat_id, thread_id, tool_revision) VALUES (?1, ?2, ?3)
             ON CONFLICT(chat_id) DO UPDATE
               SET thread_id = excluded.thread_id, tool_revision = excluded.tool_revision",
            params![chat_id, thread_id, revision],
        )?;
        Ok(())
    }

    pub fn clear_thread(&self, chat_id: &str, thread_id: &str) -> Result<()> {
        self.db.execute(
            "DELETE FROM router_threads WHERE chat_id = ?1 AND thread_id = ?2",
            [chat_id, thread_id],
        )?;
        Ok(())
    }

    pub fn start_attempt(
        &self,
        seq: i64,
        number: i64,
        thread_id: &str,
        process: &ProcessIdentity,
    ) -> Result<String> {
        let id = protocol::new_id();
        self.db.execute(
            "INSERT INTO router_attempts (seq, number, id, thread_id, process_pid, process_start)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![seq, number, id, thread_id, process.pid, process.started_at],
        )?;
        Ok(id)
    }

    pub fn recent_context(&self, chat_id: &str, before: i64) -> Result<Option<String>> {
        let mut statement = self.db.prepare(
            "WITH recent AS (
                SELECT seq, outcome FROM router_inputs
                 WHERE chat_id = ?1 AND seq < ?2 AND outcome IS NOT NULL
                 ORDER BY seq DESC LIMIT 6
             )
             SELECT m.seq, 'user', substr(m.text, 1, 512), i.outcome
               FROM recent i JOIN chat_messages m ON m.seq = i.seq
             UNION ALL
             SELECT m.seq, 'assistant', substr(m.text, 1, 512), NULL
               FROM recent i JOIN chat_messages source ON source.seq = i.seq
               JOIN chat_messages m ON m.reply_to = source.id
             ORDER BY 1 DESC LIMIT 12",
        )?;
        let mut messages = statement
            .query_map(params![chat_id, before], |row| {
                Ok(json!({
                    "author": row.get::<_, String>(1)?,
                    "text": row.get::<_, String>(2)?,
                    "outcome": row.get::<_, Option<String>>(3)?,
                }))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if messages.is_empty() {
            return Ok(None);
        }
        messages.reverse();
        Ok(Some(format!(
            "Recent settled messages in this Tend conversation. Use them as context.\n{}\nThe current user request follows separately.",
            serde_json::to_string(&messages)?
        )))
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
