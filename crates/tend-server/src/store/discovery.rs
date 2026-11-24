use super::Store;
use anyhow::Result;
use protocol::{BranchResult, FindPull, GitHubPull, PullDiscovery, RunObservation, WorkBranch};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

pub enum Accepted {
    Saved,
    Conflict,
    Missing,
}

pub struct Pending {
    pub id: String,
    pub issue: String,
    pub branches: Vec<WorkBranch>,
}

pub fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE pull_discoveries (
            operation_id TEXT PRIMARY KEY,
            issue_id TEXT NOT NULL REFERENCES issues(id),
            task_id TEXT NOT NULL,
            automatic INTEGER NOT NULL,
            state TEXT NOT NULL,
            branches TEXT NOT NULL,
            results TEXT NOT NULL DEFAULT '[]'
         );
         CREATE INDEX pull_discoveries_by_issue ON pull_discoveries(issue_id);
         CREATE UNIQUE INDEX pull_discoveries_auto ON pull_discoveries(task_id) WHERE automatic = 1;
         CREATE INDEX pull_discoveries_pending ON pull_discoveries(state) WHERE state = 'pending';",
    )?;
    Ok(())
}

fn branches(db: &Connection, issue: &str, task: &str) -> Result<Option<Vec<WorkBranch>>> {
    let observation = db
        .query_row(
            "WITH RECURSIVE tree(id) AS (
            SELECT id FROM issues WHERE id = ?1
            UNION ALL SELECT i.id FROM issues i JOIN tree p ON i.parent_issue_id = p.id
         )
         SELECT (SELECT observation FROM task_runs WHERE task_id = t.id ORDER BY rowid DESC LIMIT 1)
         FROM tasks t JOIN tree ON tree.id = t.issue_id WHERE t.id = ?2",
            params![issue, task],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?;
    observation
        .map(|value| {
            Ok(value
                .as_deref()
                .map(serde_json::from_str::<RunObservation>)
                .transpose()?
                .map(|run| run.branches)
                .unwrap_or_default())
        })
        .transpose()
}

pub fn queue(db: &Connection, issue: &str, request: &FindPull) -> Result<()> {
    let recorded = branches(db, issue, &request.task_id)?;
    let state = match &recorded {
        Some(branches) if !branches.is_empty() => "pending",
        Some(_) => "no_recorded_branch",
        None => "unrelated_task",
    };
    db.execute(
        "INSERT INTO pull_discoveries (operation_id, issue_id, task_id, automatic, state, branches)
         VALUES (?1, ?2, ?3, 0, ?4, ?5)",
        params![
            request.operation_id,
            issue,
            request.task_id,
            state,
            serde_json::to_string(&recorded.unwrap_or_default())?
        ],
    )?;
    Ok(())
}

pub fn queue_automatic(db: &Connection, observation: &RunObservation) -> Result<()> {
    if observation.branches.is_empty() {
        return Ok(());
    }
    let issue: Option<String> = db
        .query_row(
            "SELECT i.id FROM tasks t JOIN issues i ON i.id = t.issue_id
         WHERE t.id = ?1 AND i.parent_issue_id IS NOT NULL",
            [&observation.task_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(issue) = issue else { return Ok(()) };
    let mut pending = Vec::new();
    let mut known = db.prepare(
        "SELECT EXISTS(SELECT 1 FROM issue_pulls link JOIN github_pulls p ON p.url = link.url
           WHERE link.issue_id = ?1 AND p.repository = ?2
             AND json_extract(p.facts, '$.head_repository') = ?3
             AND json_extract(p.facts, '$.head_branch') = ?4)",
    )?;
    for branch in &observation.branches {
        if !known.query_row(
            params![
                issue,
                branch.repository,
                branch.head_repository,
                branch.branch
            ],
            |row| row.get::<_, bool>(0),
        )? {
            pending.push(branch);
        }
    }
    if !pending.is_empty() {
        db.execute(
            "INSERT INTO pull_discoveries (operation_id, issue_id, task_id, automatic, state, branches)
             VALUES (?1, ?2, ?3, 1, 'pending', ?4) ON CONFLICT DO NOTHING",
            params![protocol::new_id(), issue, observation.task_id, serde_json::to_string(&pending)?],
        )?;
    }
    Ok(())
}

impl Store {
    pub fn find_pull(&mut self, issue: &str, request: &FindPull) -> Result<Accepted> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some((saved_issue, saved_task, automatic)) = tx
            .query_row(
                "SELECT issue_id, task_id, automatic FROM pull_discoveries WHERE operation_id = ?1",
                [&request.operation_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, bool>(2)?,
                    ))
                },
            )
            .optional()?
        {
            return Ok(
                if issue == saved_issue && request.task_id == saved_task && !automatic {
                    Accepted::Saved
                } else {
                    Accepted::Conflict
                },
            );
        }
        if !super::pull::issue_exists(&tx, issue)? {
            return Ok(Accepted::Missing);
        }
        queue(&tx, issue, request)?;
        tx.commit()?;
        Ok(Accepted::Saved)
    }

    pub fn discoveries(
        &self,
        issue: &str,
        operation: Option<&str>,
    ) -> Result<Option<Vec<PullDiscovery>>> {
        if !super::pull::issue_exists(&self.db, issue)? {
            return Ok(None);
        }
        let mut query = self.db.prepare(
            "SELECT operation_id, task_id, automatic, state, results FROM pull_discoveries
             WHERE issue_id = ?1 AND (?3 IS NULL OR operation_id = ?3) ORDER BY rowid DESC LIMIT ?2",
        )?;
        let rows = query.query_map(params![issue, super::PAGE_SIZE, operation], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        let discoveries = rows
            .map(|row| {
                let (operation_id, task_id, automatic, state, results) = row?;
                Ok(PullDiscovery {
                    operation_id,
                    task_id,
                    automatic,
                    state,
                    results: serde_json::from_str(&results)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Some(discoveries))
    }

    pub fn pending_discovery(&self) -> Result<Option<Pending>> {
        let row = self.db.query_row(
            "SELECT operation_id, issue_id, branches FROM pull_discoveries WHERE state = 'pending' ORDER BY rowid LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get::<_, String>(2)?)),
        ).optional()?;
        row.map(|(id, issue, branches)| {
            Ok(Pending {
                id,
                issue,
                branches: serde_json::from_str(&branches)?,
            })
        })
        .transpose()
    }

    pub fn finish_discovery(&mut self, request: &Pending, results: &[BranchResult]) -> Result<()> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for result in results {
            if let Some(pull) = result.url.as_deref().and_then(GitHubPull::parse) {
                super::pull::bind(&tx, &request.issue, &pull)?;
            }
        }
        tx.execute(
            "UPDATE pull_discoveries SET state = 'completed', results = ?2 WHERE operation_id = ?1",
            params![request.id, serde_json::to_string(results)?],
        )?;
        tx.commit()?;
        Ok(())
    }
}
