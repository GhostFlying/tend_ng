use super::coordination;
use anyhow::Result;
use protocol::{IssueInput, IssueInputCause, TendReview, TendReviewConclusion};
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
) -> Result<()> {
    db.execute(
        "INSERT INTO tend_reviews (request_id, author_issue_id, author_task_id, review_issue_id,
                                   review_task_id, candidate, scope)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            request,
            author_issue,
            author_task,
            review_issue,
            review_task,
            candidate,
            scope
        ],
    )?;
    Ok(())
}

pub(super) fn binding(db: &Connection, review_task: &str) -> Result<Option<TendReview>> {
    Ok(db
        .query_row(
            "SELECT request_id, author_issue_id, author_task_id, candidate, scope
         FROM tend_reviews WHERE review_task_id = ?1",
            [review_task],
            |row| {
                Ok(TendReview {
                    request_id: row.get(0)?,
                    author_issue_id: row.get(1)?,
                    author_task_id: row.get(2)?,
                    candidate: row.get(3)?,
                    scope: row.get(4)?,
                })
            },
        )
        .optional()?)
}

/// Keep the first result or terminal no-result receipt with the exact source
/// Task. A finished or cancelled author is not replaced or reopened here.
pub(super) fn finish(
    db: &Connection,
    review_task: &str,
    source: &str,
    run_id: Option<&str>,
    result: Option<&TendReviewConclusion>,
    summary: &str,
) -> Result<()> {
    let Some(review) = binding(db, review_task)? else {
        return Ok(());
    };
    let issue = db.query_row(
        "SELECT review_issue_id FROM tend_reviews WHERE review_task_id = ?1 AND result_input_id IS NULL",
        [review_task], |row| row.get::<_, String>(0),
    ).optional()?;
    let Some(review_issue) = issue else {
        return Ok(());
    };
    let id = protocol::new_id();
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
        "UPDATE tend_reviews SET result_input_id = ?2 WHERE review_task_id = ?1",
        params![review_task, id],
    )?;
    Ok(())
}
