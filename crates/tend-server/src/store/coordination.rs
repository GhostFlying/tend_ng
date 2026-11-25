use anyhow::Result;
use protocol::{
    IssueDelivery, IssueInput, IssueInputCause, RUNNER_SYNC_BATCH, RunObservation, RunOutcome,
    RunPhase, RunnerProfile, TaskAssignment, WORK_CATEGORIES, WorkAction, WorkOperation,
    matching_task_agent,
};
use rusqlite::{Connection, OptionalExtension, params};

pub fn create_tables(db: &Connection) -> Result<()> {
    db.execute_batch(
        "ALTER TABLE issues ADD COLUMN parent_issue_id TEXT REFERENCES issues(id);
         ALTER TABLE tasks ADD COLUMN pinned_runner_id TEXT REFERENCES runners(id);
         CREATE TABLE runner_operations (
            id TEXT PRIMARY KEY,
            runner_id TEXT NOT NULL REFERENCES runners(id),
            operation TEXT NOT NULL
         );
         CREATE TABLE child_dispatches (
            task_id TEXT PRIMARY KEY REFERENCES tasks(id),
            operation_id TEXT NOT NULL UNIQUE REFERENCES runner_operations(id)
         );
         CREATE TABLE issue_inputs (
            id TEXT PRIMARY KEY,
            assignment_id TEXT NOT NULL REFERENCES runner_assignments(id),
            payload TEXT NOT NULL,
            run_id TEXT REFERENCES task_runs(id)
         );
         CREATE INDEX pending_issue_inputs ON issue_inputs(assignment_id) WHERE run_id IS NULL;",
    )?;
    Ok(())
}

pub(super) fn migrate_input_targets(db: &Connection) -> Result<()> {
    db.execute_batch(
        "ALTER TABLE tasks ADD COLUMN pinned_agent_id TEXT;
         ALTER TABLE tasks ADD COLUMN previous_task_id TEXT REFERENCES tasks(id);
         CREATE INDEX tasks_by_issue ON tasks(issue_id);
         ALTER TABLE issue_inputs RENAME TO old_issue_inputs;
         CREATE TABLE issue_inputs (
            id TEXT PRIMARY KEY,
            task_id TEXT NOT NULL REFERENCES tasks(id),
            payload TEXT NOT NULL,
            run_id TEXT REFERENCES task_runs(id)
         );
         INSERT INTO issue_inputs(id, task_id, payload, run_id)
         SELECT o.id, a.task_id, o.payload, o.run_id FROM old_issue_inputs o
         JOIN runner_assignments a ON a.id = o.assignment_id ORDER BY o.rowid;
         DROP TABLE old_issue_inputs;
         CREATE INDEX pending_issue_inputs ON issue_inputs(task_id) WHERE run_id IS NULL;",
    )?;
    Ok(())
}

const CHILD_WINDOW_MS: i64 = 300_000;
const NOW_MS: &str = "CAST(unixepoch('subsec') * 1000 AS INTEGER)";

pub(super) fn migrate_continuity(db: &Connection) -> Result<()> {
    db.execute_batch(
        "ALTER TABLE issue_inputs ADD COLUMN wake_at INTEGER;
         CREATE INDEX pending_child_windows ON issue_inputs(task_id, wake_at)
           WHERE run_id IS NULL AND wake_at IS NOT NULL;
         CREATE INDEX due_child_windows ON issue_inputs(wake_at, task_id)
           WHERE run_id IS NULL AND wake_at IS NOT NULL;",
    )?;
    Ok(())
}

