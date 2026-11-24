use super::Store;
use anyhow::Result;
use protocol::{HandoffPhase, HandoffProof, RunnerProfile};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::{Value, json};

pub fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(
        "ALTER TABLE runners ADD COLUMN protocol INTEGER NOT NULL DEFAULT 1;
         ALTER TABLE tasks ADD COLUMN handoff TEXT;
         CREATE TABLE runner_replacements (
            predecessor TEXT PRIMARY KEY REFERENCES runners(id),
            successor TEXT NOT NULL REFERENCES runners(id), drained_at INTEGER, retired_at INTEGER
         );
         CREATE INDEX replacement_successor ON runner_replacements(successor);
         CREATE TABLE handoff_roots (root_id TEXT PRIMARY KEY REFERENCES issues(id), runner_id TEXT NOT NULL REFERENCES runners(id));
         CREATE TABLE runner_handoff_proofs (id TEXT PRIMARY KEY, predecessor TEXT NOT NULL, proof TEXT NOT NULL);",
    )?;
    Ok(())
}

pub fn drain_target(db: &Connection, runner: &str) -> Result<Option<String>> {
    Ok(db.query_row("SELECT successor FROM runner_replacements WHERE predecessor = ?1 AND retired_at IS NULL",
                    [runner], |row| row.get(0)).optional()?)
}

pub fn note_drained(db: &Connection, runner: &str) -> Result<()> {
    db.execute(
        "UPDATE runner_replacements SET drained_at = unixepoch()
         WHERE predecessor = ?1 AND drained_at IS NULL
           AND NOT EXISTS(SELECT 1 FROM artifacts WHERE runner_id = ?1 AND available = 0)
           AND NOT EXISTS(SELECT 1 FROM workspace_reclaims WHERE runner_id = ?1 AND receipt IS NULL)
           AND NOT EXISTS(SELECT 1 FROM tasks t JOIN runner_assignments a ON a.id = t.current_assignment
                  WHERE a.runner_id = ?1 AND t.status NOT IN ('completed','failed','cancelled'))",
        [runner],
    )?;
    db.execute(
        "UPDATE handoff_roots SET runner_id = (SELECT successor FROM runner_replacements WHERE predecessor = ?1)
         WHERE runner_id = ?1 AND EXISTS(
             SELECT 1 FROM runner_replacements WHERE predecessor = ?1 AND drained_at IS NOT NULL
         )",
        [runner],
    )?;
    Ok(())
}

