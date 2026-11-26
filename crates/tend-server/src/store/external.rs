use super::{Store, coordination};
use anyhow::Result;
use protocol::{
    ExternalCondition, ExternalFact, ExternalKind, GitHubPull, IssueInput, IssueInputCause,
    PullFacts, WatchMode,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::{Value, json};

pub fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(
        "ALTER TABLE tasks ADD COLUMN workspace_key TEXT;
         CREATE TABLE external_facts (
             id TEXT PRIMARY KEY, url TEXT NOT NULL REFERENCES github_pulls(url),
             fact TEXT NOT NULL, error TEXT, observed_at INTEGER NOT NULL DEFAULT (unixepoch())
         );
         CREATE TABLE external_latest (
             url TEXT NOT NULL REFERENCES github_pulls(url), stream TEXT NOT NULL,
             fact_id TEXT NOT NULL REFERENCES external_facts(id), PRIMARY KEY(url, stream)
         );
         CREATE TABLE external_watches (
             id TEXT PRIMARY KEY, issue_id TEXT NOT NULL REFERENCES issues(id),
             task_id TEXT NOT NULL REFERENCES tasks(id), url TEXT NOT NULL REFERENCES github_pulls(url),
             condition TEXT NOT NULL, mode TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'active'
         );
         CREATE INDEX external_active ON external_watches(url) WHERE state = 'active';
         CREATE INDEX external_task ON external_watches(task_id);
         CREATE TABLE external_batches (
             task_id TEXT PRIMARY KEY REFERENCES tasks(id), due_at INTEGER NOT NULL
         );
         CREATE INDEX external_batches_due ON external_batches(due_at);
         CREATE TABLE external_deliveries (
             id TEXT PRIMARY KEY, watch_id TEXT NOT NULL REFERENCES external_watches(id),
             fact_id TEXT NOT NULL REFERENCES external_facts(id), released_at INTEGER,
             UNIQUE(watch_id, fact_id)
         );
         CREATE INDEX external_undelivered ON external_deliveries(watch_id) WHERE released_at IS NULL;"
    )?;
    Ok(())
}

struct Watch {
    id: String,
    task: String,
    condition: ExternalCondition,
    mode: WatchMode,
}

pub fn register(
    db: &Connection,
    id: &str,
    issue: &str,
    task: &str,
    condition: &ExternalCondition,
    mode: WatchMode,
) -> Result<()> {
    let Some(pull) = GitHubPull::parse(&condition.url) else {
        return Ok(());
    };
    super::pull::ensure_observed(db, &(&pull).into())?;
    let mode_name = match mode {
        WatchMode::Optional => "optional",
        WatchMode::Required => "required",
        WatchMode::Wait => "wait",
    };
    db.execute(
        "INSERT INTO external_watches (id, issue_id, task_id, url, condition, mode) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, issue, task, condition.url, serde_json::to_string(condition)?, mode_name],
    )?;
    let watch = Watch {
        id: id.to_owned(),
        task: task.to_owned(),
        condition: condition.clone(),
        mode,
    };
    let mut statement = db.prepare(
        "SELECT f.id, f.fact, f.error FROM external_latest l JOIN external_facts f ON f.id = l.fact_id
         JOIN pulls p ON p.url = l.url AND l.url = ?1
         WHERE f.error IS NOT NULL OR (p.observation_error IS NULL
           AND (?2 IS NOT NULL OR json_extract(f.fact, '$.head_sha') = json_extract(p.facts, '$.head_sha')))
         ORDER BY f.rowid DESC",
    )?;
    for saved in statement.query_map(params![condition.url, condition.head_sha], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })? {
        let (fact_id, data, error) = saved?;
        let fact = serde_json::from_str(&data)?;
        if matches(&watch, &fact, error.as_deref(), false) {
            queue(db, &watch, &fact_id)?;
            if mode == WatchMode::Wait {
                return Ok(());
            }
        }
    }
    // A watch on an older head or on a completed PR can still require an observation.
    db.execute(
        "UPDATE pulls SET due_at = unixepoch() WHERE url = ?1
         AND (due_at IS NULL OR due_at > unixepoch())",
        [&condition.url],
    )?;
    Ok(())
}

pub fn clear_waits(db: &Connection, task: &str) -> Result<()> {
    db.execute(
        "UPDATE external_watches SET state = 'withdrawn'
         WHERE task_id = ?1 AND mode = 'wait' AND state IN ('active','matched')",
        [task],
    )?;
    Ok(())
}

