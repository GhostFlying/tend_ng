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

pub fn apply(
    db: &Connection,
    runner: &RunnerProfile,
    operation: &WorkOperation,
    comment_baselines: &super::codebase_comments::Prelinks,
) -> Result<bool> {
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
        let matches = origin == runner_id && serialized == content;
        if matches
            && let WorkAction::LinkPull { url } = &operation.action
            && let Some(url) = protocol::PullRef::parse(url).map(|pull| pull.url())
            && let Some(prelink) = comment_baselines.get(&url)
        {
            super::codebase_comments::record_existing_prelink(db, &url, prelink)?;
        }
        return Ok(matches);
    }

    let source = db
        .query_row(
            "SELECT i.id, i.source_seq, i.parent_issue_id, t.category, t.id, a.contract
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
                    row.get::<_, String>(5)?,
                ))
            },
        )
        .optional()?;
    let Some((source_issue, source_message, parent_issue, category, source_task, contract)) =
        source
    else {
        return Ok(false);
    };
    let owns_root = protocol::can_own_root(&category, parent_issue.as_deref());
    if !owns_root
        && matches!(
            operation.action,
            WorkAction::ReportIssue { .. }
                | WorkAction::ForwardIssueMessage { .. }
                | WorkAction::EscalateAttention { .. }
                | WorkAction::AnswerChildAttention { .. }
                | WorkAction::AskAttention {
                    proposal: Some(_),
                    ..
                }
        )
    {
        return Ok(false);
    }
    if let WorkAction::AskStageAttention { report_id, .. } = &operation.action
        && (!owns_root
            || !db.query_row(
                "SELECT EXISTS(SELECT 1 FROM issue_reports report
                 JOIN runner_operations op ON op.id = report.id
                 JOIN task_runs run ON run.id = json_extract(op.operation, '$.run_id')
                 WHERE report.id = ?1 AND report.issue_id = ?2 AND run.task_id = ?3)",
                params![report_id, source_issue, source_task],
                |row| row.get::<_, bool>(0),
            )?)
    {
        return Ok(false);
    }
    if let WorkAction::Delegate {
        category: requested,
        work,
        ..
    } = &operation.action
        && (!owns_root
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
            rereview,
        } if category == "review"
            || !protocol::valid_id(issue_id)
            || !protocol::valid_id(task_id)
            || !protocol::valid_tend_review(candidate, scope, work)
            || *rereview
                && (work.agent.is_some()
                    || !super::tend_review::can_request_again(
                        db,
                        task_id,
                        issue_id,
                        &source_task,
                        candidate,
                        &operation.run_id,
                    )?)
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
        WorkAction::DeliverReview { result, request_id } => {
            let bound = super::tend_review::request_for_run(db, &operation.run_id)?;
            if category != "review"
                || !result.valid()
                || bound.is_none()
                || request_id
                    .as_ref()
                    .is_some_and(|id| Some(id) != bound.as_ref())
            {
                return Ok(false);
            }
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
            if !super::backlog::old_task(db, &source_task)? {
                flush_pending(db, &source_task)?;
            }
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
                isolated_workspace.then_some(issue_id.as_str()),
                work,
            )?;
        }
        WorkAction::RequestReview {
            issue_id,
            task_id,
            candidate,
            scope,
            work,
            rereview,
        } => {
            if !super::backlog::old_task(db, &source_task)? {
                flush_pending(db, &source_task)?;
            }
            let author: TaskAssignment = serde_json::from_str(&contract)?;
            let workspace = author.workspace_path();
            if !rereview {
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
                    workspace.strip_prefix("workspaces/"),
                    work,
                )?;
            }
            super::tend_review::bind(
                db,
                &operation.operation_id,
                &source_issue,
                &source_task,
                issue_id,
                task_id,
                candidate,
                scope,
                &workspace,
            )?;
            if !super::tend_review::author_can_request(db, operation, task_id, &source_task)? {
                if !rereview {
                    db.execute(
                        "UPDATE tasks SET status = 'cancelled' WHERE id = ?1",
                        [task_id],
                    )?;
                }
                super::tend_review::not_run(
                    db,
                    task_id,
                    operation,
                    "Review was not run because its Issue tree or author Task was cancelled, stopping, or no longer assigned to the request's Runner.",
                )?;
            } else if *rereview {
                super::tend_review::request_again(db, runner, operation, issue_id, task_id, work)?;
            } else {
                super::runner::assign_task(db, task_id)?;
            }
        }
        WorkAction::DeliverReview { result, .. } => {
            let request = super::tend_review::request_for_run(db, &operation.run_id)?;
            super::tend_review::finish(
                db,
                &source_task,
                request.as_deref(),
                &operation.operation_id,
                Some(&operation.run_id),
                Some(result),
                &result.conclusion,
            )?;
        }
        WorkAction::CloseIssue { summary } => {
            if super::backlog::old_task(db, &source_task)? {
                return Ok(true);
            }
            flush_pending(db, &source_task)?;
            let changed = db.execute(
                "UPDATE issues SET state = 'closed' WHERE id = ?1 AND state = 'open'",
                [&source_issue],
            )?;
            if changed != 0
                && let Some(parent_issue) = parent_issue
                && !review_reports_directly(db, &source_task, &parent_issue)?
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
            ..
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
        }
        | WorkAction::AskStageAttention { request, .. } => {
            super::attention::register(
                db,
                &operation.operation_id,
                &source_issue,
                &source_task,
                source_message,
                parent_issue.as_deref(),
                request,
            )?;
            if let WorkAction::AskStageAttention { report_id, .. } = &operation.action {
                db.execute(
                    "INSERT INTO attention_report_dependencies (attention_id, report_id) VALUES (?1, ?2)",
                    params![operation.operation_id, report_id],
                )?;
                // The report and Attention can land in two sync batches.
                // A report already settled without a reply now has a dependent.
                if let Some(input) = db.query_row(
                    "SELECT seq FROM router_inputs WHERE report_id = ?1 AND outcome IS NOT NULL",
                    [report_id], |row| row.get::<_, i64>(0),
                ).optional()? {
                    super::reports::publish_stage_fallback(db, input)?;
                }
            }
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
            if super::backlog::old_task(db, &source_task)? {
                return Ok(true);
            }
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
            if *mode == protocol::WatchMode::Wait && super::backlog::old_task(db, &source_task)? {
                super::external::clear_waits(db, &source_task)?;
            }
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
            let Some(pull) = protocol::PullRef::parse(url) else {
                return Ok(false);
            };
            super::pull::bind(db, &source_issue, &pull, comment_baselines.get(&pull.url()))?;
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
    workspace_key: Option<&str>,
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
                serde_json::to_string(&work.required_capabilities)?, runner_id, workspace_key,
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
        Some(&agent),
    )?;
    if super::backlog::parked(db, parent_issue)? || super::backlog::old_task(db, source_task)? {
        super::backlog::park_late_child(db, issue_id, task_id)?;
        return Ok(());
    }
    if requested == "review" && !runner.shared_reviews
        || matching_task_agent(
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
                summary: if requested == "review" && !runner.shared_reviews {
                    format!(
                        "Runner {runner_id} does not yet support shared review. The original dispatch remains pending."
                    )
                } else {
                    format!(
                        "Runner {runner_id} currently has no matching configuration. The original dispatch remains pending."
                    )
                },
            },
        )?;
    }
    Ok(())
}

