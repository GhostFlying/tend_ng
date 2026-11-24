use super::{PAGE_SIZE, Store};
use anyhow::Result;
use protocol::{
    ExternalFact, GitHubPull, IssueBoard, IssueCard, LinkPull, PullFacts, PullLink, PullLinkReceipt,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

pub enum Linked {
    Saved(PullLinkReceipt),
    Conflict,
    Missing,
}

pub fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE github_pulls (
             url TEXT PRIMARY KEY,
             repository TEXT NOT NULL,
             number INTEGER NOT NULL,
             due_at INTEGER DEFAULT (unixepoch()),
             facts TEXT,
             observed_at INTEGER,
             observation_error TEXT
         );
         CREATE INDEX github_pulls_due ON github_pulls(due_at) WHERE due_at IS NOT NULL;
         CREATE TABLE issue_pulls (
             issue_id TEXT NOT NULL REFERENCES issues(id),
             url TEXT NOT NULL REFERENCES github_pulls(url),
             PRIMARY KEY (issue_id, url)
         );
         CREATE TABLE pull_link_requests (
             operation_id TEXT PRIMARY KEY,
             issue_id TEXT NOT NULL,
             url TEXT NOT NULL,
             FOREIGN KEY (issue_id, url) REFERENCES issue_pulls(issue_id, url)
         );",
    )?;
    Ok(())
}

pub(super) fn ensure_observed(db: &Connection, pull: &GitHubPull) -> Result<()> {
    db.execute(
        "INSERT INTO github_pulls(url, repository, number) VALUES (?1, ?2, ?3)
         ON CONFLICT(url) DO NOTHING",
        params![pull.url(), pull.repository, pull.number],
    )?;
    Ok(())
}

pub(super) fn bind(db: &Connection, issue: &str, pull: &GitHubPull) -> Result<PullLinkReceipt> {
    ensure_observed(db, pull)?;
    let url = pull.url();
    db.execute(
        "INSERT INTO issue_pulls VALUES (?1, ?2) ON CONFLICT DO NOTHING",
        params![issue, url],
    )?;
    Ok(PullLinkReceipt {
        issue_id: issue.to_owned(),
        url,
    })
}

