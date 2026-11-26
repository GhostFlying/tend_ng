use super::Store;
use anyhow::Result;
use protocol::{
    ExternalFact, GitHubPull, LinkPull, PullFacts, PullLink, PullLinkReceipt, PullProvider, PullRef,
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

pub fn migrate_providers(db: &Connection) -> Result<()> {
    // SQLite rewrites the inbound foreign keys in place. Keeping the rows also
    // preserves observation/association order and all receipts and route data.
    db.execute_batch(
        "ALTER TABLE github_pulls RENAME TO pulls;
         ALTER TABLE pulls ADD COLUMN provider TEXT NOT NULL DEFAULT 'github'
           CHECK (provider IN ('github', 'codebase'));
         DROP INDEX github_pulls_due;
         CREATE INDEX pulls_due ON pulls(provider, due_at) WHERE due_at IS NOT NULL;",
    )?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn restore_legacy_schema(db: &Connection) -> Result<()> {
    db.execute_batch(
        "DROP INDEX pulls_due;
         ALTER TABLE pulls DROP COLUMN provider;
         ALTER TABLE pulls RENAME TO github_pulls;
         CREATE INDEX github_pulls_due ON github_pulls(due_at) WHERE due_at IS NOT NULL;",
    )?;
    Ok(())
}

pub(super) fn ensure_observed(db: &Connection, pull: &PullRef) -> Result<()> {
    db.execute(
        "INSERT INTO pulls(url, repository, number, provider) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(url) DO NOTHING",
        params![
            pull.url(),
            pull.repository,
            pull.number,
            pull.provider.as_str()
        ],
    )?;
    Ok(())
}

pub(super) fn bind(db: &Connection, issue: &str, pull: &PullRef) -> Result<PullLinkReceipt> {
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
    pub fn link_pull(&mut self, issue: &str, request: &LinkPull, pull: &PullRef) -> Result<Linked> {
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

    pub fn due_pull(&self, provider: PullProvider) -> Result<Option<PullRef>> {
        Ok(self
            .db
            .query_row(
                "SELECT repository, number FROM pulls WHERE provider = ?1 AND due_at <= unixepoch()
             ORDER BY due_at, rowid LIMIT 1",
                [provider.as_str()],
                |row| {
                    Ok(PullRef {
                        provider,
                        repository: row.get(0)?,
                        number: row.get(1)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn observe_pull(
        &mut self,
        pull: &PullRef,
        observation: Result<(PullFacts, Vec<ExternalFact>), &'static str>,
    ) -> Result<()> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        match &observation {
            Ok((facts, _)) => {
                let refresh = facts.state == "open" || facts.ci.as_deref() == Some("pending")
                    || tx.query_row("SELECT EXISTS(SELECT 1 FROM external_watches WHERE url = ?1 AND state = 'active')
                                            OR EXISTS(SELECT 1 FROM external_routes WHERE url = ?1)",
                        [pull.url()], |row| row.get::<_, bool>(0))?;
                tx.execute(
                    "UPDATE pulls SET facts = ?2, observed_at = unixepoch(), observation_error = NULL,
                       due_at = CASE WHEN ?3 THEN unixepoch() + 300 END WHERE url = ?1",
                    params![pull.url(), serde_json::to_string(&facts)?, refresh],
                )?;
            }
            Err(reason) => {
                tx.execute(
                    "UPDATE pulls SET observation_error = ?2, due_at = unixepoch() + 300 WHERE url = ?1",
                    params![pull.url(), reason],
                )?;
            }
        }
        let (facts, checks) = match &observation {
            Ok((facts, checks)) => (Ok(facts), checks.as_slice()),
            Err(error) => (Err(*error), &[][..]),
        };
        if let Some(github_pull) = pull.github() {
            super::external::observe(&tx, &github_pull, facts, checks)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn watched_ci_heads(&self, pull: &GitHubPull) -> Result<Vec<String>> {
        let mut statement = self.db.prepare(
            "SELECT DISTINCT json_extract(condition, '$.head_sha') AS head
             FROM (
                 SELECT condition FROM external_watches WHERE url = ?1 AND state = 'active'
                 UNION ALL SELECT condition FROM external_routes WHERE url = ?1
             )
             WHERE json_extract(condition, '$.event') = 'ci' AND head IS NOT NULL",
        )?;
        Ok(statement
            .query_map([pull.url()], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }
}

pub(super) fn issue_exists(db: &Connection, issue: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM issues WHERE id = ?1)",
        [issue],
        |row| row.get(0),
    )?)
}

pub(super) fn pulls(db: &Connection, issues: &[&str]) -> Result<Vec<(String, PullLink)>> {
    if issues.is_empty() {
        return Ok(Vec::new());
    }
    let slots = vec!["?"; issues.len()].join(",");
    let mut query = db.prepare(&format!(
        "SELECT link.issue_id, p.url, p.number, p.facts, p.observed_at, p.observation_error, p.repository, p.provider = 'codebase'
         FROM issue_pulls link JOIN pulls p ON p.url = link.url
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
                row.get(6)?,
                row.get::<_, bool>(7)?,
            ))
        })?
        .map(|row| {
            let (id, url, number, json, observed_at, observation_error, repository, codebase) =
                row?;
            let facts = json.as_deref().map(serde_json::from_str).transpose()?;
            Ok((
                id,
                PullLink {
                    url,
                    number,
                    repository,
                    provider: if codebase {
                        PullProvider::Codebase
                    } else {
                        PullProvider::GitHub
                    },
                    facts,
                    observed_at,
                    observation_error,
                },
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn legacy_upgrade_preserves_links_receipts_discoveries_and_external_references() -> Result<()> {
        for version in [34, 35, 36] {
            upgrade_from(version)?;
        }
        Ok(())
    }

    fn upgrade_from(version: i64) -> Result<()> {
        let directory =
            std::env::temp_dir().join(format!("tend-pull-migration-{}", protocol::new_id()));
        std::fs::create_dir(&directory)?;
        let result = (|| {
            let path = directory.join("server.sqlite");
            let pull = PullRef::parse("https://github.com/example/guide/pull/7").unwrap();
            let url = pull.url();
            {
                let store = Store::initialize(&path)?;
                // Restore the legacy provider schema before writing any fixtures.
                restore_legacy_schema(&store.db)?;
                store.db.pragma_update(None, "user_version", version)?;
                if version == 34 {
                    store
                        .db
                        .execute_batch("DROP TABLE runner_auto_assignment_order;")?;
                }
                if version < 36 {
                    store.db.execute_batch(
                        "DROP VIEW runner_handoff_limits;
                         ALTER TABLE runner_replacements DROP COLUMN occupied_slots;
                         ALTER TABLE runner_replacements DROP COLUMN slot_budget;",
                    )?;
                }
                store.db.execute_batch(
                    "INSERT INTO chat_messages (seq, id, chat_id, text) VALUES (1, 'source', 'main', 'Fixture');
                     INSERT INTO issues (id, source_seq, title, objective) VALUES ('issue', 1, 'Fixture', '');
                     INSERT INTO runners (id, host_id, profile, last_seen) VALUES
                         ('runner', 'fixture', '{\"slot_limit\":4}', 0), ('successor', 'fixture', '{}', 0);
                     INSERT INTO runner_replacements (predecessor, successor) VALUES ('runner', 'successor');
                     INSERT INTO tasks (id, issue_id, category, status, repo_ids, required_capabilities)
                         VALUES ('task', 'issue', 'fixture', 'completed', '[]', '[]');",
                )?;
                if version >= 35 {
                    store.db.execute_batch(
                        "INSERT INTO runner_auto_assignment_order VALUES ('runner', 12);",
                    )?;
                }
                if version >= 36 {
                    store.db.execute_batch(
                        "UPDATE runner_replacements SET slot_budget = 4, occupied_slots = 1;",
                    )?;
                }
                let facts = json!({
                    "title": "Migrated PR", "state": "open", "draft": false,
                    "head_sha": "old-head", "ci": "success",
                });
                store.db.execute(
                    "INSERT INTO github_pulls VALUES (?1, 'example/guide', 7, 0, ?2, 42, NULL)",
                    params![url, facts.to_string()],
                )?;
                store
                    .db
                    .execute("INSERT INTO issue_pulls VALUES ('issue', ?1)", [&url])?;
                store.db.execute(
                    "INSERT INTO pull_link_requests VALUES ('legacy-link', 'issue', ?1)",
                    [&url],
                )?;
                store.db.execute(
                    "INSERT INTO external_routes VALUES ('route', 1, ?1, '{}', 'optional')",
                    [&url],
                )?;
                store.db.execute(
                    "INSERT INTO external_watches
                        (id, issue_id, task_id, url, condition, mode, route_id, origin_issue)
                        VALUES ('watch', 'issue', 'task', ?1, '{}', 'wait', 'route', 'issue')",
                    [&url],
                )?;
                store.db.execute(
                    "INSERT INTO external_facts (id, url, fact) VALUES ('fact', ?1, '{}')",
                    [&url],
                )?;
                store.db.execute(
                    "INSERT INTO external_latest VALUES (?1, 'pull', 'fact')",
                    [&url],
                )?;
                store.db.execute_batch(
                    "INSERT INTO external_deliveries (id, watch_id, fact_id)
                         VALUES ('delivery', 'watch', 'fact');",
                )?;
                let branch = json!({
                    "repo_id": "guide", "repository": "example/guide",
                    "head_repository": "example/guide", "branch": "example",
                });
                let result = json!([{"branch": branch, "state": "linked", "url": url}]);
                store.db.execute(
                    "INSERT INTO pull_discoveries VALUES ('legacy-find', 'issue', 'task', 1, 'completed', ?1, ?2),
                        ('pending-find', 'issue', 'task', 0, 'pending', ?1, '[]')",
                    params![json!([branch]).to_string(), result.to_string()],
                )?;
            }
            let mut store = Store::initialize(&path)?;
            let upgraded: i64 = store
                .db
                .pragma_query_value(None, "user_version", |row| row.get(0))?;
            assert_eq!(upgraded, crate::store::SCHEMA_VERSION);
            let runner_order: Option<i64> = store
                .db
                .query_row("SELECT last_selected FROM runner_auto_assignment_order WHERE runner_id = 'runner'", [], |row| row.get(0))
                .optional()?;
            assert_eq!(runner_order, (version >= 35).then_some(12));
            let limit = store.db.query_row(
                "SELECT limit_slots FROM runner_handoff_limits WHERE successor = 'successor'",
                [],
                |row| row.get::<_, i64>(0),
            )?;
            assert_eq!(limit, if version >= 36 { 3 } else { 0 });
            let links = store.issue_pulls("issue")?.unwrap();
            assert_eq!(links.len(), 1);
            assert_eq!(links[0].provider, PullProvider::GitHub);
            assert_eq!(serde_json::to_value(&links[0])?["provider"], "github");
            assert_eq!(links[0].repository, "example/guide");
            assert_eq!(links[0].observed_at, Some(42));
            assert_eq!(links[0].facts.as_ref().unwrap().title, "Migrated PR");
            assert_eq!(store.due_pull(PullProvider::GitHub)?, Some(pull));
            let pull = PullRef::parse(&url).unwrap();
            assert!(matches!(
                store.link_pull("issue", &LinkPull {
                    operation_id: "legacy-link".into(), url: format!("{url}/#legacy")
                }, &pull)?,
                Linked::Saved(receipt) if receipt.url == url
            ));
            let saved = store.discoveries("issue", Some("legacy-find"))?.unwrap();
            assert_eq!(saved[0].results[0].branch.provider, PullProvider::GitHub);
            assert_eq!(saved[0].results[0].url.as_deref(), Some(url.as_str()));
            let pending = store.pending_discovery(PullProvider::GitHub)?.unwrap();
            assert_eq!(pending.id, "pending-find");
            assert!(store.pending_discovery(PullProvider::Codebase)?.is_none());
            assert_eq!(
                store.db.query_row(
                    "SELECT w.url FROM external_deliveries d
                     JOIN external_watches w ON d.watch_id = w.id
                     JOIN external_routes r ON w.route_id = r.id AND w.url = r.url
                     JOIN external_latest l ON d.fact_id = l.fact_id AND l.url = r.url
                     WHERE d.id = 'delivery'",
                    [],
                    |row| row.get::<_, String>(0)
                )?,
                url
            );
            assert!(
                !store
                    .db
                    .prepare("PRAGMA foreign_key_check")?
                    .query([])?
                    .next()?
                    .is_some()
            );
            for table in [
                "issue_pulls",
                "external_routes",
                "external_watches",
                "external_facts",
                "external_latest",
            ] {
                assert!(store.db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_list(?1) WHERE \"table\" = 'pulls')",
                    [table], |row| row.get::<_, bool>(0)
                )?, "{table}");
            }
            assert!(
                store
                    .db
                    .execute("DELETE FROM pulls WHERE url = ?1", [&url])
                    .is_err()
            );
            Ok(())
        })();
        std::fs::remove_dir_all(directory)?;
        result
    }
}
