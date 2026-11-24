use super::{Store, coordination};
use crate::config::Repo;
use anyhow::{Result, ensure};
use codex_app::ProcessIdentity;
use protocol::{
    IssueInput, RUNNER_SYNC_BATCH, RunObservation, RunOutcome, RunPhase, RunnerRecord,
    TaskAssignment,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

pub struct LocalRun {
    pub id: String,
    pub assignment: TaskAssignment,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub process: Option<ProcessIdentity>,
    pub inputs: Vec<IssueInput>,
    pub repos: Vec<Repo>,
}

pub(super) fn create_tables(db: &Connection) -> Result<()> {
    let version: i64 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    ensure!(
        version <= 1,
        "unsupported Runner database version {version}"
    );
    if version == 1 {
        return Ok(());
    }
    let legacy: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = 'local_runs')",
        [],
        |row| row.get(0),
    )?;
    db.execute_batch(
        "ALTER TABLE received_assignments ADD COLUMN execution_state TEXT NOT NULL DEFAULT 'queued';
         UPDATE received_assignments SET execution_state = 'rejected'
             WHERE json_extract(decision, '$.status') = 'rejected';
         CREATE INDEX assignment_execution_queue ON received_assignments(execution_state);",
    )?;
    if legacy {
        db.execute_batch(
            "ALTER TABLE local_runs RENAME TO legacy_runs;
             ALTER TABLE run_outbox ADD COLUMN record_kind TEXT NOT NULL DEFAULT 'run';",
        )?;
    }
    db.execute_batch(
        "CREATE TABLE local_runs (
            id TEXT PRIMARY KEY,
            assignment_id TEXT NOT NULL REFERENCES received_assignments(id),
            state TEXT NOT NULL,
            thread_id TEXT,
            turn_id TEXT,
            process_pid INTEGER,
            process_start TEXT
         );
         CREATE TABLE IF NOT EXISTS run_outbox (
            id TEXT PRIMARY KEY,
            assignment_id TEXT NOT NULL REFERENCES received_assignments(id),
            observation TEXT NOT NULL,
            acknowledged INTEGER NOT NULL DEFAULT 0,
            record_kind TEXT NOT NULL DEFAULT 'run'
         );
         CREATE INDEX IF NOT EXISTS unsent_runs ON run_outbox(acknowledged) WHERE acknowledged = 0;",
    )?;
    if legacy {
        db.execute_batch(
            "INSERT INTO local_runs SELECT * FROM legacy_runs ORDER BY rowid;
             UPDATE received_assignments AS a
                SET execution_state = coalesce((SELECT CASE WHEN state = 'stopped'
                    THEN 'finished' ELSE 'active' END FROM legacy_runs WHERE assignment_id = a.id), execution_state)
                WHERE execution_state = 'queued';
             DROP TABLE legacy_runs;",
        )?;
    }
    db.execute_batch(
        "CREATE INDEX local_run_state ON local_runs(state);
         CREATE UNIQUE INDEX local_active_assignment ON local_runs(assignment_id)
             WHERE state IN ('prepared', 'running');",
    )?;
    coordination::create_tables(db)?;
    db.pragma_update(None, "user_version", 1)?;
    Ok(())
}

