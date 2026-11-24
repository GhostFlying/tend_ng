use super::{
    Store, coordination, run,
    work::{DISPATCH_QUERY, read_dispatch},
};
use anyhow::{Result, ensure};
use protocol::{
    AgentCandidate, AgentQuery, ClaimDecision, CoordinationFollowup, IssueDispatch, Placement,
    RunnerProfile, RunnerRecord, RunnerSync, RunnerSyncReply, TaskAssignment, matching_agents,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

const FRESH_SECONDS: i64 = 15;
const RESERVATION_SECONDS: i64 = 10;
const HEARTBEAT_WRITE_SECONDS: i64 = 3;
const PAGE_SIZE: usize = protocol::RUNNER_SYNC_BATCH;

pub fn create_tables(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE runners (
            id TEXT PRIMARY KEY,
            host_id TEXT NOT NULL,
            profile TEXT NOT NULL,
            last_seen INTEGER NOT NULL
         );
         CREATE INDEX runners_by_contact ON runners(last_seen);
         CREATE TABLE runner_assignments (
            id TEXT PRIMARY KEY,
            task_id TEXT NOT NULL REFERENCES tasks(id),
            runner_id TEXT NOT NULL REFERENCES runners(id),
            contract TEXT NOT NULL,
            state TEXT NOT NULL CHECK (state IN ('reserved', 'issued', 'accepted', 'rejected', 'revoked')),
            expires_at INTEGER NOT NULL,
            decision TEXT
         );
         CREATE INDEX assignment_delivery ON runner_assignments(runner_id, state);
         CREATE INDEX assignment_expiration ON runner_assignments(state, expires_at);
         CREATE INDEX pending_task_order ON tasks(status) WHERE status = 'awaiting_assignment';
         ALTER TABLE tasks ADD COLUMN current_assignment TEXT REFERENCES runner_assignments(id);",
    )?;
    Ok(())
}

struct AvailableRunner {
    profile: RunnerProfile,
    last_seen: i64,
}

