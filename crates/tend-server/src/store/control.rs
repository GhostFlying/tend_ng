use super::{Accepted, Store};
use anyhow::Result;
use protocol::{
    ControlAck, ControlAction, ControlState, ControlView, IssueTransition, RunnerControl,
    SendMessage, TaskAssignment,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

pub enum ResultState<T> {
    Found(T),
    Conflict,
    Missing,
}

pub fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE task_controls (
            id TEXT PRIMARY KEY, task_id TEXT NOT NULL REFERENCES tasks(id),
            assignment_id TEXT REFERENCES runner_assignments(id), run_id TEXT,
            action TEXT NOT NULL, state TEXT NOT NULL
         );
         CREATE INDEX task_controls_pending ON task_controls(assignment_id) WHERE state = 'pending';
         CREATE TABLE issue_transitions (
            id TEXT PRIMARY KEY, issue_id TEXT NOT NULL REFERENCES issues(id), state TEXT NOT NULL, reason TEXT NOT NULL
         );",
    )?;
    Ok(())
}

fn state(db: &Connection, id: &str) -> Result<Option<(ControlView, String)>> {
    Ok(db
        .query_row(
            "SELECT task_id, run_id, state, action FROM task_controls WHERE id = ?1",
            [id],
            |row| {
                Ok((
                    ControlView {
                        operation_id: id.to_owned(),
                        task_id: row.get(0)?,
                        run_id: row.get(1)?,
                        state: row.get(2)?,
                    },
                    row.get(3)?,
                ))
            },
        )
        .optional()?)
}

pub fn submit(
    db: &Connection,
    operation: &str,
    task: &str,
    run: Option<&str>,
    action: &ControlAction,
) -> Result<ResultState<ControlView>> {
    let raw = serde_json::to_string(action)?;
    if let Some((original, saved)) = state(db, operation)? {
        return Ok(
            if raw == saved && original.task_id == task && original.run_id.as_deref() == run {
                ResultState::Found(original)
            } else {
                ResultState::Conflict
            },
        );
    }
    let actual = db
        .query_row(
            "SELECT t.current_assignment, t.status, a.state FROM tasks t
         LEFT JOIN runner_assignments a ON a.id = t.current_assignment WHERE t.id = ?1",
            [task],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((mut assignment, task_status, assignment_status)) = actual else {
        return Ok(ResultState::Missing);
    };
    if matches!(action, ControlAction::Stop) {
        super::coordination::flush_pending(db, task)?;
    }
    let control = match action {
        ControlAction::Stop
            if matches!(task_status.as_str(), "completed" | "failed" | "cancelled") =>
        {
            ControlState::Stopped
        }
        ControlAction::Stop
            if assignment.is_none() || assignment_status.as_deref() == Some("reserved") =>
        {
            if let Some(reserved) = assignment.take() {
                db.execute(
                    "UPDATE runner_assignments SET state = 'revoked' WHERE id = ?1",
                    [&reserved],
                )?;
            }
            db.execute(
                "UPDATE tasks SET status = 'cancelled', current_assignment = NULL WHERE id = ?1",
                [task],
            )?;
            super::external::clear_waits(db, task)?;
            report_unstarted_stop(db, task, operation)?;
            ControlState::Stopped
        }
        ControlAction::Stop => ControlState::Pending,
        ControlAction::Steer { .. } => {
            let live = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM task_runs WHERE id = ?1 AND assignment_id = ?2
                     AND json_extract(observation, '$.phase.state') = 'started')",
                params![run, assignment],
                |row| row.get::<_, bool>(0),
            )?;
            if live && task_status == "running" {
                super::coordination::flush_pending(db, task)?;
                ControlState::Pending
            } else {
                ControlState::Undelivered
            }
        }
    };
    db.execute(
        "INSERT INTO task_controls VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![operation, task, assignment, run, raw, control.as_str()],
    )?;
    if matches!(action, ControlAction::Stop) && control == ControlState::Stopped {
        super::external::route_stopped_required(db, task)?;
    }
    Ok(ResultState::Found(ControlView {
        operation_id: operation.to_owned(),
        task_id: task.to_owned(),
        run_id: run.map(str::to_owned),
        state: control.as_str().to_owned(),
    }))
}

