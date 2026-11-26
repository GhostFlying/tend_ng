use super::{Store, coordination};
use anyhow::Result;
use protocol::{Artifact, IssueInput, IssueInputCause, RUNNER_SYNC_BATCH};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

pub(super) fn complete_file(path: &std::path::Path, bytes: u64) -> std::io::Result<bool> {
    match path.metadata() {
        Ok(metadata) => Ok(metadata.is_file() && metadata.len() == bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

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

// Replacement registration verifies the same host and workspace root. Follow
// only the uploader's predecessor chain, including generations already retired.
const PRODUCERS: &str = "WITH RECURSIVE producers(id) AS (
    SELECT ?1
    UNION SELECT r.predecessor FROM runner_replacements r JOIN producers p ON r.successor = p.id
    WHERE ?2
)";

pub fn pending(
    db: &Connection,
    runner: &str,
    repair_host_artifacts: bool,
    task: Option<&str>,
) -> Result<Vec<String>> {
    let mut statement = db.prepare_cached(&format!(
        "{PRODUCERS} SELECT id FROM artifacts WHERE runner_id IN producers AND available = 0
         AND (?3 IS NULL OR task_id = ?3) ORDER BY runner_id != ?1, rowid LIMIT ?4"
    ))?;
    Ok(statement
        .query_map(
            params![runner, repair_host_artifacts, task, RUNNER_SYNC_BATCH],
            |row| row.get(0),
        )?
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
    pub(super) fn has_repairable_artifacts(&self, task: &str, runner: &str) -> Result<bool> {
        Ok(self.db.query_row(
            &format!(
                "{PRODUCERS} SELECT EXISTS(
                SELECT 1 FROM artifacts WHERE task_id = ?3 AND runner_id IN producers)"
            ),
            params![runner, true, task],
            |row| row.get(0),
        )?)
    }

    pub fn reconcile_artifacts(
        &self,
        root: &std::path::Path,
        task: &str,
        runner: &str,
        repair_host_artifacts: bool,
    ) -> Result<()> {
        let mut query = self.db.prepare_cached(&format!(
            "{PRODUCERS} SELECT id FROM artifacts WHERE task_id = ?3 AND runner_id IN producers AND available = 1"
        ))?;
        let files = query
            .query_map(params![runner, repair_host_artifacts, task], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for id in files {
            let path = root.join(&id);
            if !path.is_file() {
                self.mark_artifact_missing(&id, &path)?;
            }
        }
        Ok(())
    }

    pub fn audit_artifact_files(&self, root: &std::path::Path, after: i64) -> Result<i64> {
        const BATCH: usize = 64;
        let mut query = self.db.prepare_cached(
            "SELECT rowid, id, available FROM artifacts
             WHERE rowid > ?1 ORDER BY rowid LIMIT ?2",
        )?;
        let files = query
            .query_map(params![after, BATCH], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, bool>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (_, id, available) in &files {
            if *available {
                let path = root.join(id);
                if !path.is_file() {
                    self.mark_artifact_missing(id, &path)?;
                }
            }
        }
        Ok(if files.len() == BATCH {
            files.last().expect("full artifact page").0
        } else {
            0
        })
    }

    pub fn issue_artifacts(&self, issue: &str) -> Result<Vec<Artifact>> {
        let mut statement = self.db.prepare(
            "SELECT id, name, bytes, available FROM artifacts WHERE issue_id = ?1 ORDER BY rowid",
        )?;
        Ok(statement
            .query_map([issue], artifact)?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn artifact(&self, id: &str) -> Result<Option<Artifact>> {
        Ok(self
            .db
            .query_row(
                "SELECT id, name, bytes, available FROM artifacts WHERE id = ?1",
                [id],
                artifact,
            )
            .optional()?)
    }

    pub fn artifact_for_upload(&self, runner: &str, id: &str) -> Result<Option<Artifact>> {
        Ok(self
            .db
            .query_row(
                &format!(
                    "{PRODUCERS} SELECT id, name, bytes, available FROM artifacts
                      WHERE id = ?3 AND runner_id IN producers"
                ),
                params![runner, true, id],
                artifact,
            )
            .optional()?)
    }

    pub fn mark_artifact_missing(&self, id: &str, path: &std::path::Path) -> Result<()> {
        // Check again inside the short write transaction in case an upload just restored it.
        let tx = self.db.unchecked_transaction()?;
        let bytes: Option<u64> = tx.query_row(
            "UPDATE artifacts SET available = 0 WHERE id = ?1 AND available = 1 RETURNING bytes",
            [id], |row| row.get(0),
        ).optional()?;
        if let Some(bytes) = bytes
            && !complete_file(path, bytes)?
        {
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
        super::board::commit(tx)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repair_follows_the_registered_generations_without_transferring_ownership() -> Result<()> {
        let store = Store {
            db: Connection::open_in_memory()?,
        };
        store.db.execute_batch(
            "CREATE TABLE runner_replacements (predecessor TEXT PRIMARY KEY, successor TEXT);
             CREATE INDEX replacement_successor ON runner_replacements(successor);
             INSERT INTO runner_replacements VALUES ('producer','second'), ('second','third');
             CREATE TABLE artifacts (
                 id TEXT PRIMARY KEY, runner_id TEXT, task_id TEXT, name TEXT, bytes INTEGER,
                 available INTEGER
             );
             INSERT INTO artifacts VALUES
                 ('guide','producer','completed','guide.md',14,0),
                 ('current','third','new','file.md',8,0),
                 ('elsewhere','another-host','unrelated','other.md',4,0);",
        )?;
        assert_eq!(
            pending(&store.db, "third", true, None)?,
            ["current", "guide"]
        );
        assert_eq!(pending(&store.db, "third", false, None)?, ["current"]);
        assert_eq!(
            pending(&store.db, "third", true, Some("completed"))?,
            ["guide"]
        );
        assert!(store.has_repairable_artifacts("completed", "third")?);
        assert!(store.artifact_for_upload("third", "guide")?.is_some());
        assert!(store.artifact_for_upload("producer", "current")?.is_none());
        for runner in ["unrelated-local-runner", "another-host"] {
            assert!(!store.has_repairable_artifacts("completed", runner)?);
            assert!(store.artifact_for_upload(runner, "guide")?.is_none());
        }
        store
            .db
            .execute("UPDATE artifacts SET available = 1 WHERE id = 'guide'", [])?;
        assert!(pending(&store.db, "third", true, Some("completed"))?.is_empty());
        let producer: String = store.db.query_row(
            "SELECT runner_id FROM artifacts WHERE id = 'guide'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(producer, "producer");
        Ok(())
    }
}