fn matches(watch: &Watch, fact: &ExternalFact, error: Option<&str>, pinned_only: bool) -> bool {
    if error.is_some() {
        watch.mode == WatchMode::Wait
    } else {
        (!pinned_only || watch.condition.head_sha.is_some()) && watch.condition.matches(fact)
    }
}

fn queue(db: &Connection, watch: &Watch, fact: &str) -> Result<()> {
    queue_delivery(db, &watch.id, &watch.task, fact)?;
    if watch.mode == WatchMode::Wait {
        db.execute(
            "UPDATE external_watches SET state = 'matched' WHERE id = ?1",
            [&watch.id],
        )?;
    }
    Ok(())
}

fn queue_delivery(db: &Connection, watch: &str, task: &str, fact: &str) -> Result<()> {
    if db.execute(
        "INSERT INTO external_deliveries(id, watch_id, fact_id) VALUES (?1, ?2, ?3) ON CONFLICT DO NOTHING",
        params![protocol::new_id(), watch, fact],
    )? == 0 {
        return Ok(());
    }
    db.execute(
        "INSERT INTO external_batches(task_id, due_at) VALUES (?1, unixepoch() + 300) ON CONFLICT DO NOTHING",
        [task],
    )?;
    Ok(())
}

pub(super) fn queue_required(
    db: &Connection,
    route: &str,
    fact: &str,
    url: &str,
    issue: &str,
    origin: &str,
    task: &str,
) -> Result<()> {
    db.execute(
        "INSERT INTO external_watches(id, issue_id, task_id, url, condition, mode, state, route_id, origin_issue)
         SELECT ?2, ?3, ?4, ?5, condition, 'required', 'routed', id, ?6 FROM external_routes WHERE id = ?1
         ON CONFLICT DO NOTHING",
        params![route, protocol::new_id(), issue, task, url, origin],
    )?;
    let watch: String = db.query_row(
        "SELECT id FROM external_watches WHERE route_id = ?1 AND task_id = ?2",
        params![route, task],
        |row| row.get(0),
    )?;
    queue_delivery(db, &watch, task, fact)
}

pub fn observe(
    db: &Connection,
    pull: &GitHubPull,
    facts: Result<&PullFacts, &'static str>,
    checks: &[ExternalFact],
) -> Result<()> {
    let url = pull.url();
    let summary = match facts {
        Ok(facts) => facts,
        Err(error) if matches!(error, "not_accessible" | "github_cli_unavailable") => {
            return save_fact(
                db,
                "unavailable",
                &ExternalFact {
                    url,
                    event: ExternalKind::Pull,
                    state: "unavailable".to_owned(),
                    head_sha: None,
                    check: None,
                    run_id: None,
                },
                Some(error),
                false,
            );
        }
        Err(_) => return Ok(()),
    };
    db.execute(
        "DELETE FROM external_latest WHERE url = ?1 AND stream = 'unavailable'",
        [&url],
    )?;
    let mut fact = ExternalFact {
        url,
        event: ExternalKind::Pull,
        state: summary.state.clone(),
        head_sha: Some(summary.head_sha.clone()),
        check: None,
        run_id: None,
    };
    save_fact(db, "pull", &fact, None, false)?;
    let current_ci_stream = format!("ci:{}", summary.head_sha);
    if let Some(ci) = &summary.ci {
        fact.event = ExternalKind::Ci;
        fact.state.clone_from(ci);
        save_fact(db, &current_ci_stream, &fact, None, false)?;
    } else {
        db.execute(
            "DELETE FROM external_latest WHERE url = ?1 AND stream = ?2",
            params![fact.url, current_ci_stream],
        )?;
    }
    for check in checks {
        let stream = match (&check.head_sha, &check.check) {
            (Some(sha), None) => format!("ci:{sha}"),
            _ => serde_json::to_string(&(&check.head_sha, &check.check, &check.run_id))?,
        };
        save_fact(
            db,
            &stream,
            check,
            None,
            check.head_sha.as_deref() != Some(&summary.head_sha),
        )?;
    }
    Ok(())
}

