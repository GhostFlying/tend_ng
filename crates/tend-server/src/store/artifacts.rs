use super::{Store, coordination};
use anyhow::Result;
use protocol::{Artifact, IssueInput, IssueInputCause, RUNNER_SYNC_BATCH};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

pub fn create_tables(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE artifacts (
             id TEXT PRIMARY KEY REFERENCES runner_operations(id),
             issue_id TEXT NOT NULL REFERENCES issues(id),
             task_id TEXT NOT NULL REFERENCES tasks(id),
             runner_id TEXT NOT NULL REFERENCES runners(id),
             name TEXT NOT NULL,
             bytes INTEGER NOT NULL,
             available INTEGER NOT NULL DEFAULT 0,
             published INTEGER NOT NULL DEFAULT 0
         );
         CREATE INDEX artifacts_by_issue ON artifacts(issue_id);
         CREATE INDEX pending_artifacts ON artifacts(runner_id) WHERE available = 0;",
    )?;
    Ok(())
}

pub fn pending(db: &Connection, runner: &str, task: Option<&str>) -> Result<Vec<String>> {
    let mut statement = db.prepare(
        "SELECT id FROM artifacts WHERE runner_id = ?1 AND available = 0
         AND (?3 IS NULL OR task_id = ?3) ORDER BY rowid LIMIT ?2",
    )?;
    Ok(statement
        .query_map(params![runner, RUNNER_SYNC_BATCH, task], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

pub(super) fn artifact(row: &rusqlite::Row<'_>) -> rusqlite::Result<Artifact> {
    Ok(Artifact {
        id: row.get(0)?,
        name: row.get(1)?,
        bytes: row.get(2)?,
        available: row.get(3)?,
    })
}

impl Store {
    pub fn reconcile_artifacts(
        &self,
        root: &std::path::Path,
        task: &str,
        runner: &str,
    ) -> Result<()> {
        let mut query = self.db.prepare(
            "SELECT id FROM artifacts WHERE task_id = ?1 AND runner_id = ?2 AND available = 1",
        )?;
        let files = query
            .query_map(params![task, runner], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for id in files {
            let path = root.join(&id);
            if !path.is_file() {
                self.mark_artifact_missing(&id, &path)?;
            }
        }
        Ok(())
    }

    pub fn issue_artifacts(&self, issue: &str) -> Result<Vec<Artifact>> {
        let mut statement = self.db.prepare(
            "SELECT id, name, bytes, available FROM artifacts WHERE issue_id = ?1 ORDER BY rowid",
        )?;
        Ok(statement
            .query_map([issue], artifact)?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn artifact(&self, id: &str, runner: Option<&str>) -> Result<Option<Artifact>> {
        Ok(self.db.query_row(
            "SELECT id, name, bytes, available FROM artifacts WHERE id = ?1 AND (?2 IS NULL OR runner_id = ?2)",
            params![id, runner], artifact,
        ).optional()?)
    }

    pub fn mark_artifact_missing(&self, id: &str, path: &std::path::Path) -> Result<()> {
        // Check again inside the short write transaction in case an upload just restored it.
        let tx = self.db.unchecked_transaction()?;
        tx.execute(
            "UPDATE artifacts SET available = 0 WHERE id = ?1 AND available = 1",
            [id],
        )?;
        if !path.is_file() {
            tx.commit()?;
        }
        Ok(())
    }

    pub fn finish_artifact(&mut self, artifact: Artifact) -> Result<()> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE artifacts SET available = 1 WHERE id = ?1 AND available = 0",
            [&artifact.id],
        )?;
        if tx.execute(
            "UPDATE artifacts SET published = 1 WHERE id = ?1 AND published = 0",
            [&artifact.id],
        )? != 0
        {
            let (issue, task, parent): (String, String, Option<String>) = tx.query_row(
                "SELECT a.issue_id, a.task_id, i.parent_issue_id FROM artifacts a
                 JOIN issues i ON i.id = a.issue_id WHERE a.id = ?1",
                [&artifact.id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            if let Some(parent) = parent
                && let Some(target) = coordination::latest_task(&tx, &parent)?
            {
                coordination::enqueue(
                    &tx,
                    &target,
                    IssueInput {
                        input_id: artifact.id.clone(),
                        child_issue_id: issue,
                        child_task_id: task,
                        summary: format!("{} is ready to download.", artifact.name),
                        cause: IssueInputCause::ArtifactAvailable {
                            artifact: Artifact {
                                available: true,
                                ..artifact
                            },
                        },
                    },
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}