fn available_runners(db: &Connection) -> Result<Vec<AvailableRunner>> {
    let mut statement = db.prepare(
        "SELECT r.profile, r.last_seen FROM runners r
         WHERE r.last_seen > unixepoch() - ?1
           AND NOT EXISTS(SELECT 1 FROM runner_replacements p WHERE p.predecessor = r.id)
           AND (coalesce(json_extract(r.profile, '$.standby'), 0) = 0 OR
                EXISTS(SELECT 1 FROM runner_replacements p WHERE p.successor = r.id))
           AND json_extract(r.profile, '$.available_for_assignment') > (
                SELECT COUNT(*) FROM runner_assignments a
                WHERE a.runner_id = r.id AND a.state IN ('reserved', 'issued')
           )
         ORDER BY r.id",
    )?;
    let rows = statement.query_map([FRESH_SECONDS], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    rows.map(|row| {
        let (profile, last_seen) = row?;
        Ok(AvailableRunner {
            profile: serde_json::from_str(&profile)?,
            last_seen,
        })
    })
    .collect()
}

pub(super) fn assign(db: &Connection, dispatch: IssueDispatch) -> Result<()> {
    if dispatch.status != "awaiting_assignment" {
        return Ok(());
    }
    if db.query_row(
        "SELECT EXISTS(SELECT 1 FROM tasks t JOIN issues i ON i.id = t.issue_id
             JOIN workspace_reclaims w ON w.issue_id = coalesce(i.parent_issue_id, i.id) AND w.receipt IS NULL
             WHERE t.id = ?1)",
        [&dispatch.task_id], |row| row.get::<_, bool>(0),
    )? { return Ok(()) }
    let (pinned, named, previous, workspace_key, continuation) = db.query_row(
        "SELECT coalesce(h.runner_id, t.pinned_runner_id), t.pinned_agent_id, t.previous_task_id, t.workspace_key, t.handoff
         FROM tasks t JOIN issues i ON i.id = t.issue_id
         LEFT JOIN handoff_roots h ON h.root_id = coalesce(i.parent_issue_id, i.id)
         WHERE t.id = ?1",
        [&dispatch.task_id],
        |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, Option<String>>(1)?,
                  row.get::<_, Option<String>>(2)?, row.get::<_, Option<String>>(3)?, row.get::<_, Option<String>>(4)?)),
    )?;
    for available in available_runners(db)? {
        let profile = available.profile;
        if pinned.as_ref().is_some_and(|id| id != &profile.runner_id) {
            continue;
        }
        if db.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM tasks candidate JOIN issues c ON c.id = candidate.issue_id
               JOIN handoff_roots h ON h.root_id = coalesce(c.parent_issue_id, c.id)
               JOIN issues i ON i.parent_issue_id = c.parent_issue_id
               JOIN tasks holder ON holder.issue_id = i.id AND holder.id != candidate.id
               LEFT JOIN runner_assignments a ON a.id = holder.current_assignment
               WHERE candidate.id = ?1
                 AND coalesce(holder.workspace_key, i.parent_issue_id) = coalesce(candidate.workspace_key, c.parent_issue_id)
                 AND holder.status NOT IN ('completed','failed','cancelled')
                 AND (a.runner_id IS NULL OR a.runner_id != ?2 OR a.state != 'accepted')
                 AND (SELECT json_extract(r.observation, '$.phase.waiting_for_external')
                      FROM task_runs r WHERE r.task_id = holder.id ORDER BY r.rowid DESC LIMIT 1) = 1
            )",
            params![dispatch.task_id, profile.runner_id], |row| row.get::<_, bool>(0),
        )? { continue; }
        let Some(agent) = protocol::matching_task_agent(
            &profile,
            &dispatch.category,
            &dispatch.work,
            named.as_deref(),
        )
        .cloned() else {
            continue;
        };
        let placement = Placement {
            assignment_id: protocol::new_id(),
            runner_id: profile.runner_id,
            host_id: profile.host_id,
            agent,
        };
        let assignment = TaskAssignment {
            placement,
            task_id: dispatch.task_id,
            issue_id: dispatch.issue_id,
            source_message_id: dispatch.source_message_id,
            chat_id: dispatch.chat_id,
            parent_issue_id: dispatch.parent_issue_id,
            workspace_key,
            handoff: continuation
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?,
            followup: previous.map(|previous_task_id| CoordinationFollowup {
                previous_task_id,
                issue_state: dispatch.issue_state,
            }),
            work: dispatch.work,
        };
        let contract = serde_json::to_string(&assignment)?;
        db.execute(
            "INSERT INTO runner_assignments (id, task_id, runner_id, contract, state, expires_at)
             VALUES (?1, ?2, ?3, ?4, 'reserved', min(unixepoch() + ?5, ?6))",
            params![
                assignment.placement.assignment_id,
                assignment.task_id,
                assignment.placement.runner_id,
                contract,
                RESERVATION_SECONDS,
                available.last_seen + FRESH_SECONDS,
            ],
        )?;
        db.execute(
            "UPDATE tasks SET current_assignment = ?1, status = 'reserved' WHERE id = ?2",
            params![assignment.placement.assignment_id, assignment.task_id],
        )?;
        db.execute(
            "UPDATE task_controls SET assignment_id = ?2 WHERE task_id = ?1 AND state = 'pending' AND run_id IS NULL",
            params![assignment.task_id, assignment.placement.assignment_id],
        )?;
        break;
    }
    Ok(())
}

impl Store {
    pub fn agent_candidates(&self, query: &AgentQuery) -> Result<Vec<AgentCandidate>> {
        let available: std::collections::HashSet<_> = available_runners(&self.db)?
            .into_iter()
            .map(|runner| runner.profile.runner_id)
            .collect();
        let mut statement = self.db.prepare(
            "SELECT profile FROM runners
             WHERE NOT EXISTS(SELECT 1 FROM runner_replacements p WHERE p.predecessor = runners.id)
             ORDER BY id",
        )?;
        let mut candidates = Vec::new();
        for row in statement.query_map([], |row| row.get::<_, String>(0))? {
            let profile: RunnerProfile = serde_json::from_str(&row?)?;
            candidates.extend(
                matching_agents(
                    &profile,
                    &query.category,
                    &query.repo_ids,
                    &query.required_capabilities,
                )
                .map(|agent| AgentCandidate {
                    runner_id: profile.runner_id.clone(),
                    available: available.contains(&profile.runner_id),
                    agent: agent.clone(),
                }),
            );
        }
        Ok(candidates)
    }

