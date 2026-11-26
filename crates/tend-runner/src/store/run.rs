use super::{Store, coordination};
use crate::config::{AppServer, Repo};
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
    pub app_server: Option<AppServer>,
    pub resume_guard: Option<String>,
    pub resume_retry_at: i64,
    pub review_workspace: Option<String>,
    pub author_workspace_stamp: Option<protocol::WorkspaceStamp>,
}

pub(super) fn create_tables(db: &Connection) -> Result<()> {
    let version: i64 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    ensure!(
        version <= 8,
        "unsupported Runner database version {version}"
    );
    if version >= 1 {
        if version == 1 {
            migrate_waits(db)?;
        }
        if version < 3 {
            super::control::migrate(db)?;
        }
        if version < 4 {
            super::handoff::migrate(db)?;
        }
        if version < 5 {
            super::reclaim::migrate(db)?;
        }
        if version < 6 {
            migrate_app_servers(db)?;
        }
        if version < 7 {
            migrate_native_context(db)?;
        }
        if version < 8 {
            migrate_shared_reviews(db)?;
        }
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
    migrate_waits(db)?;
    super::control::migrate(db)?;
    super::handoff::migrate(db)?;
    super::reclaim::migrate(db)?;
    migrate_app_servers(db)?;
    migrate_native_context(db)?;
    migrate_shared_reviews(db)?;
    Ok(())
}

fn migrate_shared_reviews(db: &Connection) -> Result<()> {
    db.execute_batch(
        "ALTER TABLE local_runs ADD COLUMN workspace_stamp TEXT;
         CREATE VIEW local_workspace_paths AS
         SELECT a.id, a.task_id, a.execution_state, a.retains_workspace,
                json_extract(a.contract, '$.review.author_task_id') AS author_task,
                coalesce('workspaces/' || coalesce(json_extract(a.contract, '$.workspace_key'),
                                                  json_extract(a.contract, '$.parent_issue_id')),
                         'tasks/' || a.task_id) AS own_path,
                json_extract(a.contract, '$.review.author_workspace') AS bound_path
           FROM received_assignments a;
         CREATE VIEW local_workspace_claims AS
         SELECT a.*, CASE WHEN a.author_task IS NULL THEN a.own_path ELSE coalesce(a.bound_path, (
                    SELECT author.own_path FROM local_workspace_paths author
                    WHERE author.task_id = a.author_task AND author.execution_state NOT IN ('rejected','handed_off')
                    ORDER BY author.id LIMIT 1
                )) END AS path,
                (a.execution_state IN ('waiting','queued') AND EXISTS(
                    SELECT 1 FROM local_task_intents o
                     WHERE o.assignment_id = a.id AND json_extract(o.operation, '$.action.kind') = 'request_review'
                       AND NOT EXISTS(SELECT 1 FROM local_work_operations c WHERE c.assignment_id = a.id
                         AND c.rowid > o.rowid AND json_extract(c.operation, '$.action.kind') = 'close_issue')
                       AND NOT EXISTS(SELECT 1 FROM local_issue_inputs i WHERE i.assignment_id = a.id
                         AND i.run_id IS NOT NULL AND json_extract(i.payload, '$.kind') = 'review_finished'
                         AND json_extract(i.payload, '$.review.request_id') = o.id)
                )) AS pending_review
           FROM local_workspace_paths a;",
    )?;
    // v7 used the hard retention bit for both kinds of wait. Preserve all
    // external waits, including the ones already queued or assigned in handoff.
    let retained = {
        let mut query = db.prepare("SELECT id FROM received_assignments WHERE retains_workspace = 1 AND execution_state != 'waiting_external'")?;
        query
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for id in retained {
        if !coordination::waiting_for_external(db, &id)? {
            db.execute(
                "UPDATE received_assignments SET retains_workspace = 0 WHERE id = ?1",
                [&id],
            )?;
        }
    }
    db.pragma_update(None, "user_version", 8)?;
    Ok(())
}

fn migrate_native_context(db: &Connection) -> Result<()> {
    db.execute_batch(
        "ALTER TABLE local_runs ADD COLUMN native_context TEXT;
        ALTER TABLE local_runs ADD COLUMN app_server TEXT;
        ALTER TABLE local_runs ADD COLUMN resume_guard TEXT;
        ALTER TABLE local_runs ADD COLUMN resume_retry_at INTEGER NOT NULL DEFAULT 0;
        CREATE INDEX local_runs_by_assignment ON local_runs(assignment_id);",
    )?;
    db.pragma_update(None, "user_version", 7)?;
    Ok(())
}

fn migrate_app_servers(db: &Connection) -> Result<()> {
    db.execute_batch("ALTER TABLE received_assignments ADD COLUMN app_server TEXT;")?;
    db.pragma_update(None, "user_version", 6)?;
    Ok(())
}

fn migrate_waits(db: &Connection) -> Result<()> {
    db.execute_batch(
        "ALTER TABLE received_assignments ADD COLUMN retains_workspace INTEGER NOT NULL DEFAULT 0;",
    )?;
    db.pragma_update(None, "user_version", 2)?;
    Ok(())
}

fn ready_assignment(db: &Connection) -> Result<Option<String>> {
    Ok(db.query_row(
            "SELECT a.id FROM local_workspace_claims a JOIN received_assignments original ON original.id = a.id
               WHERE a.execution_state = 'queued'
                 AND NOT EXISTS (SELECT 1 FROM local_workspace_claims holder
                    WHERE holder.id != a.id AND holder.path = a.path AND (
                        holder.retains_workspace = 1 OR holder.execution_state = 'active'
                        OR (holder.pending_review AND holder.task_id IS NOT a.author_task)
                        OR (holder.execution_state = 'queued' AND holder.author_task = a.task_id
                            AND a.pending_review AND a.retains_workspace = 0)
                    ))
               ORDER BY original.rowid LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
    ).optional()?)
}

impl Store {
    pub fn native_turn(&self, run: &str) -> Result<Option<(String, String, Option<AppServer>)>> {
        self.0
            .query_row(
                "SELECT r.thread_id, r.turn_id,
                        CASE WHEN r.native_context IS NULL THEN a.app_server ELSE r.app_server END FROM local_runs r
                 JOIN received_assignments a ON a.id = r.assignment_id
                 WHERE r.id = ?1 AND r.thread_id IS NOT NULL AND r.turn_id IS NOT NULL",
                [run],
                |row| Ok((row.get(0)?, row.get(1)?, row.get::<_, Option<String>>(2)?)),
            )
            .optional()?
            .map(|(thread, turn, app)| {
                Ok((
                    thread,
                    turn,
                    app.as_deref().map(serde_json::from_str).transpose()?,
                ))
            })
            .transpose()
    }

    pub fn active_runs(&self) -> Result<Vec<String>> {
        let mut query = self.0.prepare(
            "SELECT id FROM local_runs WHERE state IN ('prepared', 'running') ORDER BY rowid",
        )?;
        Ok(query
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn next_queued_run(&mut self, slot_limit: u32) -> Result<Option<LocalRun>> {
        // Idle and blocked queues need no writer. Recheck under the same
        // transaction that installs the exclusive Run.
        let can_start = |db: &Connection| -> rusqlite::Result<bool> {
            db.query_row(
                "SELECT NOT EXISTS(SELECT 1 FROM local_drain)
                    AND (SELECT count(*) FROM local_runs WHERE state IN ('prepared', 'running'))
                        < min(?1, coalesce((SELECT slots FROM local_execution_limit), ?1))",
                [slot_limit],
                |row| row.get(0),
            )
        };
        if !can_start(&self.0)? || ready_assignment(&self.0)?.is_none() {
            return Ok(None);
        }
        let tx = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !can_start(&tx)? {
            return Ok(None);
        }
        let Some(assignment_id) = ready_assignment(&tx)? else {
            return Ok(None);
        };

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
             )",
            params![id, assignment_id],
        )?;
        tx.commit()?;
        self.load_run(&id).map(Some)
    }

    pub fn load_run(&self, id: &str) -> Result<LocalRun> {
        let (contract, repositories, thread_id, turn_id, process, app, resume_guard, resume_retry_at) = self.0.query_row(
            "SELECT a.contract, a.repositories, r.thread_id, r.turn_id, r.process_pid, r.process_start,
                    CASE WHEN r.native_context IS NULL THEN a.app_server ELSE r.app_server END,
                    r.resume_guard, r.resume_retry_at
             FROM local_runs r JOIN received_assignments a ON a.id = r.assignment_id
             WHERE r.id = ?1", [id],
            |row| {
                let process = match row.get::<_, Option<u32>>(4)? {
                    Some(pid) => Some(ProcessIdentity { pid, started_at: row.get(5)? }),
                    None => None,
                };
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get(2)?, row.get(3)?, process,
                    row.get::<_, Option<String>>(6)?, row.get(7)?, row.get(8)?))
            },
        )?;
        let mut assignment: TaskAssignment = serde_json::from_str(&contract)?;
        if assignment.review.is_some() {
            let request = self.0.query_row(
                "SELECT i.payload FROM local_issue_inputs i JOIN local_runs prior ON prior.id = i.run_id
                   JOIN local_runs r ON r.id = ?1 AND r.assignment_id = prior.assignment_id
                   WHERE prior.rowid <= r.rowid AND json_extract(i.payload, '$.kind') = 'review_requested'
                   ORDER BY prior.rowid DESC LIMIT 1",
                [id], |row| row.get::<_, String>(0),
            ).optional()?;
            if let Some(request) = request {
                let input: IssueInput = serde_json::from_str(&request)?;
                if let protocol::IssueInputCause::ReviewRequested { review, base_refs } =
                    input.cause
                {
                    assignment.work.objective = format!(
                        "Independently review {}. Scope: {}.",
                        review.candidate, review.scope
                    );
                    assignment.work.repo_ids = base_refs.keys().cloned().collect();
                    assignment.work.base_refs = base_refs;
                    assignment.review = Some(review);
                }
            }
        }
        let author_workspace = if let Some(review) = &assignment.review {
            self.0.query_row(
                "SELECT p.own_path, coalesce((
                       SELECT r.workspace_stamp FROM local_runs r WHERE r.assignment_id = a.id AND r.turn_id IS NOT NULL
                       ORDER BY r.rowid DESC LIMIT 1
                     ), json_extract(a.contract, '$.handoff.workspace_stamp'))
                   FROM local_workspace_paths p JOIN received_assignments a ON a.id = p.id
                   WHERE p.task_id = ?1 AND json_extract(a.contract, '$.issue_id') = ?2
                     AND a.execution_state NOT IN ('rejected','handed_off','active')
                     AND (EXISTS(SELECT 1 FROM local_runs r WHERE r.assignment_id = a.id AND r.turn_id IS NOT NULL)
                          OR json_extract(a.contract, '$.handoff') IS NOT NULL)
                   ORDER BY a.rowid DESC LIMIT 1",
                params![review.author_task_id, review.author_issue_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            ).optional()?.filter(|(path, _)| review.author_workspace.as_ref().is_none_or(|bound| bound == path))
        } else {
            None
        };
        let (review_workspace, stamp) =
            author_workspace.map_or((None, None), |(path, stamp)| (Some(path), stamp));
        Ok(LocalRun {
            id: id.to_owned(),
            assignment,
            repos: serde_json::from_str(&repositories)?,
            thread_id,
            turn_id,
            process,
            app_server: app.as_deref().map(serde_json::from_str).transpose()?,
            inputs: coordination::run_inputs(&self.0, id)?,
            resume_guard,
            resume_retry_at,
            review_workspace,
            author_workspace_stamp: stamp.as_deref().map(serde_json::from_str).transpose()?,
        })
    }

    pub fn reusable_thread(&self, run: &LocalRun, native_context: &str) -> Result<Option<String>> {
        // Only the immediately preceding execution of this assignment can
        // supply a candidate, and only with a recorded stop and turn.
        let previous = self
            .0
            .query_row(
                "SELECT state, thread_id, turn_id, native_context FROM local_runs
             WHERE assignment_id = ?1 AND id != ?2 AND (NOT ?3 OR turn_id IS NOT NULL)
             ORDER BY rowid DESC LIMIT 1",
                params![
                    run.assignment.placement.assignment_id,
                    run.id,
                    run.assignment.review.is_some()
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()?;
        Ok(previous.and_then(|(state, thread, turn, context)| {
            (state == "stopped" && turn.is_some() && context.as_deref() == Some(native_context))
                .then_some(thread)
                .flatten()
        }))
    }

    pub fn defer_active_resume(&self, run_id: &str, thread: &str) -> Result<()> {
        ensure!(
            self.0.execute(
                "UPDATE local_runs SET resume_guard = ?2, resume_retry_at = unixepoch() + 5
                 WHERE id = ?1 AND state = 'prepared' AND process_pid IS NULL
                   AND (resume_guard IS NULL OR resume_guard = ?2)",
                params![run_id, thread],
            )? == 1,
            "cannot defer the original prepared Run"
        );
        Ok(())
    }

    pub fn retry_resume(&self, run_id: &str) -> Result<()> {
        ensure!(
            self.0.execute(
                "UPDATE local_runs SET resume_retry_at = unixepoch() + 2
              WHERE id = ?1 AND state = 'prepared' AND process_pid IS NULL",
                [run_id],
            )? == 1,
            "cannot retry the original prepared Run"
        );
        Ok(())
    }

    pub fn attach_process(
        &self,
        run_id: &str,
        thread_id: &str,
        process: &ProcessIdentity,
        native_context: &str,
        app_server: Option<&AppServer>,
        workspace_stamp: &protocol::WorkspaceStamp,
    ) -> Result<()> {
        ensure!(
            self.0.execute(
                "UPDATE local_runs SET thread_id = ?2, process_pid = ?3, process_start = ?4, native_context = ?5, app_server = ?6, workspace_stamp = ?7, resume_guard = NULL, resume_retry_at = 0
             WHERE id = ?1 AND state = 'prepared' AND process_pid IS NULL",
                params![run_id, thread_id, process.pid, process.started_at, native_context,
                    app_server.map(serde_json::to_string).transpose()?, serde_json::to_string(workspace_stamp)?],
            )? == 1,
            "original Run is already attached"
        );
        Ok(())
    }

    pub fn mark_started(
        &mut self,
        run: &LocalRun,
        turn_id: &str,
        branches: &[protocol::WorkBranch],
    ) -> Result<()> {
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
        save_event(&tx, run, RunPhase::Started, branches)?;
        tx.commit()?;
        Ok(())
    }

    pub fn finish(
        &mut self,
        run: &LocalRun,
        outcome: RunOutcome,
        error: Option<String>,
        recovered_turn: Option<&str>,
        branches: &[protocol::WorkBranch],
        native_items: &[serde_json::Value],
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
            save_event(&tx, run, RunPhase::Started, branches)?;
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
        let (cancelled, commands) =
            super::control::stopped(&tx, assignment_id, &run.id, native_items)?;
        let outcome = if cancelled {
            RunOutcome::Cancelled
        } else {
            outcome
        };
        let waiting_for_external =
            !cancelled && coordination::waiting_for_external(&tx, assignment_id)?;
        let waiting_for_attention =
            !cancelled && coordination::waiting_for_attention(&tx, assignment_id)?;
        let has_unread = coordination::has_unread_input(&tx, assignment_id)?;
        let queued_input = outcome == RunOutcome::Completed && has_unread;
        let waiting_for_review =
            !cancelled && coordination::waiting_for_review(&tx, assignment_id)?;
        let waiting_for_children = !cancelled
            && !waiting_for_attention
            && !queued_input
            && (waiting_for_review || coordination::has_followup(&tx, assignment_id)?);
        let next_state = if cancelled {
            "cancelled"
        } else if queued_input
            || (waiting_for_attention || waiting_for_children || waiting_for_external) && has_unread
        {
            "queued"
        } else if waiting_for_external {
            "waiting_external"
        } else if waiting_for_attention || waiting_for_children {
            "waiting"
        } else {
            "finished"
        };
        tx.execute(
            "UPDATE received_assignments SET execution_state = ?2, retains_workspace = ?3 WHERE id = ?1",
            params![assignment_id, next_state, waiting_for_external],
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
                waiting_for_external,
            },
            branches,
        )?;
        for (command, state) in commands {
            super::control::finish(&tx, &command, state)?;
        }
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
    } else if kind == "handoff" {
        Ok(RunnerRecord::Handoff(serde_json::from_str(payload)?))
    } else if kind == "control" {
        Ok(RunnerRecord::Control(serde_json::from_str(payload)?))
    } else {
        Ok(RunnerRecord::Operation(serde_json::from_str(payload)?))
    }
}

