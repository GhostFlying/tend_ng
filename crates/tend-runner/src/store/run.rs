use super::Store;
use anyhow::{Result, ensure};
use codex_app::ProcessIdentity;
use protocol::{RUNNER_SYNC_BATCH, RunObservation, RunOutcome, RunPhase, TaskAssignment};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

pub struct LocalRun {
    pub id: String,
    pub assignment: TaskAssignment,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub process: Option<ProcessIdentity>,
}

pub(super) fn create_tables(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS local_runs (
            id TEXT PRIMARY KEY,
            assignment_id TEXT NOT NULL UNIQUE REFERENCES received_assignments(id),
            state TEXT NOT NULL,
            thread_id TEXT,
            turn_id TEXT,
            process_pid INTEGER,
            process_start TEXT
         );
         CREATE INDEX IF NOT EXISTS local_run_state ON local_runs(state);
         CREATE TABLE IF NOT EXISTS run_outbox (
            id TEXT PRIMARY KEY,
            assignment_id TEXT NOT NULL REFERENCES received_assignments(id),
            observation TEXT NOT NULL,
            acknowledged INTEGER NOT NULL DEFAULT 0
         );
         CREATE INDEX IF NOT EXISTS unsent_runs ON run_outbox(acknowledged) WHERE acknowledged = 0;",
    )?;
    Ok(())
}

impl Store {
    pub fn next_run(&mut self) -> Result<Option<LocalRun>> {
        let active = self
            .0
            .query_row(
                "SELECT r.id, a.contract, r.thread_id, r.turn_id, r.process_pid, r.process_start
             FROM local_runs r JOIN received_assignments a ON a.id = r.assignment_id
             WHERE r.state IN ('prepared', 'running') ORDER BY r.rowid LIMIT 1",
                [],
                |row| {
                    let process = match row.get::<_, Option<u32>>(4)? {
                        Some(pid) => Some(ProcessIdentity {
                            pid,
                            started_at: row.get(5)?,
                        }),
                        None => None,
                    };
                    Ok((
                        row.get(0)?,
                        row.get::<_, String>(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        process,
                    ))
                },
            )
            .optional()?;
        if let Some((id, contract, thread_id, turn_id, process)) = active {
            return Ok(Some(LocalRun {
                id,
                assignment: serde_json::from_str(&contract)?,
                thread_id,
                turn_id,
                process,
            }));
        }

        let queued = self
            .0
            .query_row(
                "SELECT a.id, a.contract FROM received_assignments a
             WHERE json_extract(a.decision, '$.status') = 'accepted'
               AND NOT EXISTS (SELECT 1 FROM local_runs r WHERE r.assignment_id = a.id)
             ORDER BY a.rowid LIMIT 1",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let Some((assignment_id, contract)) = queued else {
            return Ok(None);
        };
        let assignment: TaskAssignment = serde_json::from_str(&contract)?;
        let id = protocol::new_id();
        let inserted = self.0.execute(
            "INSERT INTO local_runs (id, assignment_id, state) VALUES (?1, ?2, 'prepared')
             ON CONFLICT(assignment_id) DO NOTHING",
            params![id, assignment_id],
        )?;
        Ok((inserted == 1).then_some(LocalRun {
            id,
            assignment,
            thread_id: None,
            turn_id: None,
            process: None,
        }))
    }

    pub fn attach_process(
        &self,
        run_id: &str,
        thread_id: &str,
        process: &ProcessIdentity,
    ) -> Result<()> {
        ensure!(
            self.0.execute(
                "UPDATE local_runs SET thread_id = ?2, process_pid = ?3, process_start = ?4
             WHERE id = ?1 AND state = 'prepared' AND process_pid IS NULL",
                params![run_id, thread_id, process.pid, process.started_at],
            )? == 1,
            "original Run is already attached"
        );
        Ok(())
    }

    pub fn mark_started(&mut self, run: &LocalRun, turn_id: &str) -> Result<()> {
        let tx = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure!(
            tx.execute(
                "UPDATE local_runs SET state = 'running', turn_id = ?2
             WHERE id = ?1 AND state = 'prepared' AND thread_id IS NOT NULL",
                params![run.id, turn_id],
            )? == 1,
            "cannot start the original Run"
        );
        save_event(&tx, run, RunPhase::Started)?;
        tx.commit()?;
        Ok(())
    }

    pub fn finish(
        &mut self,
        run: &LocalRun,
        outcome: RunOutcome,
        error: Option<String>,
        recovered_turn: Option<&str>,
    ) -> Result<()> {
        let tx = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(turn) = recovered_turn {
            ensure!(
                tx.execute(
                    "UPDATE local_runs SET turn_id = ?2 WHERE id = ?1 AND state = 'prepared'
                 AND turn_id IS NULL AND thread_id IS NOT NULL",
                    params![run.id, turn],
                )? == 1,
                "cannot attach the recovered turn"
            );
            save_event(&tx, run, RunPhase::Started)?;
        }
        ensure!(
            tx.execute(
                "UPDATE local_runs SET state = 'stopped'
             WHERE id = ?1 AND state IN ('prepared', 'running')",
                [&run.id],
            )? == 1,
            "cannot settle the original Run"
        );
        save_event(&tx, run, RunPhase::Stopped { outcome, error })?;
        tx.commit()?;
        Ok(())
    }

    pub fn running_slots(&self) -> Result<u32> {
        let mut statement = self
            .0
            .prepare("SELECT process_pid, process_start FROM local_runs WHERE state = 'running'")?;
        let mut running = 0;
        for process in statement.query_map([], |row| {
            Ok(ProcessIdentity {
                pid: row.get(0)?,
                started_at: row.get(1)?,
            })
        })? {
            if !process?.is_stopped()? {
                running += 1;
            }
        }
        Ok(running)
    }

    pub fn run_events(&self) -> Result<Vec<RunObservation>> {
        let mut statement = self.0.prepare(
            "SELECT o.observation FROM run_outbox o
             JOIN received_assignments a ON a.id = o.assignment_id
             WHERE o.acknowledged = 0 AND (a.acknowledged = 1 OR a.id IN (
                 SELECT id FROM received_assignments WHERE acknowledged = 0 ORDER BY rowid LIMIT ?1
             ))
             ORDER BY o.rowid LIMIT ?1",
        )?;
        statement
            .query_map([RUNNER_SYNC_BATCH], |row| row.get::<_, String>(0))?
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect()
    }
}

fn save_event(db: &Connection, run: &LocalRun, phase: RunPhase) -> Result<()> {
    let (thread_id, turn_id) = db.query_row(
        "SELECT thread_id, turn_id FROM local_runs WHERE id = ?1",
        [&run.id],
        |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
            ))
        },
    )?;
    let event = RunObservation {
        event_id: protocol::new_id(),
        assignment_id: run.assignment.placement.assignment_id.clone(),
        task_id: run.assignment.task_id.clone(),
        run_id: run.id.clone(),
        thread_id,
        turn_id,
        phase,
    };
    db.execute(
        "INSERT INTO run_outbox (id, assignment_id, observation) VALUES (?1, ?2, ?3)",
        params![
            event.event_id,
            event.assignment_id,
            serde_json::to_string(&event)?
        ],
    )?;
    Ok(())
}
