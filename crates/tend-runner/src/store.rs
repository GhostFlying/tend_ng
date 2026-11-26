mod control;
mod coordination;
mod handoff;
mod reclaim;
mod reconcile;
pub mod run;

use crate::config::Config;
use anyhow::{Result, ensure};
use protocol::{ClaimDecision, ClaimReceipt, RUNNER_SYNC_BATCH, RunnerSyncReply};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::{path::Path, time::Duration};

pub struct Store(Connection);

impl Store {
    pub fn open(path: &Path, config: &Config) -> Result<Self> {
        let mut database = Connection::open(path)?;
        database.busy_timeout(Duration::from_secs(5))?;
        database.pragma_update(None, "journal_mode", "WAL")?;
        database.pragma_update(None, "synchronous", "FULL")?;
        database.pragma_update(None, "foreign_keys", true)?;
        let tx = database.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS runner_instance (
                 singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                 runner_id TEXT NOT NULL,
                 host_id TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS received_assignments (
                 id TEXT PRIMARY KEY,
                 task_id TEXT NOT NULL,
                 contract TEXT NOT NULL,
                 repositories TEXT NOT NULL,
                 decision TEXT NOT NULL,
                 acknowledged INTEGER NOT NULL DEFAULT 0
             );
             CREATE INDEX IF NOT EXISTS unsent_claims
                ON received_assignments(acknowledged) WHERE acknowledged = 0;
             CREATE INDEX IF NOT EXISTS local_assignments_by_task ON received_assignments(task_id);
             CREATE TABLE IF NOT EXISTS local_execution_limit (
                 singleton INTEGER PRIMARY KEY CHECK(singleton = 1), slots INTEGER NOT NULL
             );",
        )?;
        run::create_tables(&tx)?;
        tx.execute_batch(
            "CREATE INDEX IF NOT EXISTS records_by_assignment ON run_outbox(assignment_id);",
        )?;
        tx.execute(
            "INSERT INTO runner_instance VALUES (1, ?1, ?2) ON CONFLICT DO NOTHING",
            params![config.runner_id, config.host_id],
        )?;
        let same = tx.query_row(
            "SELECT runner_id = ?1 AND host_id = ?2 FROM runner_instance WHERE singleton = 1",
            params![config.runner_id, config.host_id],
            |row| row.get::<_, bool>(0),
        )?;
        ensure!(
            same,
            "Runner database belongs to a different Runner or Host"
        );
        tx.commit()?;
        Ok(Self(database))
    }

    pub fn unfinished(&self) -> Result<u32> {
        Ok(self.0.query_row(
            "SELECT COUNT(*) FROM local_workspace_claims a WHERE a.execution_state = 'active'
                OR (a.execution_state = 'queued' AND NOT EXISTS (
                    SELECT 1 FROM local_workspace_claims holder
                    WHERE holder.id != a.id AND holder.path = a.path AND (holder.retains_workspace = 1
                      OR (holder.pending_review AND holder.task_id IS NOT a.author_task))
                ))",
            [],
            |row| row.get(0),
        )?)
    }