// A bound review for a direct Owner already delivers a result (including
// failure without a verdict) to that original parent Task. Its ordinary
// close/stop fact remains visible on the review Issue without a second wake.
pub(super) fn review_reports_directly(
    db: &Connection,
    review_task: &str,
    parent_issue: &str,
) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM tend_reviews v JOIN tasks author ON author.id = v.author_task_id
         WHERE v.review_task_id = ?1 AND author.issue_id = ?2 AND author.category != 'issue-router')",
        params![review_task, parent_issue],
        |row| row.get(0),
    )?)
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
    if let Some(request) = super::tend_review::request_for_run(db, &observation.run_id)? {
        super::tend_review::finish(
            db,
            &observation.task_id,
            Some(&request),
            &observation.event_id,
            Some(&observation.run_id),
            None,
            error
                .as_deref()
                .unwrap_or("Review Task stopped without a structured conclusion."),
        )?;
    }
    if *outcome != RunOutcome::Cancelled
        && db.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM issue_inputs WHERE task_id = ?1 AND run_id IS NULL
                AND json_extract(payload, '$.kind') != 'review_finished'
                AND NOT EXISTS (SELECT 1 FROM proposal_decisions d WHERE d.input_id = issue_inputs.id)
                AND NOT EXISTS (SELECT 1 FROM issue_attention_replies r WHERE r.input_id = issue_inputs.id)
                AND ({})
                AND (wake_at IS NULL OR wake_at <= CAST(unixepoch('subsec') * 1000 AS INTEGER)))", super::external::CAN_FOLLOW_TASK),
            [&observation.task_id],
            |row| row.get::<_, bool>(0),
        )?
    {
        target_due_task(db, &child_issue_id)?;
    }
    let Some(parent_issue) = parent_issue else {
        return Ok(());
    };
    // A successful close already carries the owner's outcome. A stopped
    // Task with the Issue still open, or an actual failure, carries its own fact.
    if *outcome == RunOutcome::Completed && state == "closed"
        || review_reports_directly(db, &observation.task_id, &parent_issue)?
    {
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
    find_target_task(db, issue, true)
}