pub fn apply(db: &Connection, runner: &RunnerProfile, operation: &WorkOperation) -> Result<bool> {
    let runner_id = runner.runner_id.as_str();
    let serialized = serde_json::to_string(operation)?;
    let prior = db
        .query_row(
            "SELECT runner_id, operation FROM runner_operations WHERE id = ?1",
            [&operation.operation_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    if let Some((origin, content)) = prior {
        return Ok(origin == runner_id && serialized == content);
    }

    let source = db
        .query_row(
            "SELECT i.id, i.source_seq, i.parent_issue_id, t.category, t.id
         FROM task_runs r JOIN runner_assignments a ON a.id = r.assignment_id
         JOIN tasks t ON t.id = r.task_id
         JOIN issues i ON i.id = t.issue_id
         WHERE r.id = ?1 AND a.id = ?2 AND a.runner_id = ?3 AND a.state = 'accepted'",
            params![operation.run_id, operation.assignment_id, runner_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .optional()?;
    let Some((source_issue, source_message, parent_issue, category, source_task)) = source else {
        return Ok(false);
    };
    if let WorkAction::Delegate {
        category: requested,
        work,
        ..
    } = &operation.action
        && (category != "issue-router"
            || !WORK_CATEGORIES.contains(&requested.as_str())
            || work
                .agent
                .as_ref()
                .is_some_and(|agent| !agent.valid() || agent.runner_id.is_some()))
    {
        return Ok(false);
    }

    match &operation.action {
        WorkAction::RequestReview {
            issue_id,
            task_id,
            candidate,
            scope,
            work,
        } if category == "review"
            || !protocol::valid_id(issue_id)
            || !protocol::valid_id(task_id)
            || !protocol::valid_tend_review(candidate, scope, work)
            || !work.repo_ids.iter().all(|repo| {
                db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM tasks WHERE id = ?1
                         AND EXISTS(SELECT 1 FROM json_each(tasks.repo_ids) j WHERE j.value = ?2))",
                    params![source_task, repo],
                    |row| row.get::<_, bool>(0),
                )
                .unwrap_or(false)
            }) =>
        {
            return Ok(false);
        }
        WorkAction::DeliverReview { result }
            if category != "review"
                || !result.valid()
                || super::tend_review::binding(db, &source_task)?.is_none() =>
        {
            return Ok(false);
        }
        _ => {}
    }
    if let WorkAction::RequestReview { candidate, .. } = &operation.action
        && let Some(file) = candidate.strip_prefix("artifact:")
        && (!protocol::valid_id(file) || !db.query_row(
            "SELECT EXISTS(SELECT 1 FROM artifacts WHERE id = ?1 AND issue_id = ?2 AND task_id = ?3)",
            params![file, source_issue, source_task], |row| row.get::<_, bool>(0),
        )?)
    {
        return Ok(false);
    }
    db.execute(
        "INSERT INTO runner_operations (id, runner_id, operation) VALUES (?1, ?2, ?3)",
        params![operation.operation_id, runner_id, serialized],
    )?;
    match &operation.action {
        WorkAction::Delegate {
            issue_id,
            task_id,
            category: requested,
            isolated_workspace,
            work,
        } => {
            delegate(
                db,
                runner,
                operation,
                &source_issue,
                &source_task,
                source_message,
                issue_id,
                task_id,
                requested,
                *isolated_workspace,
                work,
            )?;
        }
        WorkAction::RequestReview {
            issue_id,
            task_id,
            candidate,
            scope,
            work,
        } => {
            let coordination_parent = parent_issue.as_deref().unwrap_or(&source_issue);
            delegate(
                db,
                runner,
                operation,
                coordination_parent,
                &source_task,
                source_message,
                issue_id,
                task_id,
                "review",
                true,
                work,
            )?;
            super::tend_review::bind(
                db,
                &operation.operation_id,
                &source_issue,
                &source_task,
                issue_id,
                task_id,
                candidate,
                scope,
            )?;
        }
        WorkAction::DeliverReview { result } => {
            super::tend_review::finish(
                db,
                &source_task,
                &operation.operation_id,
                Some(&operation.run_id),
                Some(result),
                &result.conclusion,
            )?;
        }
        WorkAction::CloseIssue { summary } => {
            let changed = db.execute(
                "UPDATE issues SET state = 'closed' WHERE id = ?1 AND state = 'open'",
                [&source_issue],
            )?;
            if changed != 0
                && let Some(parent_issue) = parent_issue
                && let Some(parent_task) = latest_task(db, &parent_issue)?
            {
                enqueue(
                    db,
                    &parent_task,
                    IssueInput {
                        input_id: operation.operation_id.clone(),
                        child_issue_id: source_issue,
                        child_task_id: source_task,
                        cause: IssueInputCause::IssueClosed,
                        summary: summary.clone(),
                    },
                )?;
            }
        }
        WorkAction::SubmitArtifact { name, bytes } => {
            db.execute(
                "INSERT INTO artifacts (id, issue_id, task_id, runner_id, name, bytes)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    operation.operation_id,
                    source_issue,
                    source_task,
                    runner_id,
                    name,
                    bytes
                ],
            )?;
        }
        WorkAction::ReportIssue {
            summary,
            artifact_ids,
        } => {
            if category != "issue-router" {
                return Ok(false);
            }
            super::reports::enqueue(
                db,
                operation,
                &source_issue,
                source_message,
                summary,
                artifact_ids,
            )?;
        }
        WorkAction::AskAttention {
            request,
            proposal: Some(proposal),
        } => {
            super::proposals::register(
                db,
                operation,
                &source_issue,
                &source_task,
                source_message,
                parent_issue.as_deref(),
                request,
                proposal,
            )?;
        }
        WorkAction::AskAttention {
            request,
            proposal: None,
        } => {
            super::attention::register(
                db,
                &operation.operation_id,
                &source_issue,
                &source_task,
                source_message,
                parent_issue.as_deref(),
                request,
            )?;
        }
        WorkAction::EscalateAttention { attention_id } => {
            super::attention::apply_parent(db, &source_issue, attention_id, None)?;
        }
        WorkAction::AnswerChildAttention {
            attention_id,
            answer,
        } => {
            super::attention::apply_parent(
                db,
                &source_issue,
                attention_id,
                Some((&operation.operation_id, answer)),
            )?;
        }
        WorkAction::ForwardIssueMessage {
            child_issue_id,
            message_id,
        } => {
            super::issue_chat::forward(
                db,
                operation,
                &source_issue,
                &source_task,
                child_issue_id,
                message_id,
            )?;
        }
        WorkAction::ObserveSubagent { .. } | WorkAction::RecordReview { .. } => {
            super::review::record(db, &source_issue, operation)?;
        }
        WorkAction::StopTask { task_id } => {
            let related = db.query_row(
                "WITH RECURSIVE tree(id) AS (SELECT ?1 UNION ALL
                   SELECT i.id FROM issues i JOIN tree p ON i.parent_issue_id = p.id)
                 SELECT EXISTS(SELECT 1 FROM tasks JOIN tree ON tree.id = tasks.issue_id WHERE tasks.id = ?2)
                     OR EXISTS(SELECT 1 FROM tend_reviews WHERE review_task_id = ?2 AND author_task_id = ?3)",
                params![source_issue, task_id, source_task], |row| row.get::<_, bool>(0),
            )?;
            if related {
                super::control::submit(
                    db,
                    &operation.operation_id,
                    task_id,
                    None,
                    &protocol::ControlAction::Stop,
                )?;
            }
        }
        WorkAction::SetIssueState { state, reason } => {
            super::control::transition(
                db,
                &source_issue,
                &protocol::IssueTransition {
                    operation_id: operation.operation_id.clone(),
                    state: state.clone(),
                    reason: reason.clone(),
                },
            )?;
        }
        WorkAction::WatchExternal { condition, mode } => {
            super::external::register(
                db,
                &operation.operation_id,
                &source_issue,
                &source_task,
                condition,
                *mode,
            )?;
        }
        WorkAction::ConfirmExternal { .. } => {}
        WorkAction::ClearExternalWaits => super::external::clear_waits(db, &source_task)?,
        WorkAction::FindPull { task_id } => {
            super::discovery::queue(
                db,
                &source_issue,
                &protocol::FindPull {
                    operation_id: operation.operation_id.clone(),
                    task_id: task_id.clone(),
                },
            )?;
        }
        WorkAction::LinkPull { url } => {
            let Some(pull) = protocol::GitHubPull::parse(url) else {
                return Ok(false);
            };
            super::pull::bind(db, &source_issue, &pull)?;
        }
        WorkAction::PostIssueMessage { text, reply_to } => {
            super::issue_chat::agent_message(
                db,
                &operation.operation_id,
                &source_task,
                &operation.run_id,
                text,
                reply_to.as_deref(),
            )?;
        }
    }
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
fn delegate(
    db: &Connection,
    runner: &RunnerProfile,
    operation: &WorkOperation,
    parent_issue: &str,
    source_task: &str,
    source_message: i64,
    issue_id: &str,
    task_id: &str,
    requested: &str,
    isolated_workspace: bool,
    work: &protocol::DispatchIssue,
) -> Result<()> {
    let runner_id = &runner.runner_id;
    db.execute(
        "INSERT INTO issues (id, source_seq, title, objective, parent_issue_id)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            issue_id,
            source_message,
            work.title,
            work.objective,
            parent_issue
        ],
    )?;
    db.execute(
        "INSERT INTO tasks (id, issue_id, category, status, repo_ids, required_capabilities,
                            pinned_runner_id, workspace_key, base_refs, requested_agent, pinned_agent_id)
         VALUES (?1, ?2, ?3, 'awaiting_assignment', ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![task_id, issue_id, requested, serde_json::to_string(&work.repo_ids)?,
                serde_json::to_string(&work.required_capabilities)?, runner_id, isolated_workspace.then_some(issue_id),
                serde_json::to_string(&work.base_refs)?, work.agent.as_ref().map(serde_json::to_string).transpose()?,
                work.agent.as_ref().map(|agent| &agent.id)],
    )?;
    db.execute(
        "INSERT INTO child_dispatches (task_id, operation_id) VALUES (?1, ?2)",
        params![task_id, operation.operation_id],
    )?;
    let agent: String = db.query_row(
        "SELECT json_extract(contract, '$.placement.agent.id') FROM runner_assignments WHERE id = ?1",
        [&operation.assignment_id], |row| row.get(0),
    )?;
    super::issue_chat::dispatch(
        db,
        &operation.operation_id,
        issue_id,
        &agent,
        &work.objective,
        None,
    )?;
    if matching_task_agent(
        runner,
        requested,
        work,
        work.agent.as_ref().map(|agent| agent.id.as_str()),
    )
    .is_none()
    {
        let missing_repo_ids = work
            .repo_ids
            .iter()
            .filter(|repo| !runner.repos.contains(repo))
            .cloned()
            .collect();
        enqueue(
            db,
            source_task,
            IssueInput {
                input_id: operation.operation_id.clone(),
                child_issue_id: issue_id.to_owned(),
                child_task_id: task_id.to_owned(),
                cause: IssueInputCause::NoCandidate {
                    missing_repo_ids,
                    category: requested.to_owned(),
                    required_capabilities: work.required_capabilities.clone(),
                    agent_id: work.agent.as_ref().map(|agent| agent.id.clone()),
                },
                summary: format!(
                    "Runner {runner_id} currently has no matching configuration. The original dispatch remains pending."
                ),
            },
        )?;
    }
    Ok(())
}

