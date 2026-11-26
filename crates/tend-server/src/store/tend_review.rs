use super::coordination;
use anyhow::Result;
use protocol::{
    DispatchIssue, IssueInput, IssueInputCause, RunnerProfile, TendReview, TendReviewConclusion,
    WorkOperation,
};
use rusqlite::{Connection, OptionalExtension, params};

pub(super) fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE tend_reviews (
            request_id TEXT PRIMARY KEY REFERENCES runner_operations(id),
            author_issue_id TEXT NOT NULL REFERENCES issues(id),
            author_task_id TEXT NOT NULL REFERENCES tasks(id),
            review_issue_id TEXT NOT NULL UNIQUE REFERENCES issues(id),
            review_task_id TEXT NOT NULL UNIQUE REFERENCES tasks(id),
            candidate TEXT NOT NULL,
            scope TEXT NOT NULL,
            result_input_id TEXT UNIQUE REFERENCES issue_inputs(id)
         );
         CREATE INDEX tend_reviews_by_author ON tend_reviews(author_task_id);",
    )?;
    Ok(())
}

pub(super) fn migrate_rounds(db: &Connection) -> Result<()> {
    if db.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('tend_reviews') WHERE name = 'author_workspace')",
        [], |row| row.get::<_, bool>(0),
    )? {
        return Ok(());
    }
    db.execute_batch(
        "ALTER TABLE tend_reviews RENAME TO old_tend_reviews;
         CREATE TABLE tend_reviews (
            request_id TEXT PRIMARY KEY REFERENCES runner_operations(id),
            author_issue_id TEXT NOT NULL REFERENCES issues(id),
            author_task_id TEXT NOT NULL REFERENCES tasks(id),
            review_issue_id TEXT NOT NULL REFERENCES issues(id),
            review_task_id TEXT NOT NULL REFERENCES tasks(id),
            candidate TEXT NOT NULL,
            scope TEXT NOT NULL,
            result_input_id TEXT UNIQUE REFERENCES issue_inputs(id),
            author_workspace TEXT
         );
         INSERT INTO tend_reviews (request_id, author_issue_id, author_task_id, review_issue_id,
                                   review_task_id, candidate, scope, result_input_id)
         SELECT request_id, author_issue_id, author_task_id, review_issue_id,
                review_task_id, candidate, scope, result_input_id FROM old_tend_reviews ORDER BY rowid;
         DROP TABLE old_tend_reviews;
         CREATE INDEX tend_reviews_by_author ON tend_reviews(author_task_id);
         CREATE INDEX tend_reviews_by_reviewer ON tend_reviews(review_task_id);",
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn bind(
    db: &Connection,
    request: &str,
    author_issue: &str,
    author_task: &str,
    review_issue: &str,
    review_task: &str,
    candidate: &str,
    scope: &str,
    author_workspace: &str,
) -> Result<()> {
    db.execute(
        "INSERT INTO tend_reviews (request_id, author_issue_id, author_task_id, review_issue_id,
                                   review_task_id, candidate, scope, author_workspace)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            request,
            author_issue,
            author_task,
            review_issue,
            review_task,
            candidate,
            scope,
            author_workspace
        ],
    )?;
    Ok(())
}

pub(super) fn binding(db: &Connection, review_task: &str) -> Result<Option<TendReview>> {
    round(db, review_task, None)
}

pub(super) fn work_for_round(db: &Connection, review: &TendReview) -> Result<DispatchIssue> {
    let raw: String = db.query_row(
        "SELECT json_extract(operation, '$.action.work') FROM runner_operations WHERE id = ?1",
        [&review.request_id],
        |row| row.get(0),
    )?;
    Ok(serde_json::from_str(&raw)?)
}

