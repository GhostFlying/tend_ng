use super::Store;
use anyhow::Result;
use protocol::{HandoffContext, HandoffPhase, HandoffProof};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

pub fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE local_drain (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), successor TEXT NOT NULL);
         CREATE VIEW local_task_intents AS
           SELECT id, assignment_id, operation, rowid FROM local_work_operations
           UNION ALL
           SELECT json_extract(item.value, '$.operation_id'), a.id, item.value, item.key - 1000000000
             FROM received_assignments a, json_each(a.contract, '$.handoff.operations') item;"
    )?;
    db.pragma_update(None, "user_version", 4)?;
    Ok(())
}

pub fn accept_drain(db: &Connection, target: &str) -> Result<()> {
    db.execute(
        "INSERT INTO local_drain VALUES (1, ?1) ON CONFLICT DO NOTHING",
        [target],
    )?;
    Ok(())
}

impl Store {
    pub fn is_draining(&self) -> Result<bool> {
        Ok(self
            .0
            .query_row("SELECT EXISTS(SELECT 1 FROM local_drain)", [], |row| {
                row.get(0)
            })?)
    }

    pub fn drain_finished(&self) -> Result<bool> {
        Ok(self.0.query_row(
            "SELECT EXISTS(SELECT 1 FROM local_drain)
               AND NOT EXISTS(SELECT 1 FROM received_assignments WHERE execution_state NOT IN ('handed_off','rejected') OR acknowledged = 0)
               AND NOT EXISTS(SELECT 1 FROM run_outbox WHERE acknowledged = 0)
               AND NOT EXISTS(SELECT 1 FROM local_workspace_reclaims WHERE acknowledged = 0)",
            [], |row| row.get(0),
        )?)
    }

    pub fn prepare_handoffs(&mut self) -> Result<()> {
        let Some(successor) = self
            .0
            .query_row("SELECT successor FROM local_drain", [], |row| {
                row.get::<_, String>(0)
            })
            .optional()?
        else {
            return Ok(());
        };
        let tx = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let candidates = {
            let mut query = tx.prepare(
                "WITH scope AS MATERIALIZED (
                    SELECT id, task_id, acknowledged, execution_state,
                        coalesce(json_extract(contract, '$.parent_issue_id'), json_extract(contract, '$.issue_id')) AS root
                     FROM received_assignments
                 )
                 SELECT a.id, a.task_id, a.execution_state FROM scope a
                 WHERE a.execution_state IN ('queued','waiting','waiting_external','finished','cancelled')
                 AND NOT EXISTS(SELECT 1 FROM scope b WHERE b.root = a.root AND (
                     b.execution_state = 'active' OR b.acknowledged = 0
                       OR EXISTS(SELECT 1 FROM run_outbox o WHERE o.assignment_id = b.id AND o.acknowledged = 0)
                 )) ORDER BY a.execution_state != 'waiting_external', a.id",
            )?;
            query
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (id, task_id, state) in candidates {
            let phase = match state.as_str() {
                "queued" => HandoffPhase::Queued,
                "waiting" => HandoffPhase::Waiting,
                "waiting_external" => HandoffPhase::WaitingExternal,
                _ => HandoffPhase::Finished,
            };
            let operations = if phase == HandoffPhase::Finished {
                Vec::new()
            } else {
                let mut query = tx.prepare(
                    "SELECT o.operation FROM local_task_intents o
                     WHERE o.assignment_id = ?1 AND (
                       json_extract(o.operation, '$.action.kind') = 'delegate'
                       OR (json_extract(o.operation, '$.action.kind') = 'request_review'
                         AND NOT EXISTS(SELECT 1 FROM local_issue_inputs i WHERE i.assignment_id = ?1
                           AND i.run_id IS NOT NULL AND json_extract(i.payload, '$.kind') = 'review_finished'
                           AND json_extract(i.payload, '$.review.request_id') = o.id))
                       OR (json_extract(o.operation, '$.action.kind') = 'ask_attention'
                         AND json_extract(o.operation, '$.action.proposal.replaces') IS NULL
                         AND NOT EXISTS(SELECT 1 FROM local_issue_inputs i WHERE i.assignment_id = ?1 AND i.run_id IS NOT NULL
                           AND json_extract(i.payload, '$.kind') = 'attention_answered' AND json_extract(i.payload, '$.attention_id') = o.id))
                       OR (json_extract(o.operation, '$.action.kind') = 'watch_external' AND json_extract(o.operation, '$.action.mode') = 'wait'
                         AND o.rowid > coalesce((SELECT MAX(c.rowid) FROM local_task_intents c WHERE c.assignment_id = ?1
                           AND json_extract(c.operation, '$.action.kind') IN ('clear_external_waits','close_issue')), -2000000000)
                         AND NOT EXISTS(SELECT 1 FROM local_issue_inputs i WHERE i.assignment_id = ?1 AND i.run_id IS NOT NULL
                           AND json_extract(i.payload, '$.kind') = 'external_fact' AND json_extract(i.payload, '$.watch_id') = o.id))
                     ) ORDER BY o.rowid",
                )?;
                query
                    .query_map([&id], |row| row.get::<_, String>(0))?
                    .map(|raw| {
                        let mut operation: protocol::WorkOperation = serde_json::from_str(&raw?)?;
                        // Only the original wait obligation is handed to a successor.
                        if let protocol::WorkAction::AskAttention { proposal, .. } =
                            &mut operation.action
                        {
                            *proposal = None;
                        }
                        Ok(operation)
                    })
                    .collect::<Result<Vec<_>>>()?
            };
            let proof = HandoffProof {
                id: protocol::new_id(),
                assignment_id: id.clone(),
                task_id,
                successor: successor.clone(),
                context: HandoffContext { phase, operations },
            };
            tx.execute("UPDATE received_assignments SET execution_state = 'handed_off', retains_workspace = 0 WHERE id = ?1", [&id])?;
            tx.execute(
                "INSERT INTO run_outbox(id, assignment_id, observation, record_kind) VALUES (?1, ?2, ?3, 'handoff')",
                params![proof.id, proof.assignment_id, serde_json::to_string(&proof)?],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}