pub(super) fn report_task_stop(db: &Connection, observation: &RunObservation) -> Result<()> {
    let RunPhase::Stopped {
        outcome,
        error,
        waiting_for_children,
        waiting_for_attention,
        waiting_for_external,
        queued_input,
    } = &observation.phase
    else {
        return Ok(());
    };
    if *waiting_for_attention || *waiting_for_children || *waiting_for_external || *queued_input {
        return Ok(());
    }
    let child = db.query_row(
        "SELECT i.id, i.parent_issue_id, i.state FROM tasks t JOIN issues i ON t.issue_id = i.id
         WHERE t.id = ?1",
        [&observation.task_id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
            ))
        },
    )?;
    let (child_issue_id, parent_issue, state) = child;
    super::tend_review::finish(
        db,
        &observation.task_id,
        &observation.event_id,
        Some(&observation.run_id),
        None,
        error
            .as_deref()
            .unwrap_or("Review Task stopped without a structured conclusion."),
    )?;
    if *outcome != RunOutcome::Cancelled
        && db.query_row(
            "SELECT EXISTS(SELECT 1 FROM issue_inputs WHERE task_id = ?1 AND run_id IS NULL
                AND json_extract(payload, '$.kind') != 'review_finished'
                AND NOT EXISTS (SELECT 1 FROM proposal_decisions d WHERE d.input_id = issue_inputs.id)
                AND (wake_at IS NULL OR wake_at <= CAST(unixepoch('subsec') * 1000 AS INTEGER)))",
            [&observation.task_id],
            |row| row.get::<_, bool>(0),
        )?
    {
        target_task(db, &child_issue_id)?;
    }
    let Some(parent_issue) = parent_issue else {
        return Ok(());
    };
    // A successful close already carries the owner's outcome. A stopped
    // Task with the Issue still open, or an actual failure, carries its own fact.
    if *outcome == RunOutcome::Completed && state == "closed" {
        return Ok(());
    }
    let parent_task = if *outcome == RunOutcome::Completed {
        latest_task(db, &parent_issue)?
    } else {
        target_task(db, &parent_issue)?
    };
    let Some(parent_task) = parent_task else {
        return Ok(());
    };
    enqueue(
        db,
        &parent_task,
        IssueInput {
            input_id: observation.event_id.clone(),
            child_issue_id,
            child_task_id: observation.task_id.clone(),
            cause: IssueInputCause::TaskStopped {
                run_id: observation.run_id.clone(),
                outcome: *outcome,
            },
            summary: error
                .clone()
                .unwrap_or_else(|| "The child Task completed. Its Issue is still open.".to_owned()),
        },
    )
}