    pub fn receipts(&self) -> Result<Vec<ClaimReceipt>> {
        let mut statement = self.0.prepare(
            "SELECT id, decision FROM received_assignments WHERE acknowledged = 0 ORDER BY rowid LIMIT ?1",
        )?;
        statement
            .query_map([RUNNER_SYNC_BATCH], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .map(|row| {
                let (assignment_id, decision) = row?;
                Ok(ClaimReceipt {
                    assignment_id,
                    decision: serde_json::from_str(&decision)?,
                })
            })
            .collect()
    }

    pub fn apply_sync(&mut self, config: &Config, reply: RunnerSyncReply) -> Result<()> {
        let execution_limit = if let Some(slots) = reply.execution_slot_limit {
            let saved: Option<u32> = self
                .0
                .query_row("SELECT slots FROM local_execution_limit", [], |row| {
                    row.get(0)
                })
                .optional()?;
            (saved != Some(slots)).then_some(slots)
        } else {
            None
        };
        if reply.assignments.is_empty()
            && reply.confirmed_receipts.is_empty()
            && reply.confirmed_runs.is_empty()
            && reply.confirmed_operations.is_empty()
            && reply.inputs.is_empty()
            && reply.controls.is_empty()
            && reply.drain_to.is_none()
            && reply.workspace_reclaims.is_empty()
            && reply.confirmed_reclaims.is_empty()
            && execution_limit.is_none()
        {
            return Ok(());
        }
        let profile = config.profile(self.unfinished()?);
        let tx = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(target) = &reply.drain_to {
            handoff::accept_drain(&tx, target)?;
        }
        if let Some(slots) = execution_limit {
            tx.execute(
                "INSERT INTO local_execution_limit VALUES (1, ?1)
                 ON CONFLICT(singleton) DO UPDATE SET slots = excluded.slots",
                [slots],
            )?;
        }
        for id in reply.confirmed_reclaims {
            tx.execute(
                "UPDATE local_workspace_reclaims SET acknowledged = 1 WHERE id = ?1",
                [&id],
            )?;
        }
        for command in reply.workspace_reclaims {
            reclaim::accept(&tx, &command)?;
        }
        for acknowledged in reply.confirmed_receipts {
            tx.execute(
                "UPDATE received_assignments SET acknowledged = 1 WHERE id = ?1",
                [acknowledged],
            )?;
        }
        for event in reply
            .confirmed_runs
            .into_iter()
            .chain(reply.confirmed_operations)
        {
            tx.execute(
                "UPDATE run_outbox SET acknowledged = 1 WHERE id = ?1",
                [event],
            )?;
        }
        for assignment in reply.assignments {
            ensure!(
                assignment.placement.runner_id == config.runner_id
                    && assignment.placement.host_id == config.host_id,
                "assignment is addressed to a different Runner or Host",
            );
            let id = &assignment.placement.assignment_id;
            let contract = serde_json::to_string(&assignment)?;
            let prior = tx
                .query_row(
                    "SELECT contract FROM received_assignments WHERE id = ?1",
                    [id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            if let Some(prior) = prior {
                ensure!(
                    contract == prior,
                    "assignment changes its original contract"
                );
                continue;
            }

            let decision = config.decide(&assignment, &profile);
            let app_server = if decision == ClaimDecision::Accepted {
                assignment
                    .placement
                    .agent
                    .app_server
                    .as_ref()
                    .map(|id| serde_json::to_string(&config.app_servers[id]))
                    .transpose()?
            } else {
                None
            };
            let (repositories, state) = if decision == ClaimDecision::Accepted {
                let state = match assignment.handoff.as_ref().map(|context| &context.phase) {
                    Some(protocol::HandoffPhase::Waiting) => "waiting",
                    Some(protocol::HandoffPhase::WaitingExternal) => "waiting_external",
                    _ => "queued",
                };
                (config.repositories_for(&assignment)?, state)
            } else {
                ("[]".to_owned(), "rejected")
            };
            tx.execute(
                "INSERT INTO received_assignments (id, task_id, contract, repositories, decision,
                                                   execution_state, retains_workspace, app_server)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    id,
                    assignment.task_id,
                    contract,
                    repositories,
                    serde_json::to_string(&decision)?,
                    state,
                    state == "waiting_external"
                        || state == "queued"
                            && assignment.handoff.as_ref().is_some_and(|handoff| {
                                handoff.operations.iter().any(|op| {
                                    matches!(
                                        op.action,
                                        protocol::WorkAction::WatchExternal {
                                            mode: protocol::WatchMode::Wait,
                                            ..
                                        }
                                    )
                                })
                            }),
                    app_server
                ],
            )?;
        }
        for command in reply.controls {
            control::accept(&tx, &command)?;
        }
        for input in reply.inputs {
            coordination::accept_input(&tx, &input)?;
        }
        tx.commit()?;
        Ok(())
    }
}
