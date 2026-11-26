use super::{Store, external, pull};
use anyhow::Result;
use codex_app::ToolCall;
use protocol::{ExternalCondition, ExternalFact, GitHubPull, WatchMode};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;

pub enum Registration {
    Saved(String),
    Missing,
    Conflict,
}

pub fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE INDEX issue_pulls_by_url ON issue_pulls(url);
         CREATE TABLE external_routes (
           id TEXT PRIMARY KEY, message_seq INTEGER NOT NULL REFERENCES chat_messages(seq),
           url TEXT NOT NULL REFERENCES github_pulls(url), condition TEXT NOT NULL,
           mode TEXT NOT NULL CHECK(mode IN ('optional', 'required'))
         );
         CREATE INDEX external_routes_by_url ON external_routes(url);
         CREATE UNIQUE INDEX external_route_router ON external_routes(message_seq, condition)
           WHERE mode = 'optional';
         ALTER TABLE external_watches ADD COLUMN route_id TEXT REFERENCES external_routes(id);
         ALTER TABLE external_watches ADD COLUMN origin_issue TEXT REFERENCES issues(id);
         CREATE UNIQUE INDEX external_watch_route ON external_watches(route_id, task_id) WHERE route_id IS NOT NULL;
         CREATE TABLE external_route_batches (
           id TEXT PRIMARY KEY, chat_id TEXT NOT NULL, due_at INTEGER NOT NULL, released_at INTEGER
         );
         CREATE UNIQUE INDEX open_external_route_batch
           ON external_route_batches(chat_id) WHERE released_at IS NULL;
         CREATE INDEX due_external_route_batch
           ON external_route_batches(due_at) WHERE released_at IS NULL;
         CREATE TABLE external_route_deliveries (
           id TEXT PRIMARY KEY, route_id TEXT NOT NULL REFERENCES external_routes(id),
           fact_id TEXT NOT NULL REFERENCES external_facts(id),
           batch_id TEXT NOT NULL REFERENCES external_route_batches(id),
           origin_issue TEXT REFERENCES issues(id), UNIQUE(route_id, fact_id)
         );
         CREATE INDEX external_routes_by_batch ON external_route_deliveries(batch_id);
         ALTER TABLE router_inputs ADD COLUMN external_batch TEXT REFERENCES external_route_batches(id);
         DROP INDEX router_user_input;
         CREATE UNIQUE INDEX router_user_input ON router_inputs(message_seq)
           WHERE report_id IS NULL AND attention_id IS NULL AND external_batch IS NULL;
         CREATE UNIQUE INDEX router_external_input ON router_inputs(external_batch)
           WHERE external_batch IS NOT NULL;",
    )?;
    Ok(())
}