fn save_fact(
    db: &Connection,
    stream: &str,
    fact: &ExternalFact,
    error: Option<&str>,
    pinned_only: bool,
) -> Result<()> {
    let data = serde_json::to_string(fact)?;
    let previous = db.query_row(
        "SELECT f.fact, f.error FROM external_latest l JOIN external_facts f ON f.id = l.fact_id WHERE l.url = ?1 AND l.stream = ?2",
        params![fact.url, stream], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
    ).optional()?;
    if previous.is_some_and(|(value, reason)| value == data && reason.as_deref() == error) {
        return Ok(());
    }
    let id = protocol::new_id();
    db.execute(
        "INSERT INTO external_facts (id, url, fact, error) VALUES (?1, ?2, ?3, ?4)",
        params![id, fact.url, data, error],
    )?;
    db.execute(
        "INSERT INTO external_latest(url, stream, fact_id) VALUES (?1, ?2, ?3)
         ON CONFLICT(url, stream) DO UPDATE SET fact_id = excluded.fact_id",
        params![fact.url, stream, id],
    )?;
    let mut statement = db.prepare("SELECT id, task_id, condition, mode FROM external_watches WHERE url = ?1 AND state = 'active'")?;
    let rows = statement.query_map([&fact.url], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    let rows = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    for (watch_id, task, condition, mode) in rows {
        let watch = Watch {
            id: watch_id,
            task,
            condition: serde_json::from_str(&condition)?,
            mode: watch_mode(&mode),
        };
        if matches(&watch, fact, error, pinned_only) {
            queue(db, &watch, &id)?;
        }
    }
    if error.is_none() {
        super::external_routing::observe(db, &id, fact, pinned_only)?;
    }
    Ok(())
}

fn watch_mode(mode: &str) -> WatchMode {
    match mode {
        "optional" => WatchMode::Optional,
        "required" => WatchMode::Required,
        _ => WatchMode::Wait,
    }
}

impl Store {
    pub fn release_external(&mut self) -> Result<()> {
        let Some(task) = self.db.query_row(
            "SELECT task_id FROM external_batches WHERE due_at <= unixepoch() ORDER BY due_at LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        ).optional()? else { return Ok(()) };
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let deliveries = {
            let mut statement = tx.prepare(
                "SELECT d.id, coalesce(w.route_id, w.id), w.issue_id, w.mode, w.state, f.id, f.fact, f.error,
                        t.status, w.origin_issue, m.chat_id
                 FROM external_deliveries d JOIN external_watches w ON w.id = d.watch_id
                 JOIN external_facts f ON f.id = d.fact_id JOIN tasks t ON t.id = w.task_id
                 LEFT JOIN external_routes r ON r.id = w.route_id
                 LEFT JOIN chat_messages m ON m.seq = r.message_seq
                 WHERE w.task_id = ?1 AND d.released_at IS NULL ORDER BY d.rowid",
            )?;
            statement
                .query_map([&task], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, Option<String>>(9)?,
                        row.get::<_, Option<String>>(10)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (id, watch_id, issue, mode, state, fact_id, raw, error, task_state, origin, chat) in
            deliveries
        {
            let mode = watch_mode(&mode);
            let target = if state == "withdrawn" || task_state == "cancelled" {
                None
            } else if matches!(task_state.as_str(), "completed" | "failed") {
                if mode == WatchMode::Wait {
                    None
                } else {
                    coordination::target_task(&tx, &issue)?
                }
            } else {
                Some(task.clone())
            };
            if let Some(target) = target {
                let fact: ExternalFact = serde_json::from_str(&raw)?;
                let summary = error.as_ref().map_or_else(
                    || format!("GitHub observed {} on {}", fact.state, fact.url),
                    |error| format!("GitHub observation unavailable for {} ({error})", fact.url),
                );
                coordination::enqueue(
                    &tx,
                    &target,
                    IssueInput {
                        input_id: id.clone(),
                        child_issue_id: origin.unwrap_or(issue),
                        child_task_id: task.clone(),
                        summary,
                        cause: IssueInputCause::ExternalFact {
                            watch_id,
                            fact_id,
                            mode,
                            fact,
                            error,
                        },
                    },
                )?;
            } else if let Some(chat) = chat {
                super::external_routing::queue_router(
                    &tx,
                    &watch_id,
                    &fact_id,
                    &chat,
                    origin.as_deref(),
                    Some(0),
                )?;
            }
            tx.execute(
                "UPDATE external_deliveries SET released_at = unixepoch() WHERE id = ?1",
                [&id],
            )?;
        }
        tx.execute("DELETE FROM external_batches WHERE task_id = ?1", [&task])?;
        super::board::commit(tx)?;
        Ok(())
    }

    pub fn external_watches(&self, issue: &str) -> Result<Vec<Value>> {
        let mut statement = self.db.prepare(
            "SELECT id, task_id, mode, state, condition FROM external_watches WHERE issue_id = ?1 ORDER BY rowid DESC LIMIT 50",
        )?;
        statement
            .query_map([issue], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?
            .map(|row| {
                let (id, task, mode, state, condition) = row?;
                Ok(json!({"id":id,"task_id":task,"mode":mode,"state":state,
                "condition":serde_json::from_str::<Value>(&condition)?}))
            })
            .collect()
    }
}