pub fn apply(db: &Connection, runner: &str, proof: &HandoffProof) -> Result<bool> {
    let raw = serde_json::to_string(proof)?;
    let prior = db
        .query_row(
            "SELECT predecessor, proof FROM runner_handoff_proofs WHERE id = ?1",
            [&proof.id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    if let Some((predecessor, saved)) = prior {
        return Ok(predecessor == runner && saved == raw);
    }
    if drain_target(db, runner)?.as_ref() != Some(&proof.successor) {
        return Ok(false);
    }
    let original = db.query_row(
        "SELECT coalesce(i.parent_issue_id, i.id), t.status, t.current_assignment,
                json_extract(a.contract, '$.placement.agent.id')
           FROM runner_assignments a JOIN tasks t ON t.id = a.task_id JOIN issues i ON i.id = t.issue_id
           WHERE a.id = ?1 AND a.runner_id = ?2 AND a.task_id = ?3 AND a.state = 'accepted'",
        params![proof.assignment_id, runner, proof.task_id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                  row.get::<_, Option<String>>(2)?, row.get::<_, String>(3)?)),
    ).optional()?;
    let Some((root, status, current, owner)) = original else {
        return Ok(false);
    };
    if status == "running" && current.as_deref() == Some(&proof.assignment_id) {
        return Ok(false);
    }
    db.execute(
        "INSERT INTO handoff_roots VALUES (?1, ?2)
           ON CONFLICT(root_id) DO UPDATE SET runner_id = excluded.runner_id
           WHERE handoff_roots.runner_id = ?3",
        params![root, proof.successor, runner],
    )?;
    if proof.context.phase != HandoffPhase::Finished
        && current.as_deref() == Some(&proof.assignment_id)
    {
        db.execute(
            "UPDATE tasks SET current_assignment = NULL, status = 'awaiting_assignment', pinned_runner_id = ?2,
                              pinned_agent_id = ?3, handoff = ?4
             WHERE id = ?1 AND status NOT IN ('completed','failed','cancelled')",
            params![proof.task_id, proof.successor, owner, serde_json::to_string(&proof.context)?],
        )?;
    }
    db.execute(
        "INSERT INTO runner_handoff_proofs VALUES (?1, ?2, ?3)",
        params![proof.id, runner, raw],
    )?;
    Ok(true)
}

impl Store {
    pub fn replace_runner(&mut self, old: &str, new: &str) -> Result<Option<Value>> {
        if old == new {
            return Ok(None);
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let profile = |id: &str| -> Result<Option<RunnerProfile>> {
            let profile: Option<String> = tx
                .query_row(
                    "SELECT profile FROM runners WHERE id = ?1 AND last_seen > unixepoch() - 15",
                    [id],
                    |row| row.get(0),
                )
                .optional()?;
            Ok(profile.as_deref().map(serde_json::from_str).transpose()?)
        };
        let existing = drain_target(&tx, old)?;
        if existing.is_some() {
            return Ok((existing.as_deref() == Some(new))
                .then(|| json!({"predecessor":old,"successor":new})));
        }
        let (Some(predecessor), Some(successor)) = (profile(old)?, profile(new)?) else {
            return Ok(None);
        };
        if predecessor.host_id != successor.host_id
            || predecessor.workspace_root.is_none()
            || predecessor.workspace_root != successor.workspace_root
            || drain_target(&tx, new)?.is_some()
        {
            return Ok(None);
        }
        tx.execute(
            "INSERT INTO runner_replacements(predecessor, successor) VALUES (?1, ?2)",
            params![old, new],
        )?;
        tx.execute(
            "UPDATE tasks SET current_assignment = NULL, status = 'awaiting_assignment'
             WHERE current_assignment IN (SELECT id FROM runner_assignments WHERE runner_id = ?1 AND state = 'reserved')",
            [old],
        )?;
        tx.execute("UPDATE runner_assignments SET state = 'revoked' WHERE runner_id = ?1 AND state = 'reserved'", [old])?;
        tx.commit()?;
        Ok(Some(json!({"predecessor":old,"successor":new})))
    }

    pub fn runner_instances(&self) -> Result<Vec<Value>> {
        let mut query = self.db.prepare(
            "SELECT r.profile, r.protocol, r.last_seen, p.successor, p.drained_at, p.retired_at
             FROM runners r LEFT JOIN runner_replacements p ON p.predecessor = r.id ORDER BY r.host_id, r.id",
        )?;
        query.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?, row.get::<_, i64>(2)?,
            row.get::<_, Option<String>>(3)?, row.get::<_, Option<i64>>(4)?, row.get::<_, Option<i64>>(5)?)))?
            .map(|entry| {
                let (profile, protocol, last_seen, successor, drained_at, retired_at) = entry?;
                Ok(json!({"profile":serde_json::from_str::<Value>(&profile)?, "protocol":protocol,
                          "last_seen":last_seen,"successor":successor,"drained_at":drained_at,"retired_at":retired_at}))
            }).collect()
    }

    pub fn retire_runner(&self, runner: &str) -> Result<bool> {
        Ok(self.db.execute(
            "UPDATE runner_replacements SET retired_at = coalesce(retired_at, unixepoch())
               WHERE predecessor = ?1 AND drained_at IS NOT NULL",
            [runner],
        )? == 1)
    }

    pub fn upgrade_blockers(&self, target: u32) -> Result<Vec<Value>> {
        let mut query = self.db.prepare(
            "SELECT r.id, r.protocol FROM runners r LEFT JOIN runner_replacements p ON p.predecessor = r.id
             WHERE p.retired_at IS NULL AND (r.protocol < ?1 OR r.protocol > ?2) ORDER BY r.id",
        )?;
        Ok(query.query_map(params![target.saturating_sub(1).max(1), target], |row| {
            Ok(json!({"runner_id": row.get::<_, String>(0)?, "protocol": row.get::<_, u32>(1)?}))
        })?.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}
