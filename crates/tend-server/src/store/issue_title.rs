use super::{Store, control::ResultState};
use anyhow::Result;
use protocol::{IssueTitleActor, IssueTitleChange, IssueTitleHistory, RenameIssue};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

const HISTORY_PAGE_SIZE: usize = 12;
const CHANGE_COLUMNS: &str = "operation_id, issue_id, old_title, new_title, agent_id, changed_at";

pub(super) fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS issue_title_changes (
             seq INTEGER PRIMARY KEY,
             issue_id TEXT NOT NULL REFERENCES issues(id),
             operation_id TEXT NOT NULL,
             runner_operation_id TEXT UNIQUE REFERENCES runner_operations(id),
             agent_id TEXT,
             old_title TEXT NOT NULL,
             new_title TEXT NOT NULL,
             changed_at INTEGER NOT NULL DEFAULT (unixepoch()),
             CHECK ((agent_id IS NULL) = (runner_operation_id IS NULL)),
             CHECK (runner_operation_id IS NULL OR runner_operation_id = operation_id)
         );
         CREATE UNIQUE INDEX IF NOT EXISTS user_issue_title_operations ON issue_title_changes(operation_id)
             WHERE runner_operation_id IS NULL;
         CREATE INDEX IF NOT EXISTS issue_title_history ON issue_title_changes(issue_id, seq)
             WHERE old_title <> new_title;",
    )?;
    Ok(())
}

fn read_change(row: &rusqlite::Row<'_>, offset: usize) -> rusqlite::Result<IssueTitleChange> {
    Ok(IssueTitleChange {
        operation_id: row.get(offset)?,
        issue_id: row.get(offset + 1)?,
        old_title: row.get(offset + 2)?,
        new_title: row.get(offset + 3)?,
        actor: match row.get(offset + 4)? {
            Some(agent_id) => IssueTitleActor::Owner { agent_id },
            None => IssueTitleActor::User,
        },
        changed_at: row.get(offset + 5)?,
    })
}

// Call inside the same transaction as the original Runner operation receipt.
// The caller has already checked the exact source Issue, assignment and Run.
pub(super) fn apply_owner(
    db: &Connection,
    issue: &str,
    assignment: &str,
    operation: &str,
    agent: &str,
    title: &str,
) -> Result<()> {
    if !super::work::accepted_owner(db, issue)?.is_some_and(|(id, _)| id == assignment) {
        // A late original record is still received, but cannot rename the
        // Issue after a successor's claim. Do not strand the Runner outbox.
        return Ok(());
    }
    let changed = db.execute(
        "INSERT INTO issue_title_changes
           (issue_id, operation_id, runner_operation_id, agent_id, old_title, new_title)
         SELECT id, ?2, ?2, ?3, title, ?4 FROM issues WHERE id = ?1 AND title <> ?4",
        params![issue, operation, agent, title],
    )?;
    if changed == 1 {
        db.execute(
            "UPDATE issues SET title = ?2 WHERE id = ?1",
            params![issue, title],
        )?;
    }
    Ok(())
}

impl Store {
    pub fn rename_issue(
        &mut self,
        issue: &str,
        input: &RenameIssue,
    ) -> Result<ResultState<IssueTitleChange>> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let prior = tx
            .query_row(
                &format!(
                    "SELECT {CHANGE_COLUMNS} FROM issue_title_changes
                WHERE operation_id = ?1 AND runner_operation_id IS NULL"
                ),
                [&input.operation_id],
                |row| read_change(row, 0),
            )
            .optional()?;
        if let Some(prior) = prior {
            return Ok(
                if prior.issue_id == issue && prior.new_title == input.title {
                    ResultState::Found(prior)
                } else {
                    ResultState::Conflict
                },
            );
        }
        let change = tx
            .query_row(
                &format!(
                    "INSERT INTO issue_title_changes (issue_id, operation_id, old_title, new_title)
                SELECT id, ?2, title, ?3 FROM issues WHERE id = ?1 RETURNING {CHANGE_COLUMNS}"
                ),
                params![issue, input.operation_id, input.title],
                |row| read_change(row, 0),
            )
            .optional()?;
        let Some(change) = change else {
            return Ok(ResultState::Missing);
        };
        if change.old_title != change.new_title {
            tx.execute(
                "UPDATE issues SET title = ?2 WHERE id = ?1",
                params![issue, input.title],
            )?;
        }
        tx.commit()?;
        Ok(ResultState::Found(change))
    }

    pub fn issue_title_history(
        &self,
        issue: &str,
        before: i64,
    ) -> Result<Option<IssueTitleHistory>> {
        let tx = self.db.unchecked_transaction()?;
        if !tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM issues WHERE id = ?1)",
            [issue],
            |row| row.get::<_, bool>(0),
        )? {
            return Ok(None);
        }
        let history = history(&tx, issue, before)?;
        tx.commit()?;
        Ok(Some(history))
    }
}

