use super::{Store, run::decode_record};
use anyhow::{Result, ensure};
use protocol::{ClaimReceipt, RUNNER_SYNC_BATCH, RunnerRecord};
use rusqlite::params;

impl Store {
    pub fn reconciliation_tasks(&self) -> Result<Vec<String>> {
        let mut statement = self.0.prepare(
            "SELECT task_id FROM received_assignments
             WHERE execution_state IN ('queued', 'active', 'waiting', 'waiting_external') OR acknowledged = 0
             UNION
             SELECT a.task_id FROM run_outbox r
             JOIN received_assignments a ON a.id = r.assignment_id WHERE r.acknowledged = 0",
        )?;
        Ok(statement
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn reconcile_bounds(&self, task: &str) -> Result<(i64, i64)> {
        let bounds: (i64, i64) = self.0.query_row(
            "SELECT coalesce(max(a.rowid), 0), coalesce(max(r.rowid), 0)
             FROM received_assignments a LEFT JOIN run_outbox r ON r.assignment_id = a.id
             WHERE a.task_id = ?1",
            [task],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        ensure!(bounds.0 != 0, "Task {task} is unknown to this Runner");
        Ok(bounds)
    }

    pub fn reconcile_receipts(
        &self,
        task: &str,
        after: i64,
        through: i64,
    ) -> Result<Vec<(i64, ClaimReceipt)>> {
        let mut statement = self.0.prepare(
            "SELECT rowid, id, decision FROM received_assignments WHERE task_id = ?1
             AND rowid > ?2 AND rowid <= ?3 ORDER BY rowid LIMIT ?4",
        )?;
        statement
            .query_map(params![task, after, through, RUNNER_SYNC_BATCH], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get::<_, String>(2)?))
            })?
            .map(|row| {
                let (seq, assignment_id, decision) = row?;
                Ok((
                    seq,
                    ClaimReceipt {
                        assignment_id,
                        decision: serde_json::from_str(&decision)?,
                    },
                ))
            })
            .collect()
    }

    pub fn reconcile_records(
        &self,
        task: &str,
        after: i64,
        through: i64,
    ) -> Result<Vec<(i64, RunnerRecord)>> {
        let mut statement = self.0.prepare(
            "SELECT r.rowid, r.observation, r.record_kind FROM run_outbox r
             JOIN received_assignments a ON a.id = r.assignment_id WHERE a.task_id = ?1
             AND r.rowid > ?2 AND r.rowid <= ?3 ORDER BY r.rowid LIMIT ?4",
        )?;
        statement
            .query_map(params![task, after, through, RUNNER_SYNC_BATCH], |row| {
                Ok((
                    row.get(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .map(|row| {
                let (seq, payload, kind) = row?;
                Ok((seq, decode_record(&payload, &kind)?))
            })
            .collect()
    }
}