// Ordinary facts attach to the existing task even if it is terminal. Only a
// due window (or an independent immediate input) can create a follow-up.
pub(super) fn latest_task(db: &Connection, issue: &str) -> Result<Option<String>> {
    Ok(db
        .query_row(
            "SELECT t.id FROM tasks t JOIN issues i ON i.id = t.issue_id
         WHERE t.issue_id = ?1 AND i.state != 'cancelled' ORDER BY t.rowid DESC LIMIT 1",
            [issue],
            |row| row.get(0),
        )
        .optional()?)
}

pub(super) fn target_task(db: &Connection, issue: &str) -> Result<Option<String>> {
    let original = db
        .query_row(
            "SELECT t.id, t.status, a.contract FROM tasks t
             JOIN issues i ON i.id = t.issue_id AND i.state != 'cancelled'
             LEFT JOIN runner_assignments a ON a.id = t.current_assignment
             WHERE t.issue_id = ?1
             ORDER BY t.rowid DESC LIMIT 1",
            [issue],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((previous, status, contract)) = original else {
        return Ok(None);
    };
    if !matches!(status.as_str(), "completed" | "failed" | "cancelled") {
        return Ok(Some(previous));
    }
    let Some(contract) = contract else {
        return Ok(None);
    };
    let prior: TaskAssignment = serde_json::from_str(&contract)?;
    let task = protocol::new_id();
    db.execute(
        "INSERT INTO tasks (id, issue_id, category, status, repo_ids, required_capabilities,
                            pinned_runner_id, pinned_agent_id, previous_task_id, workspace_key, base_refs, requested_agent)
         SELECT ?1, issue_id, category, 'awaiting_assignment', repo_ids,
                CASE WHEN category = 'issue-router' THEN '[]' ELSE required_capabilities END, ?3, ?4, id, workspace_key, base_refs, requested_agent
         FROM tasks WHERE id = ?2",
        params![
            task,
            previous,
            prior.placement.runner_id,
            prior.placement.agent.id
        ],
    )?;
    db.execute(
        "UPDATE issue_inputs SET task_id = ?2 WHERE task_id = ?1 AND run_id IS NULL
           AND json_extract(payload, '$.kind') != 'review_finished'
           AND NOT EXISTS (SELECT 1 FROM proposal_decisions d WHERE d.input_id = issue_inputs.id)",
        params![previous, task],
    )?;
    Ok(Some(task))
}