pub fn pending(
    db: &Connection,
    runner: &str,
    task: Option<&str>,
    assignments: &[TaskAssignment],
) -> Result<Vec<RunnerControl>> {
    let included = serde_json::to_string(
        &assignments
            .iter()
            .map(|a| &a.placement.assignment_id)
            .collect::<Vec<_>>(),
    )?;
    let mut q = db.prepare(
        "WITH page AS MATERIALIZED (
            SELECT c.rowid FROM task_controls c JOIN runner_assignments a ON a.id = c.assignment_id
            WHERE a.runner_id = ?1 AND c.state = 'pending' AND (?2 IS NULL OR c.task_id = ?2)
            ORDER BY c.rowid LIMIT ?3
         ), paired AS (
            SELECT min(c.rowid) AS rowid FROM json_each(?4) a JOIN task_controls c ON c.assignment_id = a.value
            WHERE c.state = 'pending' AND c.action->>'kind' = 'stop' GROUP BY c.assignment_id
         )
         SELECT c.id, c.task_id, c.assignment_id, c.run_id, c.action FROM task_controls c
         WHERE c.rowid IN (SELECT rowid FROM page UNION SELECT rowid FROM paired)
         ORDER BY c.rowid",
    )?;
    q.query_map(
        params![runner, task, protocol::RUNNER_SYNC_BATCH, included],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get::<_, String>(4)?,
            ))
        },
    )?
    .map(|row| {
        let (id, task_id, assignment_id, run_id, action) = row?;
        Ok(RunnerControl {
            id,
            task_id,
            assignment_id,
            run_id,
            action: serde_json::from_str(&action)?,
        })
    })
    .collect()
}

pub(super) fn stop_on_current(db: &Connection, task: &str, state: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM tasks t JOIN task_controls c ON c.assignment_id = t.current_assignment
           WHERE t.id = ?1 AND c.task_id = t.id AND c.state = ?2 AND c.action->>'kind' = 'stop')",
        params![task, state], |r| r.get(0),
    )?)
}

pub fn acknowledge(db: &Connection, runner: &str, ack: &ControlAck) -> Result<bool> {
    let saved = db.query_row(
        "SELECT c.task_id, c.state, c.action FROM task_controls c JOIN runner_assignments a ON a.id = c.assignment_id
         WHERE c.id = ?1 AND c.assignment_id = ?2 AND a.runner_id = ?3",
        params![ack.id, ack.assignment_id, runner],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)),
    ).optional()?;
    let Some((task, current, raw)) = saved else {
        return Ok(false);
    };
    let action: ControlAction = serde_json::from_str(&raw)?;
    if current != "pending" {
        return Ok(current == ack.state.as_str()
            || current == "queued" && ack.state == ControlState::Undelivered);
    }
    match (&action, ack.state) {
        (ControlAction::Stop, ControlState::Stopped) => {
            let changed = db.execute(
                "UPDATE tasks SET status = 'cancelled' WHERE id = ?1 AND current_assignment = ?2
                        AND status NOT IN ('completed', 'failed', 'cancelled')",
                params![task, ack.assignment_id],
            )?;
            if changed > 0 {
                report_unstarted_stop(db, &task, &ack.id)?;
            }
            super::external::clear_waits(db, &task)?;
            if db.query_row(
                "SELECT current_assignment IS ?2 OR (current_assignment IS NULL AND status = 'cancelled')
                   FROM tasks WHERE id = ?1", params![task, ack.assignment_id], |r| r.get::<_, bool>(0),
            )? {
                super::external::route_stopped_required(db, &task)?;
            }
        }
        (
            ControlAction::Steer { .. },
            ControlState::Delivered | ControlState::Undelivered | ControlState::Unknown,
        ) => {}
        _ => return Ok(false),
    }
    db.execute(
        "UPDATE task_controls SET state = ?2 WHERE id = ?1",
        params![ack.id, ack.state.as_str()],
    )?;
    Ok(true)
}

