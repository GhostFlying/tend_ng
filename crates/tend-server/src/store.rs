mod artifacts;
pub(crate) mod attention;
mod board;
pub(crate) mod control;
mod coordination;
pub(crate) mod discovery;
pub mod discussion;
mod execution_hosts;
mod external;
pub(crate) mod external_routing;
mod handoff;
mod issue_chat;
mod notification;
pub(crate) mod proposals;
pub(crate) mod pull;
mod reclaim;
pub(crate) mod report_revisions;
mod reports;
mod review;
mod run;
mod runner;
mod short_numbers;
mod tend_review;
mod timing;
mod token_usage;
pub(crate) mod web_sessions;
mod work;

#[cfg(test)]
pub(crate) fn remove_token_usage_schema(db: &Connection) -> Result<()> {
    db.execute_batch(
        "DROP TRIGGER token_issue_tree_insert;
         DROP TABLE token_usage_pending;
         DROP TABLE token_requests;
         DROP TABLE token_runs;
         DROP TABLE token_issue_rollups;
         DROP TABLE token_issue_tree;",
    )?;
    Ok(())
}

use anyhow::{Context, Result, ensure};
use codex_app::{Answer, ProcessIdentity};
use protocol::{Author, Message, MessagePage, Receipt, RouterState, SendMessage};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::json;
use std::{collections::HashMap, path::Path, time::Duration};

const PAGE_SIZE: usize = 50;
pub(crate) const SCHEMA_VERSION: i64 = 38;

pub struct Store {
    db: Connection,
}

pub struct Input {
    pub seq: i64,
    pub message_seq: i64,
    pub message_id: String,
    pub text: String,
    pub report_id: Option<String>,
    pub attention_id: Option<String>,
    pub external_batch: Option<String>,
    pub reply_context: Option<String>,
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
    InvalidReply,
}

impl Store {
    pub fn initialize(path: &Path) -> Result<Self> {
        let mut store = Self::connect(path)?;
        store.db.pragma_update(None, "journal_mode", "WAL")?;
        let tx = store.db.transaction()?;
        let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
        ensure!(
            version <= SCHEMA_VERSION,
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
        }
        if version < 4 {
            run::create_tables(&tx)?;
        }
        if version < 5 {
            coordination::create_tables(&tx)?;
        }
        if version < 6 {
            coordination::migrate_input_targets(&tx)?;
        }
        if version < 7 {
            artifacts::create_tables(&tx)?;
        }
        if version < 8 {
            reports::migrate_queue(&tx)?;
        }
        if version < 9 {
            attention::migrate(&tx)?;
        }
        if version < 10 {
            issue_chat::migrate(&tx)?;
        }
        if version < 11 {
            notification::migrate(&tx)?;
        }
        if version < 12 {
            pull::migrate(&tx)?;
        }
        if version < 13 {
            discovery::migrate(&tx)?;
        }
        if version < 14 {
            external::migrate(&tx)?;
        }
        if version < 15 {
            control::migrate(&tx)?;
        }
        if version < 16 {
            handoff::migrate(&tx)?;
        }
        if version < 17 {
            review::migrate(&tx)?;
        }
        if version < 18 {
            tx.execute_batch("ALTER TABLE tasks ADD COLUMN base_refs TEXT NOT NULL DEFAULT '{}';")?;
        }
        if version < 19 {
            reclaim::migrate(&tx)?;
        }
        if version < 20 {
            external_routing::migrate(&tx)?;
        }
        if version < 21 {
            tx.execute_batch(
                "ALTER TABLE tasks ADD COLUMN requested_agent TEXT;
                              ALTER TABLE router_threads ADD COLUMN model TEXT;",
            )?;
        }
        if version < 22 {
            tx.execute_batch(
                "UPDATE external_latest SET stream = 'ci:' || (
                     SELECT json_extract(fact, '$.head_sha') FROM external_facts WHERE id = external_latest.fact_id
                 ) WHERE stream = 'ci';"
            )?;
        }
        if version < 23 {
            work::migrate_backlog(&tx)?;
        }
        if version < 24 {
            short_numbers::migrate(&tx)?;
        }
        if version < 25 {
            coordination::migrate_continuity(&tx)?;
            tend_review::migrate(&tx)?;
        }
        if version < 26 {
            proposals::migrate(&tx)?;
        }
        if version < 27 {
            report_revisions::migrate(&tx)?;
        }
        if version < 28 {
            board::migrate(&tx)?;
        }
        if version < 29 {
            attention::migrate_issue_replies(&tx)?;
        }
        if version < 30 {
            web_sessions::migrate(&tx)?;
        }
        if version < 31 {
            tend_review::migrate_rounds(&tx)?;
        }
        if version < 32 {
            discussion::migrate(&tx)?;
        }
        if version < 33 {
            discussion::migrate_archive(&tx)?;
        }
        if version < 34 {
            issue_chat::migrate_dispatch_and_takeover(&tx)?;
        }
        if version < 35 {
            runner::migrate_auto_assignment_order(&tx)?;
        }
        if version < 36 {
            handoff::migrate_slot_budget(&tx)?;
        }
        if version < 37 {
            pull::migrate_providers(&tx)?;
        }
        if version < 38 {
            token_usage::migrate(&tx)?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        tx.execute_batch(
            "CREATE INDEX IF NOT EXISTS artifacts_by_task_producer ON artifacts(task_id, runner_id);
             CREATE INDEX IF NOT EXISTS assignments_by_task ON runner_assignments(task_id, runner_id);
             CREATE INDEX IF NOT EXISTS handoff_proofs_by_assignment ON runner_handoff_proofs(predecessor, json_extract(proof, '$.assignment_id'));
             CREATE INDEX IF NOT EXISTS chat_artifacts_by_file ON chat_artifacts(artifact_id, message_id);
             CREATE INDEX IF NOT EXISTS issue_reports_by_issue ON issue_reports(issue_id);
             CREATE INDEX IF NOT EXISTS issue_report_publications_by_report ON issue_report_publications(report_id);",
        )?;
        execution_hosts::install_index(&tx)?;
        tx.commit()?;
        board::install_tracking(&store.db)?;
        Ok(store)
    }

