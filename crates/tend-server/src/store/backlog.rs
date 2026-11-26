use super::{Store, control::ResultState, work};
use anyhow::Result;
use protocol::{ControlAction, IssueDetail};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::{Value, json};

const TREE: &str = "WITH RECURSIVE tree(id) AS (SELECT ?1 UNION ALL
    SELECT i.id FROM issues i JOIN tree ON i.parent_issue_id = tree.id)";

pub(super) fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS issue_parks (issue_id TEXT PRIMARY KEY REFERENCES issues(id));
         CREATE TABLE IF NOT EXISTS parked_tasks (
             task_id TEXT PRIMARY KEY REFERENCES tasks(id), control_id TEXT REFERENCES task_controls(id)
         );
         CREATE TABLE IF NOT EXISTS issue_admission_operations (
             actor TEXT NOT NULL, id TEXT NOT NULL, issue_id TEXT NOT NULL REFERENCES issues(id),
             action TEXT NOT NULL, receipt TEXT NOT NULL, PRIMARY KEY(actor, id)
         );
         CREATE TABLE IF NOT EXISTS issue_input_order (
             seq INTEGER PRIMARY KEY, input_id TEXT NOT NULL UNIQUE
         );
         CREATE TABLE IF NOT EXISTS issue_input_flushes (
             input_id TEXT PRIMARY KEY REFERENCES issue_inputs(id) ON DELETE CASCADE,
             flush_id TEXT NOT NULL
         );",
    )?;
    Ok(())
}

pub(super) fn parked(db: &Connection, issue: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM issue_parks WHERE issue_id = ?1)",
        [issue],
        |row| row.get(0),
    )?)
}

pub(super) fn old_task(db: &Connection, task: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM parked_tasks WHERE task_id = ?1)",
        [task],
        |row| row.get(0),
    )?)
}

pub(super) fn parked_task_scope(db: &Connection, task: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM tasks t JOIN issue_parks p ON p.issue_id = t.issue_id WHERE t.id = ?1)",
        [task], |row| row.get(0),
    )?)
}

pub(super) fn park_late_child(db: &Connection, issue: &str, task: &str) -> Result<()> {
    db.execute("INSERT INTO issue_parks VALUES (?1)", [issue])?;
    db.execute("INSERT INTO parked_tasks VALUES (?1, NULL)", [task])?;
    db.execute(
        "UPDATE tasks SET status = 'cancelled' WHERE id = ?1",
        [task],
    )?;
    Ok(())
}

fn original(
    db: &Connection,
    actor: &str,
    operation: &str,
    issue: &str,
    action: &str,
) -> Result<Option<ResultState<Value>>> {
    let previous = db.query_row(
        "SELECT issue_id, action, receipt FROM issue_admission_operations WHERE actor = ?1 AND id = ?2",
        params![actor, operation],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)),
    ).optional()?;
    Ok(match previous {
        Some((target, kind, saved)) if target == issue && kind == action => {
            Some(ResultState::Found(serde_json::from_str(&saved)?))
        }
        Some(_) => Some(ResultState::Conflict),
        None => None,
    })
}

fn save(
    db: &Connection,
    actor: &str,
    operation: &str,
    issue: &str,
    action: &str,
    receipt: &Value,
) -> Result<()> {
    db.execute(
        "INSERT INTO issue_admission_operations VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            actor,
            operation,
            issue,
            action,
            serde_json::to_string(receipt)?
        ],
    )?;
    Ok(())
}

fn state(db: &Connection, issue: &str, chat: Option<&str>) -> Result<Option<String>> {
    Ok(db
        .query_row(
            "SELECT i.state FROM issues i JOIN chat_messages m ON m.seq = i.source_seq
         WHERE i.id = ?1 AND (?2 IS NULL OR m.chat_id = ?2)",
            params![issue, chat],
            |row| row.get(0),
        )
        .optional()?)
}