impl Store {
    pub fn link_pull(
        &mut self,
        issue: &str,
        request: &LinkPull,
        pull: &GitHubPull,
    ) -> Result<Linked> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some((saved_issue, saved_url)) = tx
            .query_row(
                "SELECT issue_id, url FROM pull_link_requests WHERE operation_id = ?1",
                [&request.operation_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
        {
            return Ok(if saved_issue == issue && saved_url == pull.url() {
                Linked::Saved(PullLinkReceipt {
                    issue_id: saved_issue,
                    url: saved_url,
                })
            } else {
                Linked::Conflict
            });
        }
        if !issue_exists(&tx, issue)? {
            return Ok(Linked::Missing);
        }
        let receipt = bind(&tx, issue, pull)?;
        tx.execute(
            "INSERT INTO pull_link_requests VALUES (?1, ?2, ?3)",
            params![request.operation_id, receipt.issue_id, receipt.url],
        )?;
        tx.commit()?;
        Ok(Linked::Saved(receipt))
    }

    pub fn issue_pulls(&self, issue: &str) -> Result<Option<Vec<PullLink>>> {
        if !issue_exists(&self.db, issue)? {
            return Ok(None);
        }
        Ok(Some(
            pulls(&self.db, &[issue])?
                .into_iter()
                .map(|(_, pull)| pull)
                .collect(),
        ))
    }

    pub fn issue_board(&self, chat: &str, before: i64) -> Result<IssueBoard> {
        let tx = self.db.unchecked_transaction()?;
        let mut page = {
            let mut query = tx.prepare(
                "SELECT i.rowid, i.id, i.parent_issue_id, i.title, i.state, t.status
                 FROM issues i JOIN chat_messages source ON source.seq = i.source_seq
                 JOIN tasks t ON t.rowid = (SELECT rowid FROM tasks WHERE issue_id = i.id ORDER BY rowid DESC LIMIT 1)
                 WHERE source.chat_id = ?1 AND i.rowid < ?2 ORDER BY i.rowid DESC LIMIT ?3",
            )?;
            query
                .query_map(params![chat, before, PAGE_SIZE + 1], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        IssueCard {
                            id: row.get(1)?,
                            parent_issue_id: row.get(2)?,
                            title: row.get(3)?,
                            issue_state: row.get(4)?,
                            task_state: row.get(5)?,
                            pulls: Vec::new(),
                        },
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let next_before = if page.len() > PAGE_SIZE {
            page.pop();
            page.last().map(|(seq, _)| *seq)
        } else {
            None
        };
        let indices: std::collections::HashMap<_, _> = page
            .iter()
            .enumerate()
            .map(|(index, (_, issue))| (issue.id.as_str(), index))
            .collect();
        let ids: Vec<_> = indices.keys().copied().collect();
        let loaded = pulls(&tx, &ids)?
            .into_iter()
            .map(|(id, pull)| (indices[id.as_str()], pull))
            .collect::<Vec<_>>();
        for (index, pull) in loaded {
            page[index].1.pulls.push(pull);
        }
        tx.commit()?;
        Ok(IssueBoard {
            issues: page.into_iter().map(|(_, card)| card).collect(),
            next_before,
        })
    }

    pub fn due_pull(&self) -> Result<Option<GitHubPull>> {
        Ok(self
            .db
            .query_row(
                "SELECT repository, number FROM github_pulls WHERE due_at <= unixepoch()
             ORDER BY due_at, rowid LIMIT 1",
                [],
                |row| {
                    Ok(GitHubPull {
                        repository: row.get(0)?,
                        number: row.get(1)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn observe_pull(
        &self,
        pull: &GitHubPull,
        observation: Result<(PullFacts, Vec<ExternalFact>), &'static str>,
    ) -> Result<()> {
        let tx = self.db.unchecked_transaction()?;
        match &observation {
            Ok((facts, _)) => {
                let refresh = facts.state == "open" || facts.ci.as_deref() == Some("pending")
                    || tx.query_row("SELECT EXISTS(SELECT 1 FROM external_watches WHERE url = ?1 AND state = 'active')
                                            OR EXISTS(SELECT 1 FROM external_routes WHERE url = ?1)",
                        [pull.url()], |row| row.get::<_, bool>(0))?;
                tx.execute(
                    "UPDATE github_pulls SET facts = ?2, observed_at = unixepoch(), observation_error = NULL,
                       due_at = CASE WHEN ?3 THEN unixepoch() + 300 END WHERE url = ?1",
                    params![pull.url(), serde_json::to_string(&facts)?, refresh],
                )?;
            }
            Err(reason) => {
                tx.execute(
                    "UPDATE github_pulls SET observation_error = ?2, due_at = unixepoch() + 300 WHERE url = ?1",
                    params![pull.url(), reason],
                )?;
            }
        }
        let (facts, checks) = match &observation {
            Ok((facts, checks)) => (Ok(facts), checks.as_slice()),
            Err(error) => (Err(*error), &[][..]),
        };
        super::external::observe(&tx, pull, facts, checks)?;
        tx.commit()?;
        Ok(())
    }
}

pub(super) fn issue_exists(db: &Connection, issue: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM issues WHERE id = ?1)",
        [issue],
        |row| row.get(0),
    )?)
}

fn pulls(db: &Connection, issues: &[&str]) -> Result<Vec<(String, PullLink)>> {
    if issues.is_empty() {
        return Ok(Vec::new());
    }
    let slots = vec!["?"; issues.len()].join(",");
    let mut query = db.prepare(&format!(
        "SELECT link.issue_id, p.url, p.number, p.facts, p.observed_at, p.observation_error
         FROM issue_pulls link JOIN github_pulls p ON p.url = link.url
         WHERE link.issue_id IN ({slots}) ORDER BY link.issue_id, link.rowid",
    ))?;
    query
        .query_map(rusqlite::params_from_iter(issues), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })?
        .map(|row| {
            let (id, url, number, json, observed_at, observation_error) = row?;
            let facts = json.as_deref().map(serde_json::from_str).transpose()?;
            Ok((
                id,
                PullLink {
                    url,
                    number,
                    facts,
                    observed_at,
                    observation_error,
                },
            ))
        })
        .collect()
}