    pub fn reconcile_scope(&self, task: &str, request: &RunnerSync) -> Result<bool> {
        let mut statement = self
            .db
            .prepare("SELECT id FROM runner_assignments WHERE task_id = ?1 AND runner_id = ?2")?;
        let registered = statement
            .query_map(params![task, request.profile.runner_id], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<std::collections::HashSet<_>>>()?;
        let records = request.records.iter().map(|record| match record {
            RunnerRecord::Run(run) => &run.assignment_id,
            RunnerRecord::Operation(op) => &op.assignment_id,
            RunnerRecord::Control(ack) => &ack.assignment_id,
            RunnerRecord::Handoff(proof) => &proof.assignment_id,
        });
        Ok(!registered.is_empty()
            && request
                .receipts
                .iter()
                .map(|r| &r.assignment_id)
                .chain(request.runs.iter().map(|run| &run.assignment_id))
                .chain(records)
                .all(|id| registered.contains(id)))
    }

    pub fn schedule_assignments(&mut self, after: &mut i64) -> Result<()> {
        if self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM runner_assignments
             WHERE state = 'reserved' AND expires_at <= unixepoch())",
            [],
            |row| row.get::<_, bool>(0),
        )? {
            let tx = self
                .db
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            expire(&tx)?;
            tx.commit()?;
        }