pub(super) fn enqueue(db: &Connection, task: &str, input: IssueInput) -> Result<()> {
    let now = db.query_row(&format!("SELECT {NOW_MS}"), [], |row| row.get(0))?;
    enqueue_at(db, task, input, now)
}

fn enqueue_at(db: &Connection, task: &str, input: IssueInput, now: i64) -> Result<()> {
    let ordinary = match &input.cause {
        IssueInputCause::IssueClosed
        | IssueInputCause::ArtifactAvailable { .. }
        | IssueInputCause::TaskStopped {
            outcome: RunOutcome::Completed,
            ..
        } => true,
        IssueInputCause::IssueMessage { message } => {
            message.kind == protocol::IssueMessageKind::Agent
        }
        _ => false,
    };
    db.execute(
        "INSERT INTO issue_inputs (id, task_id, payload, wake_at)
         VALUES (?1, ?2, ?3, CASE WHEN ?4 THEN coalesce((
             SELECT min(wake_at) FROM issue_inputs
             WHERE task_id = ?2 AND run_id IS NULL AND wake_at > ?5
         ), ?6) END)
         ON CONFLICT(id) DO NOTHING",
        params![
            input.input_id,
            task,
            serde_json::to_string(&input)?,
            ordinary,
            now,
            now + CHILD_WINDOW_MS
        ],
    )?;
    Ok(())
}