    pub fn open(path: &Path) -> Result<Self> {
        let store = Self::connect(path)?;
        board::install_tracking(&store.db)?;
        Ok(store)
    }

    fn connect(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        db.busy_timeout(Duration::from_secs(5))?;
        db.pragma_update(None, "foreign_keys", true)?;
        db.pragma_update(None, "synchronous", "FULL")?;
        Ok(Self { db })
    }

    pub fn enable_foreground_writes(&self) -> Result<()> {
        self.db.busy_handler(Some(retry_foreground_write))?;
        Ok(())
    }

    fn reserved_chat_operation(db: &Connection, id: &str) -> Result<bool> {
        Ok(db.query_row(
            "SELECT EXISTS(SELECT 1 FROM discussion_handoffs WHERE operation_id=?1)
                 OR EXISTS(SELECT 1 FROM discussion_drafts WHERE id=?1)",
            [id],
            |r| r.get(0),
        )?)
    }

    pub fn accept(&mut self, chat_id: &str, request: &SendMessage) -> Result<Accepted> {
        let (tx, timing) = timing::begin(&mut self.db, "chat_accept")?;
        let existing: Option<(String, String, String, Option<String>)> = tx
            .query_row(
                "SELECT id, chat_id, text, reply_to FROM chat_messages WHERE operation_id = ?1",
                [&request.operation_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        if let Some((message_id, original_chat, original_text, original_reply)) = existing {
            return Ok(
                if !Self::reserved_chat_operation(&tx, &request.operation_id)?
                    && original_chat == chat_id
                    && original_text == request.text
                    && original_reply == request.reply_to
                {
                    Accepted::Received(Receipt { message_id })
                } else {
                    Accepted::Conflict
                },
            );
        }
        if let Some(reply) = &request.reply_to {
            let valid: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM chat_messages m WHERE id = ?1 AND chat_id = ?2 AND NOT EXISTS(SELECT 1 FROM discussion_drafts d WHERE d.message_id=m.id))",
                params![reply, chat_id],
                |row| row.get(0),
            )?;
            if !valid {
                return Ok(Accepted::InvalidReply);
            }
        }

        let receipt = insert_chat_input(&tx, chat_id, request)?;
        tx.execute(
            "UPDATE discussions SET updated_at=unixepoch() WHERE chat_id=?1",
            [chat_id],
        )?;
        timing::commit(tx, timing)?;
        Ok(Accepted::Received(receipt))
    }

