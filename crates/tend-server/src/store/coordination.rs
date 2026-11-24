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
        ..
    } = &operation.action
        && (category != "issue-router" || !WORK_CATEGORIES.contains(&requested.as_str()))
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
            db.execute(
                "INSERT INTO issues (id, source_seq, title, objective, parent_issue_id)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    issue_id,
                    source_message,
                    work.title,
                    work.objective,
                    source_issue
                ],
            )?;
            db.execute(
                "INSERT INTO tasks (id, issue_id, category, status, repo_ids, required_capabilities, pinned_runner_id, workspace_key)
                 VALUES (?1, ?2, ?3, 'awaiting_assignment', ?4, ?5, ?6, ?7)",
                params![task_id, issue_id, requested, serde_json::to_string(&work.repo_ids)?,
                        serde_json::to_string(&work.required_capabilities)?, runner_id, isolated_workspace.then_some(issue_id)],
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
            if matching_task_agent(runner, requested, work, None).is_none() {
                let missing_repo_ids = work
                    .repo_ids
                    .iter()
                    .filter(|repo| !runner.repos.contains(repo))
                    .cloned()
                    .collect();
                enqueue(
                    db,
                    &source_task,
                    IssueInput {
                        input_id: operation.operation_id.clone(),
                        child_issue_id: issue_id.clone(),
                        child_task_id: task_id.clone(),
                        cause: IssueInputCause::NoCandidate {
                            missing_repo_ids,
                            category: requested.clone(),
                            required_capabilities: work.required_capabilities.clone(),
                        },
                        summary: format!(
                            "Runner {runner_id} currently has no matching configuration. The original dispatch remains pending."
                        ),
                    },
                )?;
            }
        }
        WorkAction::CloseIssue { summary } => {
            let changed = db.execute(
                "UPDATE issues SET state = 'closed' WHERE id = ?1 AND state = 'open'",
                [&source_issue],
            )?;
            if changed != 0
                && let Some(parent_issue) = parent_issue
                && let Some(parent_task) = target_task(db, &parent_issue)?
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
        WorkAction::AskAttention { request } => {
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
                 SELECT EXISTS(SELECT 1 FROM tasks JOIN tree ON tree.id = tasks.issue_id WHERE tasks.id = ?2)",
                params![source_issue, task_id], |row| row.get::<_, bool>(0),
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
    if *outcome != RunOutcome::Cancelled
        && db.query_row(
            "SELECT EXISTS(SELECT 1 FROM issue_inputs WHERE task_id = ?1 AND run_id IS NULL)",
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
    let Some(parent_task) = target_task(db, &parent_issue)? else {
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
                            pinned_runner_id, pinned_agent_id, previous_task_id, workspace_key)
         SELECT ?1, issue_id, category, 'awaiting_assignment', repo_ids,
                CASE WHEN category = 'issue-router' THEN '[]' ELSE required_capabilities END, ?3, ?4, id, workspace_key
         FROM tasks WHERE id = ?2",
        params![
            task,
            previous,
            prior.placement.runner_id,
            prior.placement.agent.id
        ],
    )?;
    db.execute(
        "UPDATE issue_inputs SET task_id = ?2 WHERE task_id = ?1 AND run_id IS NULL",
        params![previous, task],
    )?;
    Ok(Some(task))
}

pub(super) fn enqueue(db: &Connection, task: &str, input: IssueInput) -> Result<()> {
    db.execute(
        "INSERT INTO issue_inputs (id, task_id, payload) VALUES (?1, ?2, ?3)",
        params![input.input_id, task, serde_json::to_string(&input)?],
    )?;
    Ok(())
}

pub fn pending_inputs(
    db: &Connection,
    runner: &str,
    task: Option<&str>,
) -> Result<Vec<IssueDelivery>> {
    let mut statement = db.prepare(
        "SELECT i.payload, a.id FROM issue_inputs i
         JOIN tasks t ON t.id = i.task_id
         JOIN runner_assignments a ON a.id = t.current_assignment
         WHERE i.run_id IS NULL AND a.runner_id = ?1 AND (?3 IS NULL OR t.id = ?3)
           AND t.status IN ('awaiting_receipt', 'accepted', 'waiting', 'running')
         ORDER BY i.rowid LIMIT ?2",
    )?;
    statement
        .query_map(params![runner, RUNNER_SYNC_BATCH, task], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .map(|value| {
            let (payload, assignment_id) = value?;
            Ok(IssueDelivery {
                assignment_id,
                input: serde_json::from_str(&payload)?,
            })
        })
        .collect()
}