pub(super) fn history(db: &Connection, issue: &str, before: i64) -> Result<IssueTitleHistory> {
    let mut statement = db.prepare(&format!(
        "SELECT seq, {CHANGE_COLUMNS} FROM issue_title_changes
         WHERE issue_id = ?1 AND old_title <> new_title AND seq < ?2 ORDER BY seq DESC LIMIT ?3"
    ))?;
    let mut rows = statement
        .query_map(params![issue, before, HISTORY_PAGE_SIZE + 1], |row| {
            Ok((row.get(0)?, read_change(row, 1)?))
        })?
        .collect::<rusqlite::Result<Vec<(i64, IssueTitleChange)>>>()?;
    let next_before = if rows.len() > HISTORY_PAGE_SIZE {
        rows.pop();
        rows.last().map(|(seq, _)| *seq)
    } else {
        None
    };
    Ok(IssueTitleHistory {
        changes: rows.into_iter().map(|(_, change)| change).collect(),
        next_before,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{RunnerProfile, WorkAction, WorkOperation};
    use serde_json::json;
    use std::path::Path;

    #[test]
    fn late_predecessor_receipt_cannot_rename_after_successor_claim() -> Result<()> {
        let mut store = Store::initialize(Path::new(":memory:"))?;
        store.db.execute_batch(
            "INSERT INTO chat_messages (id, chat_id, text) VALUES ('source', 'main', 'Fixture');
             INSERT INTO issues (id, source_seq, title, objective)
                VALUES ('issue', 1, 'Original', 'Keep this objective');
             INSERT INTO runners (id, host_id, profile, last_seen) VALUES ('runner', 'host', '{}', 0);
             INSERT INTO tasks (id, issue_id, category, status, repo_ids, required_capabilities)
                VALUES ('original', 'issue', 'implementation', 'completed', '[]', '[]'),
                       ('successor', 'issue', 'implementation', 'awaiting_receipt', '[]', '[]');",
        )?;
        for (id, task, actor, state) in [
            ("prior", "original", "old-owner", "accepted"),
            ("next", "successor", "new-owner", "issued"),
        ] {
            let contract = json!({
                "placement": {
                    "assignment_id": id, "runner_id": "runner", "host_id": "host",
                    "agent": {"id": actor, "category": "implementation", "revision": "r1"},
                },
                "task_id": task, "issue_id": "issue", "source_message_id": "source", "chat_id": "main",
                "work": {"title": "Original", "objective": "Keep this objective"},
            });
            store.db.execute(
                "INSERT INTO runner_assignments (id, task_id, runner_id, contract, state, expires_at)
                 VALUES (?1, ?2, 'runner', ?3, ?4, 0)",
                params![id, task, contract.to_string(), state],
            )?;
        }
        store.db.execute(
            r#"INSERT INTO task_runs (id, assignment_id, task_id, observation)
               VALUES ('original-run', 'prior', 'original', '{"turn_id":"original-turn"}')"#,
            [],
        )?;
        let runner: RunnerProfile = serde_json::from_value(json!({
            "runner_id":"runner", "host_id":"host", "repos": [], "agents": [],
            "slot_limit": 1, "running_slots": 0, "available_for_assignment": 1
        }))?;
        let operation = |id: &str, title: &str| WorkOperation {
            operation_id: id.into(),
            assignment_id: "prior".into(),
            run_id: "original-run".into(),
            action: WorkAction::RenameIssue {
                issue_id: "issue".into(),
                title: title.into(),
            },
        };
        let before = operation("before-claim", "Before successor claims");
        let late = operation("after-claim", "Must not overwrite");
        let apply = |store: &mut Store, op: &WorkOperation| -> Result<bool> {
            let tx = store.db.transaction()?;
            let accepted =
                super::super::coordination::apply(&tx, &runner, op, &Default::default())?;
            if accepted {
                tx.commit()?;
            }
            Ok(accepted)
        };

        assert!(apply(&mut store, &before)?);
        store.db.execute(
            "UPDATE runner_assignments SET state = 'accepted' WHERE id = 'next'",
            [],
        )?;
        assert!(apply(&mut store, &late)?);
        assert!(apply(&mut store, &before)?);
        assert!(apply(&mut store, &late)?);
        let live = store.issue_overview("issue")?.unwrap();
        assert_eq!(live.title, "Before successor claims");
        assert_eq!(live.owner_agent_id.as_deref(), Some("new-owner"));
        let audit = live.title_history.unwrap().changes;
        assert_eq!(audit.len(), 1);
        assert!(
            matches!(&audit[0].actor, IssueTitleActor::Owner { agent_id } if agent_id == "old-owner")
        );
        assert!(store.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM runner_operations WHERE id = 'after-claim')",
            [],
            |row| row.get::<_, bool>(0),
        )?);
        Ok(())
    }
}