pub(super) fn target_due_task(db: &Connection, issue: &str) -> Result<Option<String>> {
    find_target_task(db, issue, false)
}

fn find_target_task(db: &Connection, issue: &str, barrier: bool) -> Result<Option<String>> {
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
    if !matches!(status.as_str(), "completed" | "failed" | "cancelled")
        || super::backlog::parked(db, issue)?
        || super::control::stop_on_current(db, &previous, "pending")?
    {
        return Ok(Some(previous));
    }
    if super::tend_review::binding(db, &previous)?.is_some() {
        // Keep discussion on the original Task, but only an explicit
        // candidate can bring a completed bound review off the queue boundary.
        return Ok(Some(previous));
    }
    let Some(contract) = contract else {
        return Ok(None);
    };
    if barrier {
        // The immediate input will group the prefix after it moves to the
        // new recipient, including the input that caused the continuation.
        release_pending(db, &previous)?;
    }
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
        &format!("UPDATE issue_inputs SET task_id = ?2 WHERE task_id = ?1 AND run_id IS NULL
           AND json_extract(payload, '$.kind') != 'review_finished'
           AND NOT EXISTS (SELECT 1 FROM proposal_decisions d WHERE d.input_id = issue_inputs.id)
           AND NOT EXISTS (SELECT 1 FROM issue_attention_replies r WHERE r.input_id = issue_inputs.id)
           AND ({})", super::external::CAN_FOLLOW_TASK),
        params![previous, task],
    )?;
    Ok(Some(task))
}

pub(super) fn move_pending(db: &Connection, previous: &str, task: &str) -> Result<()> {
    db.execute(
        &format!("UPDATE issue_inputs SET task_id = ?2 WHERE task_id = ?1 AND run_id IS NULL
           AND json_extract(payload, '$.kind') NOT IN ('review_requested','review_finished','attention_answered')
           AND ({})", super::external::CAN_FOLLOW_TASK),
        params![previous, task],
    )?;
    Ok(())
}

// Releasing a window is a durable readiness change, not a scheduling call.
pub(super) fn flush_pending(db: &Connection, task: &str) -> Result<Option<String>> {
    release_pending(db, task)?;
    let group = protocol::new_id();
    let changed = db.execute(
        "INSERT OR IGNORE INTO issue_input_flushes(input_id, flush_id)
         SELECT id, ?2 FROM issue_inputs WHERE task_id = ?1 AND run_id IS NULL",
        params![task, group],
    )?;
    Ok((changed > 0).then_some(group))
}

fn release_pending(db: &Connection, task: &str) -> Result<()> {
    super::codebase_comments::flush_for_task(db, task)?;
    super::external::release_task_deliveries(db, task, true, None)?;
    flush_child_window(db, task)
}

fn flush_child_window(db: &Connection, task: &str) -> Result<()> {
    db.execute("UPDATE issue_inputs SET wake_at = NULL WHERE task_id = ?1 AND run_id IS NULL AND wake_at IS NOT NULL", [task])?;
    Ok(())
}

pub(super) fn record_intake(db: &Connection, input: &str) -> Result<()> {
    db.execute(
        "INSERT OR IGNORE INTO issue_input_order(input_id) VALUES (?1)",
        [input],
    )?;
    Ok(())
}

pub(super) fn enqueue_released_external(
    db: &Connection,
    task: &str,
    input: IssueInput,
) -> Result<()> {
    db.execute(
        "INSERT OR IGNORE INTO issue_inputs (id, task_id, payload) VALUES (?1, ?2, ?3)",
        params![input.input_id, task, serde_json::to_string(&input)?],
    )?;
    Ok(())
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
    let group = if !ordinary {
        let duplicate = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM issue_inputs WHERE id = ?1)",
            [&input.input_id],
            |row| row.get::<_, bool>(0),
        )?;
        if duplicate {
            return Ok(());
        }
        flush_pending(db, task)?
    } else {
        None
    };
    if db.execute(
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
    )? > 0
    {
        record_intake(db, &input.input_id)?;
        if let Some(group) = group {
            db.execute(
                "INSERT INTO issue_input_flushes VALUES (?1, ?2)",
                params![input.input_id, group],
            )?;
        }
    }
    Ok(())
}