pub(super) fn history(db: &Connection, task: &str, reviewer: bool) -> Result<Vec<IssueInput>> {
    // The Server retains every result. Give replacement sessions the most
    // recent result for each related Review Task without replaying it as input.
    let mut statement = db.prepare(if reviewer {
        "SELECT i.payload FROM tend_reviews v JOIN issue_inputs i ON i.id = v.result_input_id
         WHERE v.review_task_id = ?1 ORDER BY v.rowid DESC LIMIT 1"
    } else {
        "SELECT i.payload FROM tend_reviews v JOIN issue_inputs i ON i.id = v.result_input_id
         WHERE v.author_task_id = ?1 AND i.run_id IS NOT NULL
           AND v.rowid = (SELECT max(p.rowid) FROM tend_reviews p JOIN issue_inputs consumed ON consumed.id = p.result_input_id
                           WHERE p.review_task_id = v.review_task_id AND consumed.run_id IS NOT NULL)
         ORDER BY v.rowid DESC LIMIT 32"
    })?;
    statement
        .query_map([task], |row| row.get::<_, String>(0))?
        .map(|payload| Ok(serde_json::from_str(&payload?)?))
        .collect()
}

pub(super) fn author_can_request(
    db: &Connection,
    operation: &WorkOperation,
    task: &str,
    author_task: &str,
) -> Result<bool> {
    Ok(db.query_row(
        "WITH RECURSIVE ancestors(id, parent, state) AS (
           SELECT i.id, i.parent_issue_id, i.state FROM issues i JOIN tasks t ON t.issue_id = i.id
            WHERE t.id = ?1 OR t.id = ?2
           UNION SELECT i.id, i.parent_issue_id, i.state FROM issues i JOIN ancestors a ON a.parent = i.id
         )
         SELECT NOT EXISTS(SELECT 1 FROM ancestors a LEFT JOIN issue_parks p ON p.issue_id = a.id
                            WHERE state = 'cancelled' OR p.issue_id IS NOT NULL)
            AND EXISTS(SELECT 1 FROM tasks t JOIN task_runs r ON r.task_id = t.id
                        WHERE r.id = ?3 AND t.current_assignment = ?4
                          AND t.status NOT IN ('completed','failed','cancelled')
                          AND NOT EXISTS(SELECT 1 FROM parked_tasks p WHERE p.task_id = t.id)
                          AND NOT EXISTS(SELECT 1 FROM task_controls c WHERE c.task_id = t.id
                             AND c.assignment_id = ?4 AND c.state = 'pending'
                             AND json_extract(c.action, '$.kind') = 'stop'))",
        params![task, author_task, operation.run_id, operation.assignment_id], |row| row.get(0),
    )?)
}

fn round(db: &Connection, review_task: &str, request: Option<&str>) -> Result<Option<TendReview>> {
    Ok(db
        .query_row(
            "SELECT request_id, author_issue_id, author_task_id, candidate, scope, author_workspace
         FROM tend_reviews WHERE review_task_id = ?1 AND (?2 IS NULL OR request_id = ?2)
         ORDER BY rowid DESC LIMIT 1",
            params![review_task, request],
            |row| {
                Ok(TendReview {
                    request_id: row.get(0)?,
                    author_issue_id: row.get(1)?,
                    author_task_id: row.get(2)?,
                    candidate: row.get(3)?,
                    scope: row.get(4)?,
                    author_workspace: row.get(5)?,
                })
            },
        )
        .optional()?)
}

/// Prefer the last explicit round assigned to this Run (or a preceding Run
/// of the same assignment). The immutable original contract covers round one.
pub(super) fn request_for_run(db: &Connection, run: &str) -> Result<Option<String>> {
    Ok(db.query_row(
        "SELECT coalesce((
             SELECT i.id FROM issue_inputs i JOIN task_runs prior ON prior.id = i.run_id
             WHERE i.task_id = r.task_id AND prior.assignment_id = r.assignment_id AND prior.rowid <= r.rowid
               AND json_extract(i.payload, '$.kind') = 'review_requested'
             ORDER BY prior.rowid DESC LIMIT 1
         ), json_extract(a.contract, '$.review.request_id'))
         FROM task_runs r JOIN runner_assignments a ON a.id = r.assignment_id
         WHERE r.id = ?1 AND json_extract(a.contract, '$.review.request_id') IS NOT NULL",
        [run], |row| row.get(0),
    ).optional()?.flatten())
}

