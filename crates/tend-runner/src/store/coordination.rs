use super::{Store, run::LocalRun};
use anyhow::{Result, ensure};
use codex_app::ToolCall;
use protocol::{IssueDelivery, IssueInput, WorkAction, WorkOperation};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

pub(super) fn create_tables(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE local_work_operations (
            id TEXT PRIMARY KEY,
            assignment_id TEXT NOT NULL REFERENCES received_assignments(id),
            run_id TEXT NOT NULL REFERENCES local_runs(id),
            call_id TEXT NOT NULL,
            request TEXT NOT NULL,
            operation TEXT NOT NULL,
            UNIQUE(run_id, call_id)
         );
         CREATE INDEX work_operations_by_assignment ON local_work_operations(assignment_id);
         CREATE TABLE local_issue_inputs (
            id TEXT PRIMARY KEY,
            assignment_id TEXT NOT NULL REFERENCES received_assignments(id),
            child_issue_id TEXT NOT NULL,
            payload TEXT NOT NULL,
            run_id TEXT REFERENCES local_runs(id)
         );
         CREATE INDEX local_pending_issue_inputs ON local_issue_inputs(assignment_id) WHERE run_id IS NULL;
         CREATE INDEX local_inputs_by_run ON local_issue_inputs(run_id);
         CREATE INDEX local_child_facts ON local_issue_inputs(assignment_id, child_issue_id);",
    )?;
    Ok(())
}

impl Store {
    pub fn coordination_context(
        &self,
        run: &LocalRun,
        resumed: bool,
        review_artifact: Option<&std::path::Path>,
    ) -> Result<Option<String>> {
        if run.inputs.is_empty()
            && run.assignment.followup.is_none()
            && resumed
            && review_artifact.is_none()
        {
            return Ok(None);
        }
        let mut statement = self.0.prepare(
            "SELECT operation FROM local_task_intents
             WHERE (assignment_id = ?1 OR assignment_id IN (
                SELECT id FROM received_assignments WHERE task_id = ?2
             )) AND json_extract(operation, '$.action.kind') IN ('delegate', 'request_review')
             ORDER BY rowid",
        )?;
        let followup = run.assignment.followup.as_ref();
        let previous = followup.map(|followup| followup.previous_task_id.as_str());
        let delegations = statement
            .query_map(
                [
                    Some(run.assignment.placement.assignment_id.as_str()),
                    previous,
                ],
                |row| row.get::<_, String>(0),
            )?
            .map(|value| Ok(serde_json::from_str::<WorkOperation>(&value?)?))
            .collect::<Result<Vec<_>>>()?;
        // A replacement session also needs the previous inputs and accepted
        // operations, without repeating large histories on native reuse.
        let recovery = if !resumed {
            let mut query = self.0.prepare(
                "SELECT payload FROM local_issue_inputs
                 WHERE assignment_id = ?1 AND run_id IS NOT NULL AND run_id != ?2
                 ORDER BY rowid DESC LIMIT 32",
            )?;
            let prior = query
                .query_map([&run.assignment.placement.assignment_id, &run.id], |row| {
                    row.get::<_, String>(0)
                })?
                .map(|raw| Ok(serde_json::from_str::<IssueInput>(&raw?)?))
                .collect::<Result<Vec<_>>>()?;
            let mut query = self.0.prepare(
                "SELECT operation FROM local_task_intents WHERE assignment_id = ?1 AND
                 json_extract(operation, '$.action.kind') != 'observe_subagent'
                 ORDER BY rowid DESC LIMIT 32",
            )?;
            let receipts = query
                .query_map([&run.assignment.placement.assignment_id], |row| {
                    row.get::<_, String>(0)
                })?
                .map(|raw| Ok(serde_json::from_str::<WorkOperation>(&raw?)?))
                .collect::<Result<Vec<_>>>()?;
            (!prior.is_empty() || !receipts.is_empty()).then(|| {
                serde_json::json!({
                    "prior_inputs_recent_first": prior, "prior_receipts_recent_first": receipts,
                })
            })
        } else {
            None
        };
        if run.inputs.is_empty()
            && followup.is_none()
            && recovery.is_none()
            && review_artifact.is_none()
        {
            return Ok(None);
        }
        let records = serde_json::json!({
            "child_inputs": run.inputs,
            "previous_delegations": delegations,
            "followup": followup,
            "recovery": recovery,
            "bound_review_artifact": review_artifact,
        });
        Ok(Some(format!(
            "Read these durable coordination records, including any previous Task and current Issue state.\n{records}"
        )))
    }