impl Store {
    pub fn require_external(
        &mut self,
        operation: &str,
        message: &str,
        condition: &ExternalCondition,
    ) -> Result<Registration> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(source) = tx
            .query_row(
                "SELECT m.seq FROM chat_messages m WHERE m.id = ?1 AND m.operation_id IS NOT NULL
                 AND NOT EXISTS(SELECT 1 FROM discussions d WHERE d.chat_id=m.chat_id)",
                [message],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
        else {
            return Ok(Registration::Missing);
        };
        let encoded = serde_json::to_string(condition)?;
        if let Some((saved, original, mode)) = tx
            .query_row(
                "SELECT message_seq, condition, mode FROM external_routes WHERE id = ?1",
                [operation],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?
        {
            return Ok(
                if saved == source && original == encoded && mode == "required" {
                    Registration::Saved(operation.to_owned())
                } else {
                    Registration::Conflict
                },
            );
        }
        register(&tx, operation, source, condition, "required")?;
        tx.commit()?;
        Ok(Registration::Saved(operation.to_owned()))
    }

    pub fn router_subscribe(
        &mut self,
        attempt: &str,
        call: &ToolCall,
        condition: &ExternalCondition,
    ) -> Result<Option<String>> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(source) = tx
            .query_row(
                "SELECT i.message_seq FROM router_attempts a JOIN router_inputs i ON i.seq = a.seq
             WHERE a.id = ?1 AND a.thread_id = ?2 AND a.turn_id = ?3 AND i.outcome IS NULL",
                params![attempt, call.thread_id, call.turn_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
        else {
            return Ok(None);
        };
        if let Some(id) = tx.query_row(
            "SELECT id FROM external_routes WHERE message_seq = ?1 AND condition = ?2 AND mode = 'optional'",
            params![source, serde_json::to_string(condition)?],
            |row| row.get::<_, String>(0),
        ).optional()? {
            return Ok(Some(id));
        }
        let id = protocol::new_id();
        register(&tx, &id, source, condition, "optional")?;
        tx.commit()?;
        Ok(Some(id))
    }

    pub fn release_external_routes(&mut self) -> Result<()> {
        let Some((batch, chat)) = self
            .db
            .query_row(
                "SELECT id, chat_id FROM external_route_batches
             WHERE released_at IS NULL AND due_at <= unixepoch() ORDER BY due_at, rowid LIMIT 1",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
        else {
            return Ok(());
        };
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO router_inputs(chat_id, message_seq, external_batch)
             SELECT ?2, r.message_seq, d.batch_id
             FROM external_route_deliveries d JOIN external_routes r ON r.id = d.route_id
             WHERE d.batch_id = ?1 ORDER BY d.rowid LIMIT 1",
            params![batch, chat],
        )?;
        tx.execute(
            "UPDATE external_route_batches SET released_at = unixepoch() WHERE id = ?1",
            [&batch],
        )?;
        tx.commit()?;
        Ok(())
    }
}

fn register(
    db: &Connection,
    id: &str,
    source: i64,
    condition: &ExternalCondition,
    mode: &str,
) -> Result<()> {
    let pull = GitHubPull::parse(&condition.url).expect("validated condition URL");
    pull::ensure_observed(db, &pull)?;
    db.execute(
        "INSERT INTO external_routes(id, message_seq, url, condition, mode) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, source, condition.url, serde_json::to_string(condition)?, mode],
    )?;
    let mut query = db.prepare(
        "SELECT f.id, f.fact FROM external_latest l JOIN external_facts f ON f.id = l.fact_id
         JOIN github_pulls p ON p.url = l.url
         WHERE l.url = ?1 AND p.observation_error IS NULL AND f.error IS NULL
           AND (?2 IS NOT NULL OR json_extract(f.fact, '$.head_sha') = json_extract(p.facts, '$.head_sha'))
         ORDER BY f.rowid",
    )?;
    for row in query.query_map(params![condition.url, condition.head_sha], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })? {
        let (fact_id, raw) = row?;
        if condition.matches(&serde_json::from_str(&raw)?) {
            queue(db, id, &fact_id, &condition.url, mode, source)?;
        }
    }
    db.execute(
        "UPDATE github_pulls SET due_at = unixepoch() WHERE url = ?1 AND due_at IS NULL",
        [&condition.url],
    )?;
    Ok(())
}

pub fn observe(
    db: &Connection,
    fact_id: &str,
    fact: &ExternalFact,
    pinned_only: bool,
) -> Result<()> {
    let mut query = db.prepare(
        "SELECT id, message_seq, condition, mode FROM external_routes WHERE url = ?1 ORDER BY rowid",
    )?;
    for row in query.query_map([&fact.url], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })? {
        let (id, source, condition, mode) = row?;
        let condition = serde_json::from_str::<ExternalCondition>(&condition)?;
        if (!pinned_only || condition.head_sha.is_some()) && condition.matches(fact) {
            queue(db, &id, fact_id, &fact.url, &mode, source)?;
        }
    }
    Ok(())
}