    pub fn messages(&self, chat_id: &str, before: i64) -> Result<MessagePage> {
        let discussion = self.is_discussion(chat_id)?;
        let mut statement = self.db.prepare(
            "SELECT m.seq, m.id, m.text, m.created_at, m.reply_to, i.seq,
                    i.outcome, i.error, a.id, h.chat_id
               FROM chat_messages m
               LEFT JOIN router_inputs i ON i.message_seq = m.seq
                 AND i.report_id IS NULL AND i.attention_id IS NULL AND i.external_batch IS NULL
               LEFT JOIN router_attempts a ON a.seq = i.seq AND a.number = 1
               LEFT JOIN discussion_handoffs h ON h.message_id = m.id
              WHERE m.chat_id = ?1 AND m.seq < ?2
                AND NOT EXISTS(SELECT 1 FROM discussion_drafts d WHERE d.message_id=m.id)
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
                    author: if user {
                        Author::User
                    } else if discussion {
                        Author::Discussion
                    } else {
                        Author::Router
                    },
                    text: row.get(2)?,
                    created_at: row.get(3)?,
                    reply_to: row.get(4)?,
                    router_state,
                    router_error: row.get(7)?,
                    issue_ids: Vec::new(),
                    issue_numbers: Vec::new(),
                    artifacts: Vec::new(),
                    attention: None,
                    discussion_id: row.get(9)?,
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
                "SELECT m.seq, i.id, n.number FROM chat_messages m
                 JOIN issues i ON i.source_seq = m.seq
                 JOIN issue_short_numbers n ON n.issue_id = i.id
                 WHERE m.chat_id = ?1 AND m.seq >= ?2 AND m.seq < ?3
                   AND i.parent_issue_id IS NULL
                 ORDER BY m.seq, i.rowid",
            )?;
            let mut issues: HashMap<i64, Vec<(String, i64)>> = HashMap::new();
            for row in statement.query_map(params![chat_id, oldest, before], |row| {
                Ok((row.get::<_, i64>(0)?, (row.get(1)?, row.get(2)?)))
            })? {
                let (seq, issue) = row?;
                issues.entry(seq).or_default().push(issue);
            }
            let mut files = reports::chat_artifacts(&self.db, chat_id, *oldest, before)?;
            let mut attentions = attention::on_page(&self.db, chat_id, *oldest, before)?;
            for (seq, message) in &mut page {
                if let Some(related) = issues.remove(seq) {
                    (message.issue_ids, message.issue_numbers) = related.into_iter().unzip();
                }
                if let Some(artifacts) = files.remove(seq) {
                    message.artifacts = artifacts;
                }
                message.attention = attentions.remove(seq);
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
        let row = self
            .db
            .query_row(
                "SELECT i.seq, m.text, a.id, a.thread_id, a.turn_id,
                        a.number, a.process_pid, a.process_start, m.id, i.message_seq, i.report_id,
                        i.attention_id, m.reply_to, i.external_batch
                 FROM router_inputs i
                 JOIN chat_messages m ON m.seq = i.message_seq
                 LEFT JOIN router_attempts a ON a.seq = i.seq AND a.number = (
                     SELECT MAX(number) FROM router_attempts WHERE seq = i.seq
                 )
                 WHERE i.chat_id = ?1 AND i.outcome IS NULL
                 ORDER BY a.id IS NULL, (i.report_id IS NOT NULL OR i.attention_id IS NOT NULL OR
                                        i.external_batch IS NOT NULL), i.seq LIMIT 1",
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
                    Ok((
                        Input {
                            seq: row.get(0)?,
                            message_id: row.get(8)?,
                            message_seq: row.get(9)?,
                            report_id: row.get(10)?,
                            attention_id: row.get(11)?,
                            external_batch: row.get(13)?,
                            reply_context: None,
                            text: row.get(1)?,
                            attempt,
                        },
                        row.get::<_, Option<String>>(12)?,
                    ))
                },
            )
            .optional()?;
        let Some((mut input, reply_to)) = row else {
            return Ok(None);
        };
        if let Some(report) = &input.report_id {
            input.text = reports::context(&self.db, report, &input.text, &input.message_id)?;
        } else if let Some(id) = &input.attention_id {
            let fact = attention::lookup(&self.db, chat_id, id)?
                .context("queued decision has no Attention record")?;
            let payload = json!({
                "attention_id": id,
                "request": fact.request,
                "proposal": fact.status.proposal,
                "issue_id": fact.issue_id,
                "original_message_id": fact.source_message_id,
                "original_request": input.text,
            });
            input.text = format!("Tend Attention for the user's original topic.\n{payload}");
        } else if let Some(batch) = &input.external_batch {
            input.text = external_routing::context(&self.db, batch)?;
        } else if let Some(reply_to) = reply_to {
            let context = if let Some(fact) = attention::quoted(&self.db, chat_id, &reply_to)? {
                format!(
                    "The following user message explicitly replies to this Tend Attention.\n{}",
                    serde_json::to_string(&fact)?,
                )
            } else {
                let text: String = self.db.query_row(
                    "SELECT text FROM chat_messages WHERE id = ?1",
                    [&reply_to],
                    |row| row.get(0),
                )?;
                let mut query = self.db.prepare(
                    "WITH RECURSIVE thread(seq, reply_to) AS (
                         SELECT seq, reply_to FROM chat_messages WHERE id = ?1
                         UNION ALL SELECT m.seq, m.reply_to FROM chat_messages m JOIN thread t ON m.id = t.reply_to
                     ) SELECT i.id, n.number FROM thread t JOIN issues i ON i.source_seq = t.seq
                       JOIN issue_short_numbers n ON n.issue_id = i.id
                       WHERE i.parent_issue_id IS NULL ORDER BY i.rowid",
                )?;
                let (issues, numbers): (Vec<_>, Vec<_>) = query
                    .query_map([&reply_to], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect::<rusqlite::Result<Vec<(String, i64)>>>()?
                    .into_iter()
                    .unzip();
                let payload = json!({
                    "message_id": reply_to, "text": text, "issue_ids": issues, "issue_numbers": numbers,
                });
                format!("The following user message replies to this saved Chat message.\n{payload}")
            };
            input.reply_context = Some(context);
        }
        Ok(Some(input))
    }