impl super::Store {
    pub fn release_child_windows(&mut self) -> Result<()> {
        let exists = self
            .db
            .query_row(&format!("SELECT EXISTS({DUE_TERMINAL})"), [], |row| {
                row.get::<_, bool>(0)
            })?;
        if exists {
            let tx = self
                .db
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            release_terminal_windows(&tx)?;
            tx.commit()?;
        }
        Ok(())
    }
}

const DUE_TERMINAL: &str = "SELECT DISTINCT t.issue_id FROM issue_inputs p
     JOIN tasks t ON t.id = p.task_id JOIN issues i ON i.id = t.issue_id
     WHERE p.run_id IS NULL AND p.wake_at <= CAST(unixepoch('subsec') * 1000 AS INTEGER)
       AND t.status IN ('completed', 'failed', 'cancelled') AND t.current_assignment IS NOT NULL
       AND i.state != 'cancelled' LIMIT 32";

fn release_terminal_windows(db: &Connection) -> Result<()> {
    let mut statement = db.prepare(DUE_TERMINAL)?;
    let issues = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for issue in issues {
        if let Some(target) = target_task(db, &issue)? {
            db.execute(
                "UPDATE issue_inputs SET task_id = ?2
                 WHERE run_id IS NULL AND wake_at IS NOT NULL AND task_id IN (
                    SELECT id FROM tasks WHERE issue_id = ?1 AND id != ?2
                       AND status IN ('completed','failed','cancelled')
                 )",
                params![issue, target],
            )?;
        }
    }
    Ok(())
}

pub fn pending_inputs(
    db: &Connection,
    runner: &str,
    task: Option<&str>,
    protocol: u32,
) -> Result<Vec<IssueDelivery>> {
    let now = db.query_row(&format!("SELECT {NOW_MS}"), [], |row| row.get(0))?;
    pending_inputs_at(db, runner, task, protocol, now)
}