fn queue(
    db: &Connection,
    watch: &str,
    fact: &str,
    url: &str,
    mode: &str,
    source: i64,
) -> Result<()> {
    let chat: String = db.query_row(
        "SELECT chat_id FROM chat_messages WHERE seq = ?1",
        [source],
        |row| row.get(0),
    )?;
    let association = if mode == "required" {
        associated_issue(db, &chat, url)?
    } else {
        None
    };
    if let Some((root, origin)) = association {
        let task: Option<String> = db
            .query_row(
                "SELECT t.id FROM tasks t JOIN issues i ON i.id = t.issue_id
             WHERE t.issue_id = ?1 AND i.state != 'cancelled'
             ORDER BY t.rowid DESC LIMIT 1",
                [&root],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(task) = task {
            return external::queue_required(db, watch, fact, url, &root, &origin, &task);
        }
    }
    queue_router(db, watch, fact, &chat, None, None)
}

pub(super) fn queue_router(
    db: &Connection,
    watch: &str,
    fact: &str,
    chat: &str,
    origin_issue: Option<&str>,
    due_at: Option<i64>,
) -> Result<()> {
    db.execute(
        "INSERT INTO external_route_batches(id, chat_id, due_at)
         VALUES (?1, ?2, coalesce(?3, unixepoch() + 300)) ON CONFLICT DO NOTHING",
        params![protocol::new_id(), chat, due_at],
    )?;
    db.execute(
        "INSERT INTO external_route_deliveries(id, route_id, fact_id, batch_id, origin_issue)
         SELECT ?1, ?2, ?3, id, ?4 FROM external_route_batches WHERE chat_id = ?5 AND released_at IS NULL
         ON CONFLICT(route_id, fact_id) DO NOTHING",
        params![protocol::new_id(), watch, fact, origin_issue, chat],
    )?;
    Ok(())
}

fn associated_issue(db: &Connection, chat: &str, url: &str) -> Result<Option<(String, String)>> {
    let mut query = db.prepare(
        "WITH RECURSIVE tree(id, parent, origin) AS (
           SELECT i.id, i.parent_issue_id, i.id FROM issue_pulls p JOIN issues i ON i.id = p.issue_id
           JOIN chat_messages m ON m.seq = i.source_seq WHERE p.url = ?1 AND m.chat_id = ?2
           UNION ALL
           SELECT i.id, i.parent_issue_id, t.origin FROM tree t JOIN issues i ON i.id = t.parent
         )
         SELECT id, coalesce(max(CASE WHEN origin = id THEN origin END), min(origin))
         FROM tree WHERE parent IS NULL GROUP BY id ORDER BY id LIMIT 2",
    )?;
    let mut rows = query.query(params![url, chat])?;
    let Some(first) = rows
        .next()?
        .map(|row| Ok::<_, rusqlite::Error>((row.get(0)?, row.get(1)?)))
        .transpose()?
    else {
        return Ok(None);
    };
    Ok(rows.next()?.is_none().then_some(first))
}

#[derive(Serialize)]
struct Delivery {
    id: String,
    watch_id: String,
    fact_id: String,
    original_message_id: String,
    origin_issue: Option<String>,
    mode: WatchMode,
    fact: ExternalFact,
}

fn deliveries(db: &Connection, batch: &str) -> Result<Vec<Delivery>> {
    let mut query = db.prepare(
        "SELECT d.id, d.route_id, d.fact_id, m.id, d.origin_issue, r.mode, f.fact
         FROM external_route_deliveries d JOIN external_routes r ON r.id = d.route_id
         JOIN external_facts f ON f.id = d.fact_id JOIN chat_messages m ON m.seq = r.message_seq
         WHERE d.batch_id = ?1 ORDER BY d.rowid",
    )?;
    query
        .query_map([batch], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        })?
        .map(|row| {
            let (id, watch_id, fact_id, original_message_id, origin_issue, mode, raw) = row?;
            Ok(Delivery {
                id,
                watch_id,
                fact_id,
                original_message_id,
                origin_issue,
                mode: if mode == "required" {
                    WatchMode::Required
                } else {
                    WatchMode::Optional
                },
                fact: serde_json::from_str(&raw)?,
            })
        })
        .collect()
}

pub fn context(db: &Connection, batch: &str) -> Result<String> {
    Ok(format!(
        "Tend external facts collected for this Chat.\n{}",
        serde_json::to_string(&deliveries(db, batch)?)?
    ))
}