    pub fn thread_id(
        &self,
        chat_id: &str,
        revision: i64,
        model: Option<&str>,
    ) -> Result<Option<String>> {
        Ok(self
            .db
            .query_row(
                "SELECT thread_id FROM router_threads WHERE chat_id = ?1 AND tool_revision = ?2 AND model IS ?3",
                params![chat_id, revision, model],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn save_thread(
        &self,
        chat_id: &str,
        thread_id: &str,
        revision: i64,
        model: Option<&str>,
    ) -> Result<()> {
        self.db.execute(
            "INSERT INTO router_threads (chat_id, thread_id, tool_revision, model) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(chat_id) DO UPDATE
               SET thread_id = excluded.thread_id, tool_revision = excluded.tool_revision, model = excluded.model",
            params![chat_id, thread_id, revision, model],
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
                SELECT message_seq AS seq, outcome FROM router_inputs
                 WHERE chat_id = ?1 AND message_seq < ?2 AND outcome IS NOT NULL
                   AND report_id IS NULL AND attention_id IS NULL AND external_batch IS NULL
                   AND NOT EXISTS(SELECT 1 FROM discussion_drafts d JOIN chat_messages m ON m.id=d.message_id
                                  WHERE m.seq=router_inputs.message_seq)
                 ORDER BY message_seq DESC LIMIT 6
             )
             SELECT m.seq, 'user', substr(m.text, 1, 512), i.outcome
               FROM recent i JOIN chat_messages m ON m.seq = i.seq
             UNION ALL
             SELECT m.seq, 'assistant', substr(m.text, 1, 512), NULL
               FROM recent i JOIN chat_messages source ON source.seq = i.seq
               JOIN chat_messages m ON m.reply_to = source.id AND m.operation_id IS NULL
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
        for (index, answer) in answers.iter().enumerate() {
            let id = protocol::new_id();
            tx.execute(
                "INSERT INTO chat_messages (id, chat_id, text, reply_to, native_item_id)
                 SELECT ?2, m.chat_id, ?3, m.id, ?4 FROM router_inputs i
                 JOIN chat_messages m ON m.seq = i.message_seq WHERE i.seq = ?1",
                params![seq, id, answer.text, answer.item_id],
            )?;
            if index + 1 == answers.len() {
                reports::attach(&tx, &id, seq)?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}

fn insert_chat_input(db: &Connection, chat_id: &str, request: &SendMessage) -> Result<Receipt> {
    let message_id = protocol::new_id();
    db.execute(
        "INSERT INTO chat_messages (id, chat_id, text, operation_id, reply_to)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            message_id,
            chat_id,
            request.text,
            request.operation_id,
            request.reply_to
        ],
    )?;
    db.execute(
        "INSERT INTO router_inputs (message_seq, chat_id) VALUES (?1, ?2)",
        params![db.last_insert_rowid(), chat_id],
    )?;
    Ok(Receipt { message_id })
}

fn retry_foreground_write(attempts: i32) -> bool {
    const FAST_ATTEMPTS: i32 = 50;
    const FAST_INTERVAL_MS: i32 = 2;
    const SLOW_INTERVAL_MS: i32 = 5;
    const MAX_WAIT_MS: i32 = 5_000;
    let fast = attempts.min(FAST_ATTEMPTS);
    let slow = attempts - fast;
    let waited = fast * FAST_INTERVAL_MS + slow * SLOW_INTERVAL_MS;
    let interval = if attempts < FAST_ATTEMPTS {
        FAST_INTERVAL_MS
    } else {
        SLOW_INTERVAL_MS
    };
    if waited + interval > MAX_WAIT_MS {
        return false;
    }
    std::thread::sleep(Duration::from_millis(interval as u64));
    true
}