fn pending_inputs_at(
    db: &Connection,
    runner: &str,
    task: Option<&str>,
    protocol: u32,
    now: i64,
) -> Result<Vec<IssueDelivery>> {
    let mut statement = db.prepare(
        "WITH ready AS MATERIALIZED (
           SELECT i.rowid AS seq, i.task_id, i.wake_at, i.payload, a.id AS assignment_id
           FROM issue_inputs i JOIN tasks t ON t.id = i.task_id
           JOIN runner_assignments a ON a.id = t.current_assignment
           WHERE i.run_id IS NULL AND a.runner_id = ?1 AND (?3 IS NULL OR t.id = ?3)
             AND t.status IN ('awaiting_receipt', 'accepted', 'waiting', 'running')
             AND (?4 >= 2 OR json_extract(i.payload, '$.kind') != 'review_finished')
             AND (i.wake_at IS NULL OR i.wake_at <= ?5)
         ), page AS MATERIALIZED (
           SELECT seq, task_id, wake_at FROM ready ORDER BY wake_at IS NOT NULL, seq LIMIT ?2
         )
         SELECT payload, assignment_id FROM ready
         WHERE seq IN (SELECT seq FROM page)
            OR (?4 >= 2 AND (task_id, wake_at) IN (SELECT task_id, wake_at FROM page WHERE wake_at IS NOT NULL))
         ORDER BY wake_at IS NOT NULL, seq",
    )?;
    statement
        .query_map(
            params![runner, RUNNER_SYNC_BATCH, task, protocol, now],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?
        .map(|value| {
            let (payload, assignment_id) = value?;
            Ok(IssueDelivery {
                assignment_id,
                input: serde_json::from_str(&payload)?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{AttentionRequest, IssueMessage, IssueMessageKind};

    fn input(id: &str, cause: IssueInputCause) -> IssueInput {
        IssueInput {
            input_id: id.to_owned(),
            child_issue_id: "child".to_owned(),
            child_task_id: "child-task".to_owned(),
            summary: id.to_owned(),
            cause,
        }
    }

    fn message(kind: IssueMessageKind) -> IssueInputCause {
        IssueInputCause::IssueMessage {
            message: IssueMessage {
                id: "original".into(),
                issue_id: "child".into(),
                short_number: None,
                issue_title: "Child".into(),
                kind,
                author: "sender".into(),
                text: "message".into(),
                reply_to: None,
                run_id: None,
                source_message_id: None,
                forwarded_from: None,
                created_at: None,
            },
        }
    }

    #[test]
    fn ordinary_window_is_fixed_atomic_and_independent_of_immediate_inputs() -> Result<()> {
        let db = Connection::open_in_memory()?;
        db.execute_batch(
            "CREATE TABLE tasks(id TEXT PRIMARY KEY, current_assignment TEXT, status TEXT);
             CREATE TABLE runner_assignments(id TEXT PRIMARY KEY, runner_id TEXT);
             CREATE TABLE issue_inputs(id TEXT PRIMARY KEY, task_id TEXT, payload TEXT, run_id TEXT);
             INSERT INTO runner_assignments VALUES ('assignment', 'runner'), ('other-assignment', 'runner');
             INSERT INTO tasks VALUES ('parent', 'assignment', 'waiting'),
                                      ('other-parent', 'other-assignment', 'waiting');",
        )?;
        migrate_continuity(&db)?;
        let start = 100_000;
        let due = start + CHILD_WINDOW_MS;
        enqueue_at(
            &db,
            "parent",
            input("first", message(IssueMessageKind::Agent)),
            start,
        )?;
        let immediate = [
            input("user", message(IssueMessageKind::User)),
            input("relay", message(IssueMessageKind::Relay)),
            input(
                "question",
                IssueInputCause::AttentionRequested {
                    attention_id: "question".into(),
                    request: AttentionRequest {
                        question: "Blocked on a decision".into(),
                        context: String::new(),
                        options: vec![],
                        priority: None,
                        due_at: None,
                    },
                },
            ),
            input(
                "answer",
                IssueInputCause::AttentionAnswered {
                    attention_id: "question".into(),
                    answer: "Yes".into(),
                    user_message_id: None,
                    user_text: None,
                },
            ),
            input(
                "stop",
                IssueInputCause::TaskCancelled {
                    control_id: "control".into(),
                },
            ),
            input(
                "failure",
                IssueInputCause::TaskStopped {
                    run_id: "run".into(),
                    outcome: RunOutcome::Failed,
                },
            ),
            input(
                "no-candidate",
                IssueInputCause::NoCandidate {
                    missing_repo_ids: vec![],
                    category: "review".into(),
                    required_capabilities: vec![],
                    agent_id: None,
                },
            ),
        ];
        for fact in immediate {
            enqueue_at(&db, "parent", fact, start + 1)?;
        }
        for i in 0..(RUNNER_SYNC_BATCH + 1) {
            enqueue_at(
                &db,
                "parent",
                input(&format!("close-{i}"), IssueInputCause::IssueClosed),
                due - 1,
            )?;
        }
        enqueue_at(
            &db,
            "other-parent",
            input("other", IssueInputCause::IssueClosed),
            due - 1,
        )?;
        enqueue_at(
            &db,
            "parent",
            input("next", IssueInputCause::IssueClosed),
            due,
        )?;
        enqueue_at(
            &db,
            "parent",
            input("first", message(IssueMessageKind::Agent)),
            due + 1,
        )?;

        let keys = |now| -> Result<Vec<String>> {
            Ok(pending_inputs_at(&db, "runner", None, 2, now)?
                .into_iter()
                .map(|delivery| delivery.input.input_id)
                .collect())
        };
        let fast = keys(due - 1)?;
        assert_eq!(
            fast,
            [
                "user",
                "relay",
                "question",
                "answer",
                "stop",
                "failure",
                "no-candidate"
            ]
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM issue_inputs", [], |row| row
                .get::<_, usize>(0))?,
            fast.len() + RUNNER_SYNC_BATCH + 4
        );
        let batch = keys(due)?;
        assert_eq!(batch.len(), fast.len() + RUNNER_SYNC_BATCH + 2);
        assert_eq!(batch[fast.len()], "first");
        assert!(!batch.iter().any(|id| id == "next" || id == "other"));
        assert_eq!(keys(due + 1)?, batch);
        // Simulate one Run receipt for the matured parent window.
        db.execute("UPDATE issue_inputs SET run_id = 'run' WHERE task_id = 'parent' AND (wake_at IS NULL OR wake_at = ?1)", [due])?;
        assert!(keys(due + 1)?.is_empty());
        assert_eq!(keys(due + CHILD_WINDOW_MS - 1)?, ["other"]);
        assert_eq!(keys(due + CHILD_WINDOW_MS)?, ["other", "next"]);
        Ok(())
    }
}