impl Store {
    pub fn return_to_backlog(
        &mut self,
        issue: &str,
        chat: Option<&str>,
        operation: &str,
    ) -> Result<ResultState<Value>> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(state) = state(&tx, issue, chat)? else {
            return Ok(ResultState::Missing);
        };
        let actor = if chat.is_some() { "router" } else { "user" };
        if let Some(result) = original(&tx, actor, operation, issue, "backlog")? {
            return Ok(result);
        }
        if state != "open" || super::coordination::latest_task(&tx, issue)?.is_none() {
            return Ok(ResultState::Conflict);
        }
        let mut controls = Vec::new();
        if !parked(&tx, issue)? {
            tx.execute(
                &format!("{TREE} INSERT OR IGNORE INTO issue_parks SELECT id FROM tree"),
                [issue],
            )?;
            let tasks = {
                let mut statement = tx.prepare(&format!(
                    "{TREE} SELECT t.id, t.status IN ('completed','failed','cancelled')
                     FROM tasks t JOIN tree ON tree.id = t.issue_id
                     LEFT JOIN parked_tasks p ON p.task_id = t.id WHERE p.task_id IS NULL
                     ORDER BY t.rowid"
                ))?;
                statement
                    .query_map([issue], |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            };
            for (task, terminal) in tasks {
                let command = tx
                    .query_row(
                        "SELECT id FROM task_controls WHERE task_id = ?1 AND state = 'pending'
                       AND json_extract(action, '$.kind') = 'stop' ORDER BY rowid LIMIT 1",
                        [&task],
                        |r| r.get::<_, String>(0),
                    )
                    .optional()?;
                let control = if command.is_none() && !terminal {
                    let id = protocol::new_id();
                    super::control::submit(&tx, &id, &task, None, &ControlAction::Stop)?;
                    Some(id)
                } else {
                    command
                };
                controls.extend(control.iter().cloned());
                // Includes already settled Tasks: a buffered request from one of
                // their original Runs must not undo a later explicit start.
                tx.execute(
                    "INSERT INTO parked_tasks VALUES (?1, ?2)",
                    params![task, control],
                )?;
                super::coordination::flush_pending(&tx, &task)?;
                super::external::clear_waits(&tx, &task)?;
            }
        }
        let receipt = json!({"issue_id": issue, "operation_id": operation, "state": "backlog", "stop_control_ids": controls});
        save(&tx, actor, operation, issue, "backlog", &receipt)?;
        super::board::commit(tx)?;
        Ok(ResultState::Found(receipt))
    }