        let runners = available_runners(&self.db)?;
        if runners.is_empty() {
            *after = 0;
            return Ok(());
        }
        let pending = {
            let mut statement = self.db.prepare(
                "SELECT t.rowid, t.id, coalesce(h.runner_id, t.pinned_runner_id), t.pinned_agent_id
                 FROM tasks t JOIN issues i ON i.id = t.issue_id LEFT JOIN handoff_roots h ON h.root_id = coalesce(i.parent_issue_id, i.id)
                 WHERE t.status = 'awaiting_assignment' AND t.rowid > ?1
                 ORDER BY t.rowid LIMIT ?2",
            )?;
            statement
                .query_map(params![*after, PAGE_SIZE], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        *after = pending.last().map_or(0, |(seq, ..)| *seq);
        let query = format!("{DISPATCH_QUERY} WHERE t.id = ?1");
        for (_, id, pinned, named) in pending {
            let Some(dispatch) = self.db.query_row(&query, [&id], read_dispatch).optional()? else {
                continue;
            };
            if runners.iter().any(|runner| {
                pinned
                    .as_ref()
                    .is_none_or(|id| *id == runner.profile.runner_id)
                    && protocol::matching_task_agent(
                        &runner.profile,
                        &dispatch.category,
                        &dispatch.work,
                        named.as_deref(),
                    )
                    .is_some()
            }) {
                let tx = self
                    .db
                    .transaction_with_behavior(TransactionBehavior::Immediate)?;
                let current = tx.query_row(&query, [&id], read_dispatch)?;
                assign(&tx, current)?;
                tx.commit()?;
            }
        }
        Ok(())
    }

    pub fn sync_runner(
        &mut self,
        request: RunnerSync,
        task: Option<&str>,
    ) -> Result<Option<RunnerSyncReply>> {
        let profile = &request.profile;
        let profile_json = serde_json::to_string(profile)?;
        let stored = self
            .db
            .query_row(
                "SELECT host_id, profile, last_seen + ?2 > unixepoch() FROM runners WHERE id = ?1",
                params![profile.runner_id, HEARTBEAT_WRITE_SECONDS],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, bool>(2)?,
                    ))
                },
            )
            .optional()?;
        if stored
            .as_ref()
            .is_some_and(|(host, _, _)| host != &profile.host_id)
        {
            return Ok(None);
        }
        let refresh = task.is_none()
            && !stored.is_some_and(|(_, previous, recent)| recent && previous == profile_json);
        let reserved = task.is_none()
            && self.db.query_row(
                "SELECT EXISTS(SELECT 1 FROM runner_assignments
             WHERE runner_id = ?1 AND state = 'reserved' AND expires_at > unixepoch())",
                [&profile.runner_id],
                |row| row.get::<_, bool>(0),
            )?;
        if !refresh
            && !reserved
            && request.receipts.is_empty()
            && request.runs.is_empty()
            && request.records.is_empty()
            && !request.drained
            && request.reclaim_receipts.is_empty()
        {
            return Ok(Some(RunnerSyncReply {
                workspace_reclaims: if task.is_none() {
                    super::reclaim::pending(&self.db, &profile.runner_id)?
                } else {
                    Vec::new()
                },
                confirmed_reclaims: Vec::new(),
                assignments: if task.is_none() {
                    assignments_to_send(&self.db, &profile.runner_id, false)?
                } else {
                    Vec::new()
                },
                confirmed_receipts: Vec::new(),
                confirmed_runs: Vec::new(),
                confirmed_operations: Vec::new(),
                inputs: coordination::pending_inputs(&self.db, &profile.runner_id, task)?,
                controls: super::control::pending(&self.db, &profile.runner_id, task)?,
                drain_to: super::handoff::drain_target(&self.db, &profile.runner_id)?,
                uploads: super::artifacts::pending(&self.db, &profile.runner_id, task)?,
            }));
        }

        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if refresh {
            tx.execute(
                "INSERT INTO runners (id, host_id, profile, last_seen, protocol)
                 VALUES (?1, ?2, ?3, unixepoch(), ?4) ON CONFLICT(id) DO UPDATE SET
                   profile = excluded.profile, last_seen = excluded.last_seen, protocol = excluded.protocol",
                params![profile.runner_id, profile.host_id, profile_json, request.protocol],
            )?;
        }
        let mut confirmed_receipts = Vec::new();
        for receipt in request.receipts {
            let recorded = tx.query_row(
                "SELECT state, task_id, decision FROM runner_assignments WHERE id = ?1 AND runner_id = ?2",
                params![receipt.assignment_id, profile.runner_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?)),
            ).optional()?;
            let Some((state, task, recorded_decision)) = recorded else {
                return Ok(None);
            };
            let decision = serde_json::to_string(&receipt.decision)?;
            if state == "issued" {
                let (state, status) = match receipt.decision {
                    ClaimDecision::Accepted => {
                        let waiting = tx.query_row(
                            "SELECT coalesce(json_extract(contract, '$.handoff.phase') IN ('waiting','waiting_external'), 0)
                             FROM runner_assignments WHERE id = ?1", [&receipt.assignment_id], |row| row.get::<_, bool>(0))?;
                        ("accepted", if waiting { "waiting" } else { "accepted" })
                    }
                    ClaimDecision::Rejected { .. } => {
                        let cancelled = tx.query_row(
                            "SELECT EXISTS(SELECT 1 FROM task_controls WHERE assignment_id = ?1 AND state = 'pending'
                                  AND json_extract(action, '$.kind') = 'stop')",
                            [&receipt.assignment_id], |row| row.get::<_, bool>(0))?;
                        (
                            "rejected",
                            if cancelled {
                                "cancelled"
                            } else {
                                "awaiting_assignment"
                            },
                        )
                    }
                };
                tx.execute(
                    "UPDATE runner_assignments SET state = ?2, decision = ?3 WHERE id = ?1",
                    params![receipt.assignment_id, state, decision],
                )?;
                ensure!(tx.execute(
                    "UPDATE tasks SET status = ?2,
                            current_assignment = CASE WHEN ?2 IN ('accepted','waiting') THEN current_assignment END
                     WHERE id = ?1 AND current_assignment = ?3",
                    params![task, status, receipt.assignment_id],
                )? == 1, "delivered assignment changed before receipt");
            } else if recorded_decision.as_deref() != Some(&decision) {
                return Ok(None);
            }
            confirmed_receipts.push(receipt.assignment_id);
        }
        let mut confirmed_runs = Vec::new();
        for observation in request.runs {
            if !run::apply(&tx, &profile.runner_id, &observation)? {
                return Ok(None);
            }
            confirmed_runs.push(observation.event_id);
        }
        let mut confirmed_operations = Vec::new();
        for record in request.records {
            match record {
                RunnerRecord::Run(observation) => {
                    if !run::apply(&tx, &profile.runner_id, &observation)? {
                        return Ok(None);
                    }
                    confirmed_runs.push(observation.event_id);
                }
                RunnerRecord::Handoff(proof) => {
                    if !super::handoff::apply(&tx, &profile.runner_id, &proof)? {
                        return Ok(None);
                    }
                    confirmed_operations.push(proof.id);
                }
                RunnerRecord::Control(ack) => {
                    if !super::control::acknowledge(&tx, &profile.runner_id, &ack)? {
                        return Ok(None);
                    }
                    confirmed_operations.push(if ack.event_id.is_empty() {
                        ack.id
                    } else {
                        ack.event_id
                    });
                }
                RunnerRecord::Operation(operation) => {
                    if !coordination::apply(&tx, profile, &operation)? {
                        return Ok(None);
                    }
                    confirmed_operations.push(operation.operation_id);
                }
            }
        }

        let mut confirmed_reclaims = Vec::new();
        for receipt in request.reclaim_receipts {
            if !super::reclaim::accept(&tx, &profile.runner_id, &receipt)? {
                return Ok(None);
            }
            confirmed_reclaims.push(receipt.id);
        }
        let workspace_reclaims = if task.is_none() {
            super::reclaim::pending(&tx, &profile.runner_id)?
        } else {
            Vec::new()
        };
        let assignments = if task.is_none() {
            assignments_to_send(&tx, &profile.runner_id, true)?
        } else {
            Vec::new()
        };
        for assignment in &assignments {
            let id = &assignment.placement.assignment_id;
            if tx.execute(
                "UPDATE runner_assignments SET state = 'issued' WHERE id = ?1 AND state = 'reserved'",
                [id],
            )? == 1 {
                tx.execute(
                    "UPDATE tasks SET status = 'awaiting_receipt' WHERE current_assignment = ?1",
                    [id],
                )?;
            }
        }
        let inputs = coordination::pending_inputs(&tx, &profile.runner_id, task)?;
        let uploads = super::artifacts::pending(&tx, &profile.runner_id, task)?;
        let controls = super::control::pending(&tx, &profile.runner_id, task)?;
        let drain_to = super::handoff::drain_target(&tx, &profile.runner_id)?;
        if request.drained {
            super::handoff::note_drained(&tx, &profile.runner_id)?;
        }
        tx.commit()?;
        Ok(Some(RunnerSyncReply {
            workspace_reclaims,
            confirmed_reclaims,
            assignments,
            confirmed_receipts,
            confirmed_runs,
            confirmed_operations,
            inputs,
            controls,
            drain_to,
            uploads,
        }))
    }
}