impl super::Store {
    pub fn release_child_windows(&mut self) -> Result<()> {
        let due = due_terminal();
        let exists = self
            .db
            .query_row(&format!("SELECT EXISTS({due})"), [], |row| {
                row.get::<_, bool>(0)
            })?;
        if exists {
            let tx = self
                .db
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            release_terminal_windows(&tx)?;
            super::board::commit(tx)?;
        }
        Ok(())
    }
}

fn due_terminal() -> String {
    format!("SELECT DISTINCT t.issue_id FROM issue_inputs
     JOIN tasks t ON t.id = issue_inputs.task_id JOIN issues i ON i.id = t.issue_id
     WHERE issue_inputs.run_id IS NULL AND issue_inputs.wake_at <= CAST(unixepoch('subsec') * 1000 AS INTEGER)
       AND t.status IN ('completed', 'failed', 'cancelled') AND t.current_assignment IS NOT NULL
       AND (t.status != 'cancelled' OR issue_inputs.payload->>'kind' NOT IN ('codebase_review_comment','external_fact'))
       AND ({})
       AND NOT EXISTS (SELECT 1 FROM tend_reviews v WHERE v.review_task_id = t.id)
       AND i.state != 'cancelled'
       AND NOT EXISTS(SELECT 1 FROM issue_parks p WHERE p.issue_id = t.issue_id)
       AND NOT EXISTS(SELECT 1 FROM task_controls c WHERE c.task_id = t.id AND c.state = 'pending'
         AND json_extract(c.action, '$.kind') = 'stop') LIMIT 32", super::external::CAN_FOLLOW_TASK)
}

fn release_terminal_windows(db: &Connection) -> Result<()> {
    let mut statement = db.prepare(&due_terminal())?;
    let issues = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for issue in issues {
        if let Some(target) = target_due_task(db, &issue)? {
            db.execute(
                &format!(
                    "UPDATE issue_inputs SET task_id = ?2
                 WHERE run_id IS NULL AND wake_at IS NOT NULL AND task_id IN (
                    SELECT id FROM tasks WHERE issue_id = ?1 AND id != ?2
                       AND status IN ('completed','failed','cancelled')
                 ) AND ({})",
                    super::external::CAN_FOLLOW_TASK
                ),
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
    shared_reviews: bool,
    codebase_review_comments: bool,
) -> Result<Vec<IssueDelivery>> {
    let now = input_cutoff(db)?;
    pending_inputs_at(
        db,
        runner,
        task,
        protocol,
        shared_reviews,
        codebase_review_comments,
        now,
    )
}

pub(super) fn input_cutoff(db: &Connection) -> Result<i64> {
    Ok(db.query_row(&format!("SELECT {NOW_MS}"), [], |row| row.get(0))?)
}

pub(super) fn needs_serialized_wake(
    db: &Connection,
    runner: &str,
    task: Option<&str>,
    cutoff: i64,
) -> Result<bool> {
    // A wake window on a Codebase-linked Issue can get a companion in an
    // observation transaction that started just before the deadline. Unlike
    // the WAL fast path, the serialized path cannot overtake that writer.
    Ok(db.query_row(
        "SELECT EXISTS (
             SELECT 1 FROM runner_assignments a JOIN tasks t ON t.current_assignment=a.id
             JOIN issue_inputs p ON p.task_id=t.id
             WHERE a.runner_id=?1 AND (?2 IS NULL OR t.id=?2)
               AND a.state IN ('issued','accepted')
               AND t.status IN ('awaiting_receipt','accepted','waiting','running')
               AND p.run_id IS NULL AND p.wake_at <= ?3
               AND p.payload->>'kind' NOT IN ('codebase_review_comment','external_fact')
               AND EXISTS(SELECT 1 FROM codebase_comment_links WHERE issue_id=t.issue_id)
         )",
        params![runner, task, cutoff],
        |row| row.get(0),
    )?)
}

pub(super) fn pending_inputs_at(
    db: &Connection,
    runner: &str,
    task: Option<&str>,
    protocol: u32,
    shared_reviews: bool,
    codebase_review_comments: bool,
    now: i64,
) -> Result<Vec<IssueDelivery>> {
    // An ordinary Codebase cohort can become partly staged when its Issue
    // is cancelled. Withhold those members until the release transaction
    // stages the rest and removes the outstanding batch, even if a successor
    // has already reattached the older inputs.
    let mut statement = db.prepare(&format!(
        "WITH ready AS MATERIALIZED (
           SELECT i.rowid AS seq, coalesce(o.seq, 0) AS received, i.task_id, i.wake_at, f.flush_id, i.payload, a.id AS assignment_id
           FROM issue_inputs i JOIN tasks t ON t.id = i.task_id
           LEFT JOIN issue_input_order o ON o.input_id = i.id
           LEFT JOIN issue_input_flushes f ON f.input_id = i.id
           JOIN runner_assignments a ON a.id = t.current_assignment
           WHERE i.run_id IS NULL AND a.runner_id = ?1 AND (?3 IS NULL OR t.id = ?3)
             AND t.status IN ('awaiting_receipt', 'accepted', 'waiting', 'running')
             AND NOT EXISTS(SELECT 1 FROM issue_parks p WHERE p.issue_id = t.issue_id)
             AND NOT EXISTS(SELECT 1 FROM task_controls c WHERE c.task_id = t.id
                 AND c.assignment_id = a.id AND c.state = 'pending' AND json_extract(c.action, '$.kind') = 'stop')
             AND (?4 >= 2 OR json_extract(i.payload, '$.kind') NOT IN ('review_finished','review_requested'))
             AND (?6 OR json_extract(i.payload, '$.kind') != 'review_requested')
             AND (i.payload->>'kind' != 'codebase_review_comment'
                  OR (?7 AND ?4 >= 2 AND ({eligible})
                      AND NOT EXISTS (
                          SELECT 1 FROM codebase_comment_batches b
                          WHERE b.issue_id=t.issue_id AND b.due_at=i.wake_at
                            AND b.ordinary_task_id IS NULL
                      )))
             AND (i.wake_at IS NULL OR i.wake_at <= ?5)
         ), page AS MATERIALIZED (
           SELECT seq, task_id, wake_at, flush_id FROM ready ORDER BY received, seq LIMIT ?2
         )
         SELECT payload, assignment_id FROM ready
         WHERE seq IN (SELECT seq FROM page)
            OR (?4 >= 2 AND (task_id, wake_at) IN (SELECT task_id, wake_at FROM page WHERE wake_at IS NOT NULL))
            OR (?4 >= 2 AND (task_id, flush_id) IN (SELECT task_id, flush_id FROM page WHERE flush_id IS NOT NULL))
         ORDER BY received, seq",
        eligible = super::codebase_comments::CAN_RECEIVE,
    ))?;
    statement
        .query_map(
            params![
                runner,
                RUNNER_SYNC_BATCH,
                task,
                protocol,
                now,
                shared_reviews,
                codebase_review_comments,
            ],
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
    use crate::Store;
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
                dispatch_provenance: None,
                created_at: None,
            },
        }
    }

    #[test]
    fn codebase_comment_batches_require_an_advertising_runner() -> Result<()> {
        let store = Store::initialize(std::path::Path::new(":memory:"))?;
        let db = store.db;
        db.execute_batch(
            "INSERT INTO chat_messages (id, chat_id, text) VALUES ('source','main','Fixture');
             INSERT INTO issues (id, source_seq, title, objective) VALUES ('issue',1,'Fixture','');
             INSERT INTO runners (id, host_id, profile, last_seen) VALUES ('runner','host','{}',0);
             INSERT INTO tasks (id, issue_id, category, status, repo_ids, required_capabilities)
                 VALUES ('owner','issue','fixture','waiting','[]','[]');
             INSERT INTO runner_assignments (id, task_id, runner_id, contract, state, expires_at)
                 VALUES ('assignment','owner','runner','{}','accepted',0);
             UPDATE tasks SET current_assignment = 'assignment' WHERE id = 'owner';",
        )?;
        let comment = protocol::CodebaseReviewComment {
            id: "id".into(),
            source: protocol::CodebaseCommentSource::ThreadComment,
            repository: "example/guide".into(),
            number: 1,
            url: "https://code.byted.org/example/guide/merge_requests/1".into(),
            created_at: String::new(),
            body: "test".into(),
            thread_id: None,
            username: None,
            display_name: None,
            positions: Vec::new(),
            diff: None,
            outdating_diff: None,
        };
        let due = 1234;
        enqueue_at(
            &db,
            "owner",
            input("user", message(IssueMessageKind::User)),
            1,
        )?;
        for row in 0..(RUNNER_SYNC_BATCH + 2) {
            let id = format!("comment-{row}");
            let payload = serde_json::to_string(&input(
                &id,
                IssueInputCause::CodebaseReviewComment {
                    mode: protocol::WatchMode::Required,
                    comment: comment.clone(),
                },
            ))?;
            db.execute(
                "INSERT INTO issue_inputs (id, task_id, payload, wake_at) VALUES (?1,'owner',?2,?3)",
                params![id, payload, due],
            )?;
            record_intake(&db, &id)?;
        }
        assert_eq!(
            pending_inputs_at(&db, "runner", None, 1, false, true, due)?.len(),
            1
        );
        assert_eq!(
            pending_inputs_at(&db, "runner", None, 2, true, false, due)?.len(),
            1
        );
        assert_eq!(
            pending_inputs_at(&db, "runner", None, 2, true, true, due - 1)?.len(),
            1
        );
        let ready = pending_inputs_at(&db, "runner", None, 2, true, true, due)?;
        assert_eq!(ready.len(), RUNNER_SYNC_BATCH + 3);
        assert_eq!(ready[0].input.input_id, "user");
        Ok(())
    }

    #[test]
    fn ordinary_window_is_fixed_atomic_and_cut_by_immediate_inputs() -> Result<()> {
        let store = Store::initialize(std::path::Path::new(":memory:"))?;
        let db = store.db;
        db.execute_batch(
            "INSERT INTO chat_messages (id, chat_id, text) VALUES ('source', 'main', 'Fixture');
             INSERT INTO issues (id, source_seq, title, objective) VALUES ('root', 1, 'Fixture', '');
             INSERT INTO runners (id, host_id, profile, last_seen) VALUES ('runner', 'host', '{}', 0);
             INSERT INTO tasks (id, issue_id, category, status, repo_ids, required_capabilities)
                VALUES ('parent', 'root', 'fixture', 'waiting', '[]', '[]'),
                       ('other-parent', 'root', 'fixture', 'waiting', '[]', '[]');
             INSERT INTO runner_assignments (id, task_id, runner_id, contract, state, expires_at)
                VALUES ('assignment', 'parent', 'runner', '{}', 'accepted', 0),
                       ('other-assignment', 'other-parent', 'runner', '{}', 'accepted', 0);
             UPDATE tasks SET current_assignment = (SELECT id FROM runner_assignments WHERE task_id = tasks.id);",
        )?;
        let start = 100_000;
        let due = start + CHILD_WINDOW_MS;
        let later_due = due + CHILD_WINDOW_MS - 1;
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
            later_due,
        )?;
        enqueue_at(
            &db,
            "parent",
            input("first", message(IssueMessageKind::Agent)),
            due + 1,
        )?;

        let keys = |now| -> Result<Vec<String>> {
            Ok(pending_inputs_at(&db, "runner", None, 2, true, false, now)?
                .into_iter()
                .map(|delivery| delivery.input.input_id)
                .collect())
        };
        let fast = keys(due - 1)?;
        assert_eq!(
            fast,
            [
                "first",
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
            fast.len() + RUNNER_SYNC_BATCH + 3
        );
        assert_eq!(keys(due)?, fast);
        assert_eq!(keys(due + 1)?, fast);
        // Delete the offered prefix to simulate consumption in the next sync.
        db.execute(
            "DELETE FROM issue_inputs WHERE task_id = 'parent' AND wake_at IS NULL",
            [],
        )?;
        assert!(keys(later_due - 1)?.is_empty());
        let batch = keys(later_due)?;
        assert_eq!(
            batch.iter().filter(|id| id.starts_with("close-")).count(),
            RUNNER_SYNC_BATCH + 1,
            "{batch:?}"
        );
        assert_eq!(batch.first().map(String::as_str), Some("close-0"));
        assert!(!batch.iter().any(|id| id == "next"));
        db.execute(
            "DELETE FROM issue_inputs WHERE task_id = 'parent' AND wake_at = ?1",
            [later_due],
        )?;
        assert_eq!(keys(later_due)?, ["other"]);
        assert_eq!(keys(later_due + CHILD_WINDOW_MS)?, ["other", "next"]);
        Ok(())
    }
}