    pub fn accept_operation(
        &mut self,
        run: &LocalRun,
        call: &ToolCall,
        build: impl FnOnce(&str) -> Result<WorkAction>,
    ) -> Result<Option<WorkOperation>> {
        let request = serde_json::to_string(&(&call.tool, &call.arguments))?;
        let prior = self.0.query_row(
            "SELECT request, operation FROM local_work_operations WHERE run_id = ?1 AND call_id = ?2",
            params![run.id, call.call_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        ).optional()?;
        if let Some((saved, operation)) = prior {
            return if saved == request {
                Ok(Some(serde_json::from_str(&operation)?))
            } else {
                Ok(None)
            };
        }

        let operation_id = protocol::new_id();
        let mut action = build(&operation_id)?;
        let assignment_id = &run.assignment.placement.assignment_id;
        if let WorkAction::RequestReview {
            issue_id,
            task_id,
            candidate,
            rereview: true,
            ..
        } = &mut action
        {
            let prior: Option<String> = self.0.query_row(
                "SELECT payload FROM local_issue_inputs WHERE assignment_id = ?1 AND run_id IS NOT NULL
                  AND json_extract(payload, '$.kind') = 'review_finished' AND json_extract(payload, '$.child_task_id') = ?2
                 ORDER BY rowid DESC LIMIT 1",
                params![assignment_id, task_id.as_str()], |row| row.get(0),
            ).optional()?;
            let local: Option<IssueInput> =
                prior.as_deref().map(serde_json::from_str).transpose()?;
            let prior = local.as_ref().or_else(|| {
                run.assignment
                    .review_history
                    .iter()
                    .find(|input| input.child_task_id == *task_id)
            });
            let Some(IssueInput {
                child_issue_id,
                cause: protocol::IssueInputCause::ReviewFinished { review, .. },
                ..
            }) = prior
            else {
                anyhow::bail!(
                    "Use the Review Task ID from a completed review returned to this author Task"
                );
            };
            ensure!(
                review.author_task_id == run.assignment.task_id && review.candidate != *candidate,
                "Submit a new candidate for a review requested by this author Task"
            );
            issue_id.clone_from(child_issue_id);
            let requests = self.0.prepare(
                "SELECT operation FROM local_work_operations WHERE run_id = ?1 AND json_extract(operation, '$.action.kind') = 'request_review'
                     AND json_extract(operation, '$.action.task_id') = ?2",
            )?.query_map(params![run.id, task_id.as_str()], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if let Some(original) = requests.first() {
                let original: WorkOperation = serde_json::from_str(original)?;
                ensure!(
                    original.action == action,
                    "This Run already requested a round on that Review Task"
                );
                return Ok(Some(original));
            }
        }
        if let WorkAction::DeliverReview {
            request_id: Some(request),
            result,
        } = &action
        {
            let prior: Option<String> = self
                .0
                .query_row(
                    "SELECT operation FROM local_work_operations WHERE assignment_id = ?1
                  AND json_extract(operation, '$.action.kind') = 'deliver_review'
                  AND json_extract(operation, '$.action.request_id') = ?2 ORDER BY rowid LIMIT 1",
                    params![assignment_id, request],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(prior) = prior {
                let prior: WorkOperation = serde_json::from_str(&prior)?;
                ensure!(
                    matches!(&prior.action, WorkAction::DeliverReview { result: original, .. } if original == result),
                    "The original round already has a different recorded conclusion"
                );
                return Ok(Some(prior));
            }
        }
        if let WorkAction::RequestReview { candidate, .. } = &action
            && let Some(id) = candidate.strip_prefix("artifact:")
        {
            ensure!(protocol::valid_id(id) && self.0.query_row(
                "SELECT EXISTS(SELECT 1 FROM local_task_intents o JOIN received_assignments a ON a.id = o.assignment_id
                   WHERE o.id = ?1 AND a.task_id = ?2 AND json_extract(o.operation, '$.action.kind') = 'submit_artifact')",
                params![id, run.assignment.task_id], |row| row.get::<_, bool>(0),
            )?, "Select an immutable artifact submitted by this Task");
        }
        if let WorkAction::WatchExternal {
            condition,
            mode: protocol::WatchMode::Wait,
        } = &action
            && (condition.head_sha.is_some()
                || condition.event == protocol::ExternalKind::Pull && condition.state == "merged")
        {
            let mut saved = self.0.prepare(
                "SELECT payload FROM local_issue_inputs WHERE assignment_id = ?1
                 AND json_extract(payload, '$.kind') = 'external_fact' ORDER BY rowid DESC",
            )?;
            let observations = saved
                .query_map([&run.assignment.placement.assignment_id], |row| {
                    row.get::<_, String>(0)
                })?;
            for observed in observations {
                let input: IssueInput = serde_json::from_str(&observed?)?;
                if let protocol::IssueInputCause::ExternalFact {
                    fact_id,
                    fact,
                    error: None,
                    ..
                } = input.cause
                    && condition.matches(&fact)
                {
                    action = WorkAction::ConfirmExternal { fact_id, fact };
                    break;
                }
            }
        }
        if let WorkAction::RecordReview { review } = &action {
            let confirmed = self.0.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM local_work_operations o JOIN received_assignments a ON a.id = o.assignment_id
                     WHERE json_extract(a.contract, '$.issue_id') = ?1
                       AND json_extract(o.operation, '$.action.kind') = 'observe_subagent'
                       AND EXISTS(SELECT 1 FROM json_each(o.operation, '$.action.observation.receiverThreadIds') child
                                  WHERE child.value = ?2))",
                params![run.assignment.issue_id, review.child_thread_id], |row| row.get::<_, bool>(0),
            )?;
            ensure!(confirmed, "Reference a child thread observed in this Issue");
        }
        if let WorkAction::ForwardIssueMessage {
            child_issue_id,
            message_id,
        } = &action
        {
            let known = self.0.query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM local_issue_inputs i JOIN received_assignments a ON a.id = i.assignment_id
                   WHERE json_extract(i.payload, '$.message.id') = ?1
                     AND json_extract(i.payload, '$.message.issue_id') = ?3
                     AND json_extract(i.payload, '$.message.kind') IN ('user', 'relay')
                     AND json_extract(a.contract, '$.issue_id') = ?3
                ) AND EXISTS(
                   SELECT 1 FROM local_task_intents o JOIN received_assignments a ON a.id = o.assignment_id
                   WHERE json_extract(o.operation, '$.action.kind') = 'delegate'
                     AND json_extract(o.operation, '$.action.issue_id') = ?2
                     AND json_extract(a.contract, '$.issue_id') = ?3
                )",
                params![message_id, child_issue_id, run.assignment.issue_id],
                |row| row.get::<_, bool>(0),
            )?;
            ensure!(
                known,
                "Forward a user message received by this Issue to one of its work children"
            );
        }
        if let WorkAction::PostIssueMessage {
            reply_to: Some(reply),
            ..
        } = &action
        {
            let known: bool = self.0.query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM local_issue_inputs i JOIN received_assignments a ON a.id = i.assignment_id
                   WHERE json_extract(i.payload, '$.message.id') = ?1 AND json_extract(a.contract, '$.issue_id') = ?2
                   UNION ALL
                   SELECT 1 FROM local_task_intents o JOIN received_assignments a ON a.id = o.assignment_id
                   WHERE o.id = ?1 AND json_extract(o.operation, '$.action.kind') = 'post_issue_message'
                     AND json_extract(a.contract, '$.issue_id') = ?2
                )", params![reply, run.assignment.issue_id], |row| row.get(0),
            )?;
            ensure!(known, "Reply to a message received or posted by this Issue");
        }
        if let WorkAction::EscalateAttention { attention_id }
        | WorkAction::AnswerChildAttention { attention_id, .. } = &action
        {
            let known = self.0.query_row(
                "SELECT EXISTS(SELECT 1 FROM local_issue_inputs i JOIN received_assignments a ON a.id = i.assignment_id
                    WHERE i.id = ?1 AND json_extract(i.payload, '$.kind') = 'attention_requested'
                    AND json_extract(a.contract, '$.issue_id') = ?2)",
                params![attention_id, run.assignment.issue_id], |row| row.get::<_, bool>(0),
            )?;
            ensure!(
                known,
                "Question {attention_id} has not been received by this Issue"
            );
        }
        if let WorkAction::ReportIssue { artifact_ids, .. } = &action {
            let mut known = self.0.prepare(
                "SELECT EXISTS(SELECT 1 FROM local_issue_inputs i JOIN received_assignments a ON a.id = i.assignment_id
                 WHERE i.id = ?1 AND json_extract(i.payload, '$.kind') = 'artifact_available'
                 AND json_extract(a.contract, '$.issue_id') = ?2)",
            )?;
            for id in artifact_ids {
                ensure!(
                    known.query_row(params![id, run.assignment.issue_id], |row| row
                        .get::<_, bool>(0))?,
                    "Delivery {id} has not been received by this Issue"
                );
            }
        }
        if let WorkAction::AskAttention {
            proposal: Some(proposal),
            ..
        } = &action
        {
            let known = self.0.query_row(
                "SELECT EXISTS(SELECT 1 FROM local_work_operations o JOIN received_assignments a ON a.id = o.assignment_id
                 WHERE o.id = ?1 AND a.task_id = ?2 AND json_extract(o.operation, '$.action.kind') = 'report_issue'
                 AND EXISTS (SELECT 1 FROM json_each(o.operation, '$.action.artifact_ids') WHERE value = ?3))",
                params![proposal.report_id, run.assignment.task_id, proposal.artifact_id],
                |row| row.get::<_, bool>(0),
            )?;
            ensure!(
                known,
                "Select the Markdown attachment of a report returned by this Task"
            );
        }
        let operation = WorkOperation {
            operation_id,
            assignment_id: run.assignment.placement.assignment_id.clone(),
            run_id: run.id.clone(),
            action,
        };
        let serialized = serde_json::to_string(&operation)?;
        let tx = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO local_work_operations VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                operation.operation_id,
                operation.assignment_id,
                operation.run_id,
                call.call_id,
                request,
                serialized
            ],
        )?;
        tx.execute(
            "INSERT INTO run_outbox (id, assignment_id, observation, record_kind)
             VALUES (?1, ?2, ?3, 'operation')",
            params![operation.operation_id, operation.assignment_id, serialized],
        )?;
        tx.commit()?;
        Ok(Some(operation))
    }
}