/// The original Runner proved the assignment was never accepted or was
/// handed off without a live Run. There is no local Stop receipt to wait for.
pub(super) fn settle_proven_stops(db: &Connection, runner: &str, assignment: &str) -> Result<bool> {
    let ids = {
        let mut statement = db.prepare(
            "SELECT id FROM task_controls WHERE assignment_id = ?1 AND state = 'pending'
               AND json_extract(action, '$.kind') = 'stop' ORDER BY rowid",
        )?;
        statement
            .query_map([assignment], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for id in &ids {
        anyhow::ensure!(
            acknowledge(
                db,
                runner,
                &ControlAck {
                    event_id: String::new(),
                    id: id.clone(),
                    assignment_id: assignment.to_owned(),
                    state: ControlState::Stopped,
                }
            )?,
            "stop no longer matches its proven assignment"
        );
    }
    Ok(!ids.is_empty())
}

fn report_unstarted_stop(db: &Connection, task: &str, control: &str) -> Result<()> {
    super::tend_review::finish(
        db,
        task,
        None,
        control,
        None,
        None,
        "Review Task stopped before another execution could start.",
    )?;
    let parent: Option<(String, String)> = db
        .query_row(
            "SELECT i.id, i.parent_issue_id FROM tasks t JOIN issues i ON i.id = t.issue_id
         WHERE t.id = ?1 AND i.parent_issue_id IS NOT NULL",
            [task],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((child, parent)) = parent
        && !super::coordination::review_reports_directly(db, task, &parent)?
        && let Some(owner) = super::coordination::target_task(db, &parent)?
    {
        super::coordination::enqueue(
            db,
            &owner,
            protocol::IssueInput {
                input_id: protocol::new_id(),
                child_issue_id: child,
                child_task_id: task.to_owned(),
                summary: "The child Task stopped before another execution could start.".to_owned(),
                cause: protocol::IssueInputCause::TaskCancelled {
                    control_id: control.to_owned(),
                },
            },
        )?;
    }
    Ok(())
}

pub fn transition(
    db: &Connection,
    issue: &str,
    request: &IssueTransition,
) -> Result<ResultState<()>> {
    let prior: Option<(String, String, String)> = db
        .query_row(
            "SELECT issue_id, state, reason FROM issue_transitions WHERE id = ?1",
            [&request.operation_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    if let Some((original, state, reason)) = prior {
        return Ok(
            if original == issue && state == request.state && reason == request.reason {
                ResultState::Found(())
            } else {
                ResultState::Conflict
            },
        );
    }
    if db.execute(
        "UPDATE issues SET state = ?2 WHERE id = ?1",
        params![issue, request.state],
    )? == 0
    {
        return Ok(ResultState::Missing);
    }
    db.execute(
        "INSERT INTO issue_transitions VALUES (?1, ?2, ?3, ?4)",
        params![request.operation_id, issue, request.state, request.reason],
    )?;
    if let Some(task) = db
        .query_row(
            "SELECT id FROM tasks WHERE issue_id = ?1 ORDER BY rowid DESC LIMIT 1",
            [issue],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    {
        super::coordination::flush_pending(db, &task)?;
    }
    Ok(ResultState::Found(()))
}

impl Store {
    pub fn new_control(
        &mut self,
        id: &str,
        requested: &str,
        action: &ControlAction,
    ) -> Result<ResultState<ControlView>> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (task, run) = match action {
            ControlAction::Stop => (requested.to_owned(), None),
            ControlAction::Steer { .. } => {
                let task: Option<String> = tx
                    .query_row(
                        "SELECT task_id FROM task_runs WHERE id = ?1",
                        [requested],
                        |row| row.get(0),
                    )
                    .optional()?;
                let Some(task) = task else {
                    return Ok(ResultState::Missing);
                };
                (task, Some(requested))
            }
        };
        let result = submit(&tx, id, &task, run, action)?;
        super::board::commit(tx)?;
        Ok(result)
    }

    pub fn control(&self, id: &str) -> Result<Option<ControlView>> {
        Ok(state(&self.db, id)?.map(|(view, _)| view))
    }

    pub fn transition_issue(
        &mut self,
        issue: &str,
        request: &IssueTransition,
    ) -> Result<ResultState<()>> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let prior = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM issue_transitions WHERE id = ?1)",
            [&request.operation_id],
            |row| row.get::<_, bool>(0),
        )?;
        let result = transition(&tx, issue, request)?;
        if !prior && request.state == "cancelled" && matches!(result, ResultState::Found(())) {
            let tasks = {
                let mut query = tx.prepare(
                    "WITH RECURSIVE tree(id) AS (SELECT ?1 UNION ALL
                       SELECT i.id FROM issues i JOIN tree p ON i.parent_issue_id = p.id)
                     SELECT t.id FROM tasks t JOIN tree ON tree.id = t.issue_id
                       WHERE t.status NOT IN ('completed', 'failed', 'cancelled')",
                )?;
                query
                    .query_map([issue], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            };
            for task in tasks {
                submit(&tx, &protocol::new_id(), &task, None, &ControlAction::Stop)?;
            }
        }
        super::board::commit(tx)?;
        Ok(result)
    }

    pub fn queue_steer(&mut self, id: &str) -> Result<ResultState<ControlView>> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some((mut view, raw)) = state(&tx, id)? else {
            return Ok(ResultState::Missing);
        };
        if !matches!(view.state.as_str(), "undelivered" | "queued") {
            return Ok(ResultState::Conflict);
        }
        let ControlAction::Steer { text } = serde_json::from_str(&raw)? else {
            return Ok(ResultState::Conflict);
        };
        let issue: String = tx.query_row(
            "SELECT issue_id FROM tasks WHERE id = ?1",
            [&view.task_id],
            |row| row.get(0),
        )?;
        let message = SendMessage {
            operation_id: format!("steer-{id}"),
            text,
            reply_to: None,
        };
        if !matches!(
            super::issue_chat::post_message(&tx, &issue, &message)?,
            Some(Accepted::Received(_))
        ) {
            return Ok(ResultState::Conflict);
        }
        tx.execute(
            "UPDATE task_controls SET state = 'queued' WHERE id = ?1 AND state = 'undelivered'",
            [id],
        )?;
        view.state = "queued".to_owned();
        super::board::commit(tx)?;
        Ok(ResultState::Found(view))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unstarted_bound_review_only_wakes_its_direct_author_with_the_review_result() -> Result<()> {
        let store = Store::initialize(std::path::Path::new(":memory:"))?;
        store.db.execute_batch(
            "INSERT INTO chat_messages(seq,id,chat_id,text) VALUES (1,'source','main','Review');
             INSERT INTO runners(id,host_id,profile,last_seen) VALUES ('runner','host','{}',0);
             INSERT INTO issues(id,source_seq,title,objective,parent_issue_id) VALUES
                 ('author',1,'Work','Own the original Issue',NULL),
                 ('review',1,'Review','Independent review','author');
             INSERT INTO tasks(id,issue_id,category,status,repo_ids,required_capabilities) VALUES
                 ('author-task','author','implementation','waiting','[]','[]'),
                 ('review-task','review','review','awaiting_assignment','[]','[]');
             INSERT INTO runner_operations(id,runner_id,operation) VALUES ('request','runner','{}');",
        )?;
        super::super::tend_review::bind(
            &store.db,
            "request",
            "author",
            "author-task",
            "review",
            "review-task",
            "candidate",
            "scope",
            "workspaces/author",
        )?;
        let result = submit(
            &store.db,
            "control",
            "review-task",
            None,
            &ControlAction::Stop,
        )?;
        assert!(
            matches!(result, ResultState::Found(control) if control.state == ControlState::Stopped.as_str())
        );
        let (task, payload): (String, String) = store.db.query_row(
            "SELECT task_id, payload FROM issue_inputs WHERE id = (SELECT result_input_id FROM tend_reviews)",
            [], |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(task, "author-task");
        let input: protocol::IssueInput = serde_json::from_str(&payload)?;
        assert!(
            matches!(input.cause, protocol::IssueInputCause::ReviewFinished {
            result: None, source_id, .. } if source_id == "control")
        );
        let inputs: u32 = store
            .db
            .query_row("SELECT count(*) FROM issue_inputs", [], |row| row.get(0))?;
        assert_eq!(inputs, 1);
        Ok(())
    }
}