fn save_event(
    db: &Connection,
    run: &LocalRun,
    phase: RunPhase,
    branches: &[protocol::WorkBranch],
) -> Result<()> {
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
        branches: branches.to_vec(),
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

impl Store {
    pub fn discussion_workspace_binding(
        &self,
        issue: &str,
        task: &str,
        assignment: &str,
    ) -> Result<Option<(String, protocol::WorkspaceStamp, Vec<String>)>> {
        let row = self
            .0
            .query_row(
                "SELECT p.path, coalesce((
                 SELECT r.workspace_stamp FROM local_runs r
                  WHERE r.assignment_id = a.id AND r.workspace_stamp IS NOT NULL
                  ORDER BY r.rowid DESC LIMIT 1
               ), json_extract(a.contract, '$.handoff.workspace_stamp')),
               json_extract(a.contract, '$.work.repo_ids')
               FROM local_workspace_claims p JOIN received_assignments a ON a.id = p.id
              WHERE p.task_id = ?1 AND json_extract(a.contract, '$.issue_id') = ?2 AND a.id = ?3
                AND p.path IS NOT NULL AND p.execution_state NOT IN ('rejected','handed_off')
              ORDER BY a.rowid DESC LIMIT 1",
                params![task, issue, assignment],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((path, Some(stamp), repos)) = row else {
            return Ok(None);
        };
        Ok(Some((
            path,
            serde_json::from_str(&stamp)?,
            repos
                .map(|repos| serde_json::from_str(&repos))
                .transpose()?
                .unwrap_or_default(),
        )))
    }
}