pub(super) fn accept_input(db: &Connection, delivery: &IssueDelivery) -> Result<()> {
    let IssueDelivery {
        assignment_id,
        input,
    } = delivery;
    let accepted: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM received_assignments WHERE id = ?1 AND execution_state != 'rejected')",
        [assignment_id], |row| row.get(0),
    )?;
    if !accepted {
        return Ok(());
    }
    let payload = serde_json::to_string(input)?;
    let inserted = db.execute(
        "INSERT INTO local_issue_inputs (id, assignment_id, child_issue_id, payload)
         VALUES (?1, ?2, ?3, ?4) ON CONFLICT(id) DO NOTHING",
        params![input.input_id, assignment_id, input.child_issue_id, payload],
    )?;
    if inserted == 0 {
        let (original, assignee, run_id): (String, String, Option<String>) = db.query_row(
            "SELECT payload, assignment_id, run_id FROM local_issue_inputs WHERE id = ?1",
            [&input.input_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        ensure!(
            *input == serde_json::from_str::<IssueInput>(&original)?,
            "parent input changes its original fact"
        );
        if assignment_id == &assignee || run_id.is_some() {
            return Ok(());
        }
        db.execute(
            "UPDATE local_issue_inputs SET assignment_id = ?2 WHERE id = ?1",
            params![input.input_id, assignment_id],
        )?;
    }
    db.execute(
        "UPDATE received_assignments SET execution_state = 'queued'
         WHERE id = ?1 AND (execution_state IN ('waiting', 'waiting_external')
            OR (?2 AND execution_state = 'finished'))",
        params![
            assignment_id,
            matches!(
                input.cause,
                protocol::IssueInputCause::ReviewRequested { .. }
            )
        ],
    )?;
    Ok(())
}

pub(super) fn run_inputs(db: &Connection, run: &str) -> Result<Vec<IssueInput>> {
    let mut statement =
        db.prepare("SELECT payload FROM local_issue_inputs WHERE run_id = ?1 ORDER BY rowid")?;
    statement
        .query_map([run], |row| row.get::<_, String>(0))?
        .map(|row| Ok(serde_json::from_str(&row?)?))
        .collect()
}

pub(super) fn has_unread_input(db: &Connection, assignment: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM local_issue_inputs WHERE assignment_id = ?1 AND run_id IS NULL)",
        [assignment], |row| row.get(0),
    )?)
}