    pub fn start_issue(
        &mut self,
        issue: &str,
        chat: Option<&str>,
        operation: Option<&str>,
    ) -> Result<ResultState<Value>> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(state) = state(&tx, issue, chat)? else {
            return Ok(ResultState::Missing);
        };
        let actor = if chat.is_some() { "router" } else { "user" };
        if let Some(operation) = operation
            && let Some(result) = original(&tx, actor, operation, issue, "start")?
        {
            return Ok(result);
        }
        let detail = if parked(&tx, issue)? {
            if operation.is_none() || state != "open" || tx.query_row(
                "WITH RECURSIVE parents(id) AS (
                   SELECT parent_issue_id FROM issues WHERE id = ?1
                   UNION ALL SELECT i.parent_issue_id FROM issues i JOIN parents p ON p.id = i.id
                 ) SELECT EXISTS(SELECT 1 FROM parents JOIN issue_parks p ON p.issue_id = parents.id)",
                [issue], |r| r.get::<_, bool>(0),
            )? || tx.query_row(&format!(
                "{TREE} SELECT EXISTS(SELECT 1 FROM tasks t JOIN tree ON tree.id = t.issue_id
                 LEFT JOIN parked_tasks p ON p.task_id = t.id
                 LEFT JOIN task_controls c ON c.id = p.control_id
                 WHERE t.status NOT IN ('completed','failed','cancelled') OR c.state = 'pending')"),
                [issue], |r| r.get::<_, bool>(0),
            )? {
                return Ok(ResultState::Conflict);
            }
            let Some(previous) = super::coordination::latest_task(&tx, issue)? else {
                return Ok(ResultState::Conflict);
            };
            if super::tend_review::binding(&tx, &previous)?.is_some() {
                return Ok(ResultState::Conflict);
            }
            super::coordination::flush_pending(&tx, &previous)?;
            let task = protocol::new_id();
            tx.execute(
                "INSERT INTO tasks (id, issue_id, category, status, repo_ids, required_capabilities,
                   pinned_runner_id, pinned_agent_id, previous_task_id, workspace_key, base_refs, requested_agent)
                 SELECT ?1, t.issue_id, t.category, 'awaiting_assignment', t.repo_ids, t.required_capabilities,
                   coalesce(a.runner_id, t.pinned_runner_id),
                   coalesce(json_extract(a.contract, '$.placement.agent.id'), t.pinned_agent_id),
                   t.id, t.workspace_key, t.base_refs, t.requested_agent
                 FROM tasks t LEFT JOIN runner_assignments a ON a.id = t.current_assignment AND a.state = 'accepted'
                 WHERE t.id = ?2",
                params![task, previous],
            )?;
            super::coordination::move_pending(&tx, &previous, &task)?;
            tx.execute("DELETE FROM issue_parks WHERE issue_id = ?1", [issue])?;
            super::runner::assign_task(&tx, &task)?;
            IssueDetail::Dispatched(Box::new(tx.query_row(
                &format!("{} WHERE t.id = ?1", work::DISPATCH_QUERY),
                [&task],
                work::read_dispatch,
            )?))
        } else {
            let Some(detail) = work::start_first_recorded(&tx, issue, chat)? else {
                return Ok(ResultState::Conflict);
            };
            detail
        };
        let receipt = serde_json::to_value(detail)?;
        if let Some(operation) = operation {
            save(&tx, actor, operation, issue, "start", &receipt)?;
        }
        super::board::commit(tx)?;
        Ok(ResultState::Found(receipt))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn queued_subtree_revokes_undelivered_assignment_and_does_not_replay_admission() -> Result<()> {
        let mut store = Store::initialize(Path::new(":memory:"))?;
        store.db.execute_batch(
            "INSERT INTO chat_messages (id, chat_id, text) VALUES ('source', 'main', 'Fixture');
             INSERT INTO router_inputs (seq, message_seq, chat_id) VALUES (1, 1, 'main');
             INSERT INTO router_attempts (seq, number, id, thread_id) VALUES (1, 1, 'attempt', 'thread');
             INSERT INTO issues (id, source_seq, title, objective, parent_issue_id) VALUES
                 ('root', 1, 'Root', '', NULL), ('child', 1, 'Child', '', 'root'),
                 ('neighbor', 1, 'Neighbor', '', NULL);
             INSERT INTO tasks (id, issue_id, category, status, repo_ids, required_capabilities) VALUES
                 ('root-task', 'root', 'issue-router', 'awaiting_assignment', '[]', '[]'),
                 ('child-task', 'child', 'implementation', 'reserved', '[]', '[]'),
                 ('neighbor-task', 'neighbor', 'issue-router', 'awaiting_assignment', '[]', '[]');
             INSERT INTO router_dispatches (operation_id, issue_id, task_id, attempt_id, call_id, request)
                 VALUES ('dispatch', 'root', 'root-task', 'attempt', 'call', '{}');
             INSERT INTO runners (id, host_id, profile, last_seen) VALUES ('runner', 'host', '{}', 0);
             INSERT INTO runner_assignments (id, task_id, runner_id, contract, state, expires_at)
                 VALUES ('child-claim', 'child-task', 'runner', '{}', 'reserved', 1);
             UPDATE tasks SET current_assignment = 'child-claim' WHERE id = 'child-task';",
        )?;
        assert!(matches!(
            store.return_to_backlog("root", Some("other"), "foreign")?,
            ResultState::Missing
        ));
        let ResultState::Found(park) = store.return_to_backlog("root", None, "park")? else {
            panic!("expected park")
        };
        assert!(
            parked(&store.db, "root")?
                && parked(&store.db, "child")?
                && !parked(&store.db, "neighbor")?
        );
        assert_eq!(
            store.db.query_row(
                "SELECT state FROM runner_assignments WHERE id = 'child-claim'",
                [],
                |r| r.get::<_, String>(0)
            )?,
            "revoked"
        );
        assert_eq!(
            store.db.query_row(
                "SELECT count(*) FROM tasks WHERE status = 'cancelled'",
                [],
                |r| r.get::<_, u32>(0)
            )?,
            2
        );
        assert!(matches!(
            store.start_issue("child", None, Some("child-start"))?,
            ResultState::Conflict
        ));
        assert!(matches!(
            store.start_issue("root", None, None)?,
            ResultState::Conflict
        ));
        assert!(matches!(
            store.start_issue("root", None, Some("park"))?,
            ResultState::Conflict
        ));
        let ResultState::Found(continued) = store.start_issue("root", None, Some("start"))? else {
            panic!("expected restart")
        };
        assert_eq!(continued["issue_id"], "root");
        assert_ne!(continued["task_id"], "root-task");
        assert!(!parked(&store.db, "root")? && parked(&store.db, "child")?);
        assert!(
            matches!(store.return_to_backlog("root", None, "park")?, ResultState::Found(value) if value == park)
        );
        assert!(!parked(&store.db, "root")?);
        store.return_to_backlog("root", None, "park-again")?;
        assert!(
            matches!(store.start_issue("root", None, Some("start"))?, ResultState::Found(value) if value == continued)
        );
        assert!(parked(&store.db, "root")?);
        Ok(())
    }
}