fn expire(db: &Connection) -> Result<()> {
    let mut statement = db.prepare(
        "UPDATE runner_assignments SET state = 'revoked' WHERE id IN (
            SELECT id FROM runner_assignments
            WHERE state = 'reserved' AND expires_at <= unixepoch() ORDER BY expires_at LIMIT ?1
         ) RETURNING task_id, id",
    )?;
    let expired = statement
        .query_map([PAGE_SIZE], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (task, id) in expired {
        db.execute(
            "UPDATE tasks SET current_assignment = NULL, status = 'awaiting_assignment'
             WHERE id = ?1 AND current_assignment = ?2",
            params![task, id],
        )?;
    }
    Ok(())
}

fn assignments_to_send(
    db: &Connection,
    runner: &str,
    include_reserved: bool,
) -> Result<Vec<TaskAssignment>> {
    let mut statement = db.prepare(
        "SELECT contract FROM runner_assignments WHERE runner_id = ?1 AND
         (state = 'issued' OR (?2 AND state = 'reserved' AND expires_at > unixepoch()))
         ORDER BY rowid LIMIT ?3",
    )?;
    statement
        .query_map(params![runner, include_reserved, PAGE_SIZE], |row| {
            row.get::<_, String>(0)
        })?
        .map(|contract| Ok(serde_json::from_str(&contract?)?))
        .collect()
}
