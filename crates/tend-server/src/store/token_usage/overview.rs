use super::{Breakdown, Summary, breakdown};
use crate::store::Store;
use anyhow::{Result, ensure};
use protocol::TokenOverviewCursor;
use rusqlite::{Connection, params};
use serde::Serialize;

const PAGE_SIZE: usize = 30;
const PAGE_QUERY: &str = "WITH page AS MATERIALIZED (
    SELECT id, title, source_seq FROM issues
    WHERE parent_issue_id IS ?1 AND (source_seq, id) < (?2, ?3)
    ORDER BY source_seq DESC, id DESC LIMIT ?4
)
SELECT p.id, n.number, p.title, d.summary, s.summary, p.source_seq,
    EXISTS(SELECT 1 FROM issues c WHERE c.parent_issue_id = p.id)
FROM page p
JOIN issue_short_numbers n ON n.issue_id = p.id
LEFT JOIN token_issue_rollups d ON d.issue_id = p.id AND d.scope = 'direct'
LEFT JOIN token_issue_rollups s ON s.issue_id = p.id AND s.scope = 'subtree'
ORDER BY p.source_seq DESC, p.id DESC";

pub(in crate::store) fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE INDEX issues_token_usage_page ON issues(parent_issue_id, source_seq DESC, id DESC);
         CREATE TABLE token_global_rollup (
             id INTEGER PRIMARY KEY CHECK (id = 1),
             summary TEXT NOT NULL
         );",
    )?;
    let mut total = Summary::default();
    let mut query = db.prepare("SELECT summary FROM token_issue_rollups WHERE scope = 'direct'")?;
    let mut rows = query.query([])?;
    while let Some(row) = rows.next()? {
        total.merge(&serde_json::from_str(row.get_ref(0)?.as_str()?)?);
    }
    db.execute(
        "INSERT INTO token_global_rollup VALUES (1, ?1)",
        [serde_json::to_string(&total)?],
    )?;
    Ok(())
}

pub(super) fn read_global(db: &Connection) -> Result<Summary> {
    Ok(db
        .prepare_cached("SELECT summary FROM token_global_rollup WHERE id = 1")?
        .query_row([], |row| {
            Ok(serde_json::from_str(row.get_ref(0)?.as_str()?))
        })??)
}

pub(super) fn write_global(db: &Connection, summary: &Summary) -> Result<()> {
    ensure!(
        db.prepare_cached("UPDATE token_global_rollup SET summary = ?1 WHERE id = 1")?
            .execute([serde_json::to_string(summary)?])?
            == 1,
        "missing global Token rollup"
    );
    Ok(())
}

#[derive(Serialize)]
pub struct TokenOverview {
    #[serde(skip_serializing_if = "Option::is_none")]
    total: Option<Breakdown>,
    issues: Vec<OverviewIssue>,
    next_before: Option<String>,
}

#[derive(Serialize)]
struct OverviewIssue {
    issue_id: String,
    number: i64,
    title: String,
    direct: Breakdown,
    subtree: Breakdown,
    has_children: bool,
}

impl Store {
    pub fn token_overview(
        &self,
        parent: Option<&str>,
        before: Option<&TokenOverviewCursor>,
    ) -> Result<Option<TokenOverview>> {
        // A root page and its total must see the same sync/rebuild snapshot.
        let tx = self.db.unchecked_transaction()?;
        let total = if let Some(parent) = parent {
            if !tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM issues WHERE id = ?1)",
                [parent],
                |r| r.get::<_, bool>(0),
            )? {
                return Ok(None);
            }
            None
        } else {
            Some(read_global(&tx)?.into())
        };
        let (source_seq, issue_id) = before
            .map(|key| (key.source_seq, key.issue_id.as_str()))
            // Valid Issue IDs sort before '~', including at the largest sequence.
            .unwrap_or((i64::MAX, "~"));
        let mut issues = Vec::with_capacity(PAGE_SIZE);
        let mut last = None;
        let mut next_before = None;
        {
            let mut query = tx.prepare_cached(PAGE_QUERY)?;
            let mut rows = query.query(params![parent, source_seq, issue_id, PAGE_SIZE + 1])?;
            while let Some(row) = rows.next()? {
                if issues.len() == PAGE_SIZE {
                    next_before = last.as_ref().map(TokenOverviewCursor::encode);
                    break;
                }
                let issue: String = row.get(0)?;
                last = Some(TokenOverviewCursor {
                    source_seq: row.get(5)?,
                    issue_id: issue.clone(),
                });
                issues.push(OverviewIssue {
                    issue_id: issue,
                    number: row.get(1)?,
                    title: row.get(2)?,
                    direct: breakdown(row.get(3)?)?,
                    subtree: breakdown(row.get(4)?)?,
                    has_children: row.get(6)?,
                });
            }
        }
        tx.commit()?;
        Ok(Some(TokenOverview {
            total,
            issues,
            next_before,
        }))
    }
}

#[cfg(test)]
mod tests;
