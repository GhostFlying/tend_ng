use super::{Store, control::ResultState};
use anyhow::Result;
use protocol::{WorkspaceReclaim, WorkspaceReclaimReceipt};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::{Value, json};

pub fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE workspace_reclaims (
            id TEXT PRIMARY KEY, issue_id TEXT NOT NULL REFERENCES issues(id),
            runner_id TEXT NOT NULL REFERENCES runners(id), keys TEXT NOT NULL, receipt TEXT
         );
         CREATE INDEX reclaim_runner ON workspace_reclaims(runner_id) WHERE receipt IS NULL;
         CREATE UNIQUE INDEX reclaim_issue ON workspace_reclaims(issue_id) WHERE receipt IS NULL;",
    )?;
    Ok(())
}

pub fn pending(db: &Connection, runner: &str) -> Result<Vec<WorkspaceReclaim>> {
    let mut statement = db.prepare(
        "SELECT id, issue_id, keys FROM workspace_reclaims
                                 WHERE runner_id = ?1 AND receipt IS NULL ORDER BY rowid LIMIT ?2",
    )?;
    statement
        .query_map(params![runner, protocol::RUNNER_SYNC_BATCH], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get::<_, String>(2)?))
        })?
        .map(|row| {
            let (id, issue_id, raw) = row?;
            Ok(WorkspaceReclaim {
                id,
                issue_id,
                keys: serde_json::from_str(&raw)?,
            })
        })
        .collect()
}

pub fn accept(db: &Connection, runner: &str, receipt: &WorkspaceReclaimReceipt) -> Result<bool> {
    let raw = serde_json::to_string(receipt)?;
    Ok(db.execute(
        "UPDATE workspace_reclaims SET receipt = ?3 WHERE id = ?1 AND runner_id = ?2
                                     AND (receipt IS NULL OR receipt = ?3)",
        params![receipt.id, runner, raw],
    )? == 1)
}

impl Store {
    pub fn reclaim_workspace(&mut self, issue: &str, id: &str) -> Result<ResultState<Value>> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let prior: Option<String> = tx
            .query_row(
                "SELECT issue_id FROM workspace_reclaims WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(prior) = prior {
            return Ok(if prior == issue {
                ResultState::Found(json!({"operation_id":id, "issue_id":issue}))
            } else {
                ResultState::Conflict
            });
        }
        let target = tx.query_row(
            "SELECT i.state, coalesce(h.runner_id, (
                SELECT a.runner_id FROM tasks t JOIN runner_assignments a ON a.id = t.current_assignment
                WHERE t.issue_id = i.id ORDER BY t.rowid DESC LIMIT 1))
             FROM issues i LEFT JOIN handoff_roots h ON h.root_id = i.id
             WHERE i.id = ?1 AND i.parent_issue_id IS NULL",
            [issue], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        ).optional()?;
        let Some((state, Some(runner))) = target else {
            return Ok(ResultState::Missing);
        };
        if state == "open" || tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM workspace_reclaims WHERE issue_id = ?1 AND receipt IS NULL) OR EXISTS(
                SELECT 1 FROM tasks t JOIN issues i ON i.id = t.issue_id
                WHERE (i.id = ?1 OR i.parent_issue_id = ?1) AND t.status NOT IN ('completed','failed','cancelled'))",
            [issue], |row| row.get::<_, bool>(0),
        )? { return Ok(ResultState::Conflict) }
        let mut keys = vec![issue.to_owned()];
        {
            let mut query = tx.prepare(
                "SELECT DISTINCT t.workspace_key FROM tasks t JOIN issues i ON i.id = t.issue_id
                 WHERE i.parent_issue_id = ?1 AND t.workspace_key IS NOT NULL ORDER BY t.workspace_key",
            )?;
            keys.extend(
                query
                    .query_map([issue], |row| row.get(0))?
                    .collect::<rusqlite::Result<Vec<String>>>()?,
            );
        }
        tx.execute(
            "INSERT INTO workspace_reclaims(id,issue_id,runner_id,keys) VALUES (?1,?2,?3,?4)",
            params![id, issue, runner, serde_json::to_string(&keys)?],
        )?;
        tx.commit()?;
        Ok(ResultState::Found(
            json!({"operation_id":id, "issue_id":issue}),
        ))
    }

    pub fn workspace_reclaim(&self, id: &str) -> Result<Option<Value>> {
        let row = self
            .db
            .query_row(
                "SELECT issue_id, runner_id, receipt FROM workspace_reclaims WHERE id = ?1",
                [id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?;
        row.map(|(issue, runner, result)| {
            let result = result.as_deref().map(serde_json::from_str::<WorkspaceReclaimReceipt>).transpose()?;
            let state = match &result {
                Some(result) if result.error.is_some() => "failed",
                Some(_) => "reclaimed",
                None => "pending",
            };
            Ok(json!({"operation_id":id,"issue_id":issue,"runner_id":runner,"state":state,"result":result}))
        }).transpose()
    }
}
