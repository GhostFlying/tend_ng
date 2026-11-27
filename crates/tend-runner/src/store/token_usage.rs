use super::Store;
use anyhow::Result;
use protocol::{RUNNER_SYNC_BATCH, RequestMeter, RunTokenUsage, TokenCounts};
use rusqlite::{Connection, Params, params};

pub(super) fn create_tables(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS local_token_usage (
            id TEXT NOT NULL PRIMARY KEY,
            run_id TEXT NOT NULL REFERENCES local_runs(id),
            assignment_id TEXT NOT NULL REFERENCES received_assignments(id),
            thread_id TEXT NOT NULL,
            turn_id TEXT NOT NULL,
            response_id TEXT NOT NULL,
            last TEXT NOT NULL,
            observed_at INTEGER NOT NULL DEFAULT (unixepoch()),
            acknowledged INTEGER NOT NULL DEFAULT 0,
            UNIQUE(run_id, response_id, last)
         );
         CREATE INDEX IF NOT EXISTS unsent_token_usage
            ON local_token_usage(acknowledged) WHERE acknowledged = 0;
         CREATE INDEX IF NOT EXISTS token_usage_by_assignment ON local_token_usage(assignment_id);",
    )?;
    Ok(())
}

impl Store {
    pub fn persist_token_usage(
        &self,
        run: &str,
        thread: &str,
        turn: &str,
        response_id: &str,
        last: RequestMeter,
    ) -> Result<()> {
        self.0.execute(
            "INSERT INTO local_token_usage (id, run_id, assignment_id, thread_id, turn_id, response_id, last)
             SELECT ?1, id, assignment_id, thread_id, turn_id, ?5, ?6
             FROM local_runs WHERE id = ?2 AND thread_id = ?3 AND turn_id = ?4 AND state = 'running'
             ON CONFLICT(run_id, response_id, last) DO NOTHING",
            params![protocol::new_id(), run, thread, turn, response_id, serde_json::to_string(&last)?],
        )?;
        Ok(())
    }

    pub fn token_usage(&self) -> Result<Vec<RunTokenUsage>> {
        Ok(self
            .token_usage_page(
                "WHERE u.acknowledged = 0 AND (a.acknowledged = 1 OR a.id IN (
                     SELECT id FROM received_assignments WHERE acknowledged = 0 ORDER BY rowid LIMIT ?1
                 )) ORDER BY u.rowid LIMIT ?1",
                [RUNNER_SYNC_BATCH],
            )?
            .into_iter()
            .map(|(_, usage)| usage)
            .collect())
    }

    pub fn reconcile_token_usage_end(&self, task: &str) -> Result<i64> {
        Ok(self.0.query_row(
            "SELECT coalesce(max(u.rowid), 0) FROM received_assignments a
             JOIN local_token_usage u ON u.assignment_id = a.id WHERE a.task_id = ?1",
            [task],
            |row| row.get(0),
        )?)
    }

    pub fn reconcile_token_usage(
        &self,
        task: &str,
        after: i64,
        through: i64,
    ) -> Result<Vec<(i64, RunTokenUsage)>> {
        self.token_usage_page(
            "WHERE a.task_id = ?1 AND u.rowid > ?2 AND u.rowid <= ?3 ORDER BY u.rowid LIMIT ?4",
            params![task, after, through, RUNNER_SYNC_BATCH],
        )
    }

    fn token_usage_page(
        &self,
        filter: &str,
        params: impl Params,
    ) -> Result<Vec<(i64, RunTokenUsage)>> {
        let sql = format!(
            "SELECT u.rowid, u.id, u.assignment_id, a.task_id, u.run_id, u.thread_id, u.turn_id,
                    u.response_id, u.observed_at, u.last
             FROM local_token_usage u JOIN received_assignments a ON a.id = u.assignment_id {filter}"
        );
        let mut query = self.0.prepare(&sql)?;
        query
            .query_map(params, |row| {
                Ok((
                    row.get(0)?,
                    RunTokenUsage {
                        event_id: row.get(1)?,
                        assignment_id: row.get(2)?,
                        task_id: row.get(3)?,
                        run_id: row.get(4)?,
                        thread_id: row.get(5)?,
                        turn_id: row.get(6)?,
                        response_id: row.get(7)?,
                        observed_at: row.get(8)?,
                        last: TokenCounts::default(),
                        model: None,
                    },
                    row.get::<_, String>(9)?,
                ))
            })?
            .map(|row| {
                let (seq, mut usage, last) = row?;
                let meter: RequestMeter = serde_json::from_str(&last)?;
                usage.last = meter.counts;
                usage.model = meter.model;
                Ok((seq, usage))
            })
            .collect()
    }
}