pub(super) fn can_request_again(
    db: &Connection,
    review_task: &str,
    review_issue: &str,
    author_task: &str,
    candidate: &str,
    author_run: &str,
) -> Result<bool> {
    Ok(db
        .query_row(
            "SELECT v.review_issue_id = ?2 AND v.author_task_id = ?3 AND v.candidate != ?4
                AND EXISTS(SELECT 1 FROM task_runs consumed JOIN task_runs submitted ON submitted.id = ?5
                           WHERE consumed.id = p.run_id AND consumed.rowid <= submitted.rowid)
           FROM tend_reviews v LEFT JOIN issue_inputs p ON p.id = v.result_input_id
          WHERE v.review_task_id = ?1 ORDER BY v.rowid DESC LIMIT 1",
            params![review_task, review_issue, author_task, candidate, author_run],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(false))
}

pub(super) fn request_again(
    db: &Connection,
    runner: &RunnerProfile,
    operation: &WorkOperation,
    issue: &str,
    task: &str,
    work: &DispatchIssue,
) -> Result<()> {
    let prior = db
        .query_row(
            "SELECT a.runner_id, json_extract(a.contract, '$.placement.agent.id'),
                json_extract(a.contract, '$.review.author_workspace') IS NOT NULL
         FROM tasks t JOIN runner_assignments a ON a.id = t.current_assignment
         WHERE t.id = ?1 AND t.status IN ('completed','failed') AND a.state = 'accepted'
           AND NOT EXISTS(SELECT 1 FROM task_controls c WHERE c.assignment_id = t.current_assignment
                            AND c.state = 'pending' AND json_extract(c.action, '$.kind') = 'stop')",
            [task],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, bool>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((previous_runner, owner, shared_workspace)) = prior.filter(|_| runner.shared_reviews)
    else {
        return not_run(
            db,
            task,
            operation,
            "The original Review Task has not stopped or is not eligible to re-review.",
        );
    };
    let reuse = previous_runner == runner.runner_id && shared_workspace;
    if reuse {
        db.execute("UPDATE tasks SET status = 'waiting' WHERE id = ?1", [task])?;
    } else {
        // Preserve the named owner. Allocation to another Runner waits for
        // evidence from the exact predecessor assignment, even if the author
        // has already resumed and that proof is still in flight.
        db.execute(
            "UPDATE tasks SET status = 'awaiting_assignment', current_assignment = NULL, handoff = NULL,
                pinned_runner_id = ?2, pinned_agent_id = ?3 WHERE id = ?1",
            params![task, runner.runner_id, owner],
        )?;
    }
    let review = round(db, task, Some(&operation.operation_id))?.expect("round just bound");
    coordination::enqueue(
        db,
        task,
        IssueInput {
            input_id: operation.operation_id.clone(),
            child_issue_id: review.author_issue_id.clone(),
            child_task_id: review.author_task_id.clone(),
            cause: IssueInputCause::ReviewRequested {
                review,
                base_refs: work.base_refs.clone(),
            },
            summary: format!(
                "Explicit new candidate requested on the original Review Issue {issue}."
            ),
        },
    )?;
    if !reuse {
        super::runner::assign_task(db, task)?;
    }
    Ok(())
}

pub(super) fn not_run(
    db: &Connection,
    task: &str,
    operation: &WorkOperation,
    summary: &str,
) -> Result<()> {
    finish(
        db,
        task,
        Some(&operation.operation_id),
        &operation.operation_id,
        None,
        None,
        summary,
    )
}

/// Keep the first result or terminal no-result receipt with the exact source
/// Task. A finished or cancelled author is not replaced or reopened here.
pub(super) fn finish(
    db: &Connection,
    review_task: &str,
    request: Option<&str>,
    source: &str,
    run_id: Option<&str>,
    result: Option<&TendReviewConclusion>,
    summary: &str,
) -> Result<()> {
    let Some(review) = round(db, review_task, request)? else {
        return Ok(());
    };
    let issue = db.query_row(
        "SELECT review_issue_id FROM tend_reviews WHERE request_id = ?1 AND result_input_id IS NULL",
        [&review.request_id], |row| row.get::<_, String>(0),
    ).optional()?;
    let Some(review_issue) = issue else {
        return Ok(());
    };
    let id = protocol::new_id();
    let request_id = review.request_id.clone();
    let author = review.author_task_id.clone();
    coordination::enqueue(
        db,
        &author,
        IssueInput {
            input_id: id.clone(),
            child_issue_id: review_issue,
            child_task_id: review_task.to_owned(),
            cause: IssueInputCause::ReviewFinished {
                review,
                result: result.cloned(),
                run_id: run_id.map(str::to_owned),
                source_id: source.to_owned(),
            },
            summary: summary.to_owned(),
        },
    )?;
    db.execute(
        "UPDATE tend_reviews SET result_input_id = ?2 WHERE request_id = ?1",
        params![request_id, id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_result_survives_migration_and_late_delivery_cannot_settle_new_round() -> Result<()> {
        let db = Connection::open_in_memory()?;
        db.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE issues(id TEXT PRIMARY KEY);
             CREATE TABLE tasks(id TEXT PRIMARY KEY, issue_id TEXT);
             CREATE TABLE runner_operations(id TEXT PRIMARY KEY);
             CREATE TABLE issue_inputs(id TEXT PRIMARY KEY, task_id TEXT, payload TEXT, run_id TEXT, wake_at INTEGER);
             CREATE TABLE external_batches(task_id TEXT PRIMARY KEY);
             CREATE TABLE codebase_comment_batches(issue_id TEXT PRIMARY KEY);
             CREATE TABLE issue_input_order(seq INTEGER PRIMARY KEY, input_id TEXT NOT NULL UNIQUE);
             CREATE TABLE issue_input_flushes(input_id TEXT PRIMARY KEY, flush_id TEXT NOT NULL);
             INSERT INTO issues VALUES ('author-i'), ('review-i');
             INSERT INTO tasks VALUES ('author-t','author-i'), ('review-t','review-i');
             INSERT INTO runner_operations VALUES ('first'), ('second');",
        )?;
        migrate(&db)?;
        db.execute_batch(
            "INSERT INTO issue_inputs(id, task_id, payload) VALUES ('original-input', 'author-t', 'original');
             INSERT INTO tend_reviews VALUES ('first','author-i','author-t','review-i','review-t','old','scope one','original-input');",
        )?;
        migrate_rounds(&db)?;
        assert_eq!(binding(&db, "review-t")?.unwrap().candidate, "old");
        bind(
            &db,
            "second",
            "author-i",
            "author-t",
            "review-i",
            "review-t",
            "new",
            "scope two",
            "tasks/author-t",
        )?;
        let result = TendReviewConclusion {
            conclusion: "accepted".into(),
            evidence: "exact commit".into(),
        };
        finish(
            &db,
            "review-t",
            Some("first"),
            "duplicate",
            Some("old-run"),
            Some(&result),
            "duplicate",
        )?;
        assert_eq!(
            db.query_row("SELECT count(*) FROM issue_inputs", [], |row| row
                .get::<_, u32>(0))?,
            1
        );
        finish(
            &db,
            "review-t",
            Some("second"),
            "new-result",
            Some("new-run"),
            Some(&result),
            "accepted",
        )?;
        let payload: String = db.query_row(
            "SELECT p.payload FROM tend_reviews v JOIN issue_inputs p ON p.id = v.result_input_id WHERE v.request_id = 'second'",
            [], |row| row.get(0),
        )?;
        let input: IssueInput = serde_json::from_str(&payload)?;
        assert!(
            matches!(input.cause, IssueInputCause::ReviewFinished { review, result: Some(actual), source_id, .. }
            if review.candidate == "new" && review.scope == "scope two" && actual == result && source_id == "new-result")
        );
        Ok(())
    }
}