impl Store {
    pub fn native_turn(&self, run: &str) -> Result<Option<(String, String)>> {
        Ok(self
            .0
            .query_row(
                "SELECT thread_id, turn_id FROM local_runs WHERE id = ?1
               AND thread_id IS NOT NULL AND turn_id IS NOT NULL",
                [run],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?)
    }

    pub fn next_run(&mut self) -> Result<Option<LocalRun>> {
        let active = self.0.query_row(
            "SELECT r.id FROM local_runs r WHERE r.state IN ('prepared', 'running') ORDER BY r.rowid LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        ).optional()?;
        if let Some(id) = active {
            return self.load_run(&id).map(Some);
        }
        let queued = self.0.query_row(
            "SELECT id FROM received_assignments WHERE execution_state = 'queued' ORDER BY rowid LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        ).optional()?;
        let Some(assignment_id) = queued else {
            return Ok(None);
        };

        let tx = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let acquired = tx.execute(
            "UPDATE received_assignments SET execution_state = 'active'
              WHERE id = ?1 AND execution_state = 'queued'",
            [&assignment_id],
        )?;
        if acquired == 0 {
            return Ok(None);
        }
        let id = protocol::new_id();
        tx.execute(
            "INSERT INTO local_runs (id, assignment_id, state) VALUES (?1, ?2, 'prepared')",
            params![id, assignment_id],
        )?;
        tx.execute(
            "UPDATE local_issue_inputs SET run_id = ?1 WHERE id IN (
                 SELECT id FROM local_issue_inputs WHERE assignment_id = ?2 AND run_id IS NULL
                 ORDER BY rowid LIMIT ?3
             )",
            params![id, assignment_id, RUNNER_SYNC_BATCH],
        )?;
        tx.commit()?;
        self.load_run(&id).map(Some)
    }

    fn load_run(&self, id: &str) -> Result<LocalRun> {
        let (contract, repositories, thread_id, turn_id, process) = self.0.query_row(
            "SELECT a.contract, a.repositories, r.thread_id, r.turn_id, r.process_pid, r.process_start
             FROM local_runs r JOIN received_assignments a ON a.id = r.assignment_id
             WHERE r.id = ?1", [id],
            |row| {
                let process = match row.get::<_, Option<u32>>(4)? {
                    Some(pid) => Some(ProcessIdentity { pid, started_at: row.get(5)? }),
                    None => None,
                };
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get(2)?, row.get(3)?, process))
            },
        )?;
        Ok(LocalRun {
            id: id.to_owned(),
            assignment: serde_json::from_str(&contract)?,
            repos: serde_json::from_str(&repositories)?,
            thread_id,
            turn_id,
            process,
            inputs: coordination::run_inputs(&self.0, id)?,
        })
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
        let assignment_id = &run.assignment.placement.assignment_id;
        let waiting_for_attention = coordination::waiting_for_attention(&tx, assignment_id)?;
        let has_unread = coordination::has_unread_input(&tx, assignment_id)?;
        let queued_input = outcome == RunOutcome::Completed && has_unread;
        let waiting_for_children = !waiting_for_attention
            && !queued_input
            && coordination::has_followup(&tx, assignment_id)?;
        let next_state =
            if queued_input || (waiting_for_attention || waiting_for_children) && has_unread {
                "queued"
            } else if waiting_for_attention || waiting_for_children {
                "waiting"
            } else {
                "finished"
            };
        tx.execute(
            "UPDATE received_assignments SET execution_state = ?2 WHERE id = ?1",
            params![assignment_id, next_state],
        )?;
        save_event(
            &tx,
            run,
            RunPhase::Stopped {
                outcome,
                error,
                waiting_for_children,
                waiting_for_attention,
                queued_input,
            },
        )?;
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

    pub fn records(&self) -> Result<Vec<RunnerRecord>> {
        let mut statement = self.0.prepare(
            "SELECT o.observation, o.record_kind FROM run_outbox o
             JOIN received_assignments a ON a.id = o.assignment_id
             WHERE o.acknowledged = 0 AND (a.acknowledged = 1 OR a.id IN (
                 SELECT id FROM received_assignments WHERE acknowledged = 0 ORDER BY rowid LIMIT ?1
             ))
             ORDER BY o.rowid LIMIT ?1",
        )?;
        statement
            .query_map([RUNNER_SYNC_BATCH], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .map(|row| {
                let (payload, kind) = row?;
                decode_record(&payload, &kind)
            })
            .collect()
    }
}

pub(super) fn decode_record(payload: &str, kind: &str) -> Result<RunnerRecord> {
    if kind == "run" {
        Ok(RunnerRecord::Run(serde_json::from_str(payload)?))
    } else {
        Ok(RunnerRecord::Operation(serde_json::from_str(payload)?))
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
        input_ids: run
            .inputs
            .iter()
            .map(|input| input.input_id.clone())
            .collect(),
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