pub(super) fn waiting_for_attention(db: &Connection, assignment: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM local_task_intents o
         WHERE o.assignment_id = ?1 AND json_extract(o.operation, '$.action.kind') = 'ask_attention'
           AND json_extract(o.operation, '$.action.proposal.replaces') IS NULL
           AND NOT EXISTS(SELECT 1 FROM local_issue_inputs i
                WHERE i.assignment_id = o.assignment_id AND i.run_id IS NOT NULL
                  AND json_extract(i.payload, '$.kind') = 'attention_answered'
                  AND json_extract(i.payload, '$.attention_id') = o.id))",
        [assignment],
        |row| row.get(0),
    )?)
}

pub(super) fn has_followup(db: &Connection, assignment: &str) -> Result<bool> {
    let closed: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM local_work_operations
         WHERE assignment_id = ?1 AND json_extract(operation, '$.action.kind') = 'close_issue')",
        [assignment],
        |row| row.get(0),
    )?;
    if closed {
        return Ok(false);
    }
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM local_task_intents o
         WHERE o.assignment_id = ?1 AND json_extract(o.operation, '$.action.kind') = 'delegate'
           AND NOT EXISTS(SELECT 1 FROM local_issue_inputs i WHERE i.assignment_id = o.assignment_id
               AND i.child_issue_id = json_extract(o.operation, '$.action.issue_id')
               AND json_extract(i.payload, '$.kind') IN ('issue_closed', 'task_stopped', 'task_cancelled')))",
        [assignment],
        |row| row.get(0),
    )?)
}

