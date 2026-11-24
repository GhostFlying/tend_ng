use anyhow::Result;
use protocol::{
    IssueInput, IssueInputCause, RUNNER_SYNC_BATCH, RunObservation, RunOutcome, RunPhase,
    RunnerProfile, WORK_CATEGORIES, WorkAction, WorkOperation, matching_task_agent,
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
                "INSERT INTO tasks (id, issue_id, category, status, repo_ids, required_capabilities, pinned_runner_id)
                 VALUES (?1, ?2, ?3, 'awaiting_assignment', ?4, ?5, ?6)",
                params![task_id, issue_id, requested, serde_json::to_string(&work.repo_ids)?,
                        serde_json::to_string(&work.required_capabilities)?, runner_id],
            )?;
            db.execute(
                "INSERT INTO child_dispatches (task_id, operation_id) VALUES (?1, ?2)",
                params![task_id, operation.operation_id],
            )?;
            if matching_task_agent(runner, requested, work).is_none() {
                let missing_repo_ids = work
                    .repo_ids
                    .iter()
                    .filter(|repo| !runner.repos.contains(repo))
                    .cloned()
                    .collect();
                enqueue(
                    db,
                    IssueInput {
                        input_id: operation.operation_id.clone(),
                        assignment_id: operation.assignment_id.clone(),
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
                && let Some(assignment_id) = parent_assignment(db, &parent_issue)?
            {
                enqueue(
                    db,
                    IssueInput {
                        input_id: operation.operation_id.clone(),
                        assignment_id,
                        child_issue_id: source_issue,
                        child_task_id: source_task,
                        cause: IssueInputCause::IssueClosed,
                        summary: summary.clone(),
                    },
                )?;
            }
        }
    }
    Ok(true)
}

pub(super) fn report_task_stop(db: &Connection, observation: &RunObservation) -> Result<()> {
    let RunPhase::Stopped { outcome, error, .. } = &observation.phase else {
        return Ok(());
    };
    let child = db.query_row(
        "SELECT i.id, i.parent_issue_id, i.state FROM tasks t JOIN issues i ON t.issue_id = i.id
         WHERE t.id = ?1 AND i.parent_issue_id IS NOT NULL",
        [&observation.task_id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)),
    ).optional()?;
    let Some((child_issue_id, parent_issue, state)) = child else {
        return Ok(());
    };
    // A successful close already carries the owner's outcome. A stopped
    // Task with the Issue still open, or an actual failure, carries its own fact.
    if *outcome == RunOutcome::Completed && state == "closed" {
        return Ok(());
    }
    let Some(assignment_id) = parent_assignment(db, &parent_issue)? else {
        return Ok(());
    };
    enqueue(
        db,
        IssueInput {
            input_id: observation.event_id.clone(),
            assignment_id,
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

fn parent_assignment(db: &Connection, issue: &str) -> Result<Option<String>> {
    Ok(db
        .query_row(
            "SELECT current_assignment FROM tasks WHERE issue_id = ?1 AND category = 'issue-router'
         ORDER BY rowid DESC LIMIT 1",
            [issue],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten())
}

fn enqueue(db: &Connection, input: IssueInput) -> Result<()> {
    db.execute(
        "INSERT INTO issue_inputs (id, assignment_id, payload) VALUES (?1, ?2, ?3)",
        params![
            input.input_id,
            input.assignment_id,
            serde_json::to_string(&input)?
        ],
    )?;
    Ok(())
}

pub fn pending_inputs(db: &Connection, runner: &str) -> Result<Vec<IssueInput>> {
    let mut statement = db.prepare(
        "SELECT i.payload FROM issue_inputs i
         JOIN runner_assignments a ON a.id = i.assignment_id
         JOIN tasks t ON t.id = a.task_id
         WHERE i.run_id IS NULL AND a.runner_id = ?1 AND a.state = 'accepted'
           AND t.status IN ('accepted', 'waiting', 'running')
         ORDER BY i.rowid LIMIT ?2",
    )?;
    statement
        .query_map(params![runner, RUNNER_SYNC_BATCH], |row| {
            row.get::<_, String>(0)
        })?
        .map(|value| Ok(serde_json::from_str(&value?)?))
        .collect()
}
