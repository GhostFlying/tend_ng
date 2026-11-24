use super::Store;
use anyhow::Result;
use protocol::WorkOperation;
use rusqlite::{Connection, params};
use serde_json::Value;

pub fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE issue_reviews (
            operation_id TEXT PRIMARY KEY REFERENCES runner_operations(id),
            issue_id TEXT NOT NULL REFERENCES issues(id),
            run_id TEXT NOT NULL REFERENCES task_runs(id)
         );
         CREATE INDEX issue_reviews_by_issue ON issue_reviews(issue_id);",
    )?;
    Ok(())
}

pub fn record(db: &Connection, issue: &str, operation: &WorkOperation) -> Result<()> {
    db.execute(
        "INSERT INTO issue_reviews VALUES (?1, ?2, ?3)",
        params![operation.operation_id, issue, operation.run_id],
    )?;
    Ok(())
}

impl Store {
    pub fn issue_reviews(&self, issue: &str) -> Result<Vec<Value>> {
        let mut statement = self.db.prepare(
            "SELECT r.operation FROM issue_reviews review JOIN runner_operations r ON r.id = review.operation_id
             WHERE review.issue_id = ?1 ORDER BY review.rowid LIMIT 200",
        )?;
        statement
            .query_map([issue], |row| row.get::<_, String>(0))?
            .map(|raw| Ok(serde_json::from_str(&raw?)?))
            .collect()
    }
}