pub(super) fn waiting_for_review(db: &Connection, assignment: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM local_task_intents o
         WHERE o.assignment_id = ?1 AND json_extract(o.operation, '$.action.kind') = 'request_review'
           AND NOT EXISTS(SELECT 1 FROM local_work_operations c WHERE c.assignment_id = ?1
                 AND c.rowid > o.rowid AND json_extract(c.operation, '$.action.kind') = 'close_issue')
           AND NOT EXISTS(SELECT 1 FROM local_issue_inputs i WHERE i.assignment_id = ?1
                 AND i.run_id IS NOT NULL AND json_extract(i.payload, '$.kind') = 'review_finished'
                 AND json_extract(i.payload, '$.review.request_id') = o.id))",
        [assignment], |row| row.get(0),
    )?)
}

pub(super) fn waiting_for_external(db: &Connection, assignment: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM local_task_intents o
          WHERE o.assignment_id = ?1 AND o.rowid > coalesce((
             SELECT MAX(c.rowid) FROM local_task_intents c WHERE c.assignment_id = ?1
               AND json_extract(c.operation, '$.action.kind') IN ('clear_external_waits','close_issue')
          ), -2000000000) AND json_extract(o.operation, '$.action.kind') = 'watch_external'
            AND json_extract(o.operation, '$.action.mode') = 'wait'
            AND NOT EXISTS(SELECT 1 FROM local_issue_inputs i WHERE i.assignment_id = ?1
                AND i.run_id IS NOT NULL AND json_extract(i.payload, '$.kind') = 'external_fact'
                AND json_extract(i.payload, '$.watch_id') = o.id))",
        [assignment], |row| row.get(0),
    )?)
}
