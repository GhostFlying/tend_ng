use super::Store;
use anyhow::Result;
use codex_app::ToolCall;
use protocol::{DispatchIssue, IssueDispatch};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

const DISPATCH_QUERY: &str =
    "SELECT i.id, t.id, d.operation_id, m.id, m.chat_id, t.category, t.status,
            i.title, i.objective, t.repo_ids, t.required_capabilities, d.request
       FROM router_dispatches d
       JOIN issues i ON i.id = d.issue_id
       JOIN tasks t ON t.id = d.task_id
       JOIN chat_messages m ON m.seq = i.source_seq";

pub fn create_tables(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE issues (
            id TEXT PRIMARY KEY,
            source_seq INTEGER NOT NULL REFERENCES chat_messages(seq),
            title TEXT NOT NULL,
            objective TEXT NOT NULL
         );
         CREATE INDEX issues_by_source ON issues(source_seq);
         CREATE TABLE tasks (
            id TEXT PRIMARY KEY,
            issue_id TEXT NOT NULL REFERENCES issues(id),
            category TEXT NOT NULL,
            status TEXT NOT NULL,
            repo_ids TEXT NOT NULL,
            required_capabilities TEXT NOT NULL
         );
         CREATE INDEX tasks_by_status ON tasks(status, id);
         CREATE TABLE router_dispatches (
            operation_id TEXT PRIMARY KEY,
            issue_id TEXT NOT NULL UNIQUE REFERENCES issues(id),
            task_id TEXT NOT NULL UNIQUE REFERENCES tasks(id),
            attempt_id TEXT NOT NULL REFERENCES router_attempts(id),
            call_id TEXT NOT NULL,
            request TEXT NOT NULL,
            UNIQUE (attempt_id, call_id)
         );",
    )?;
    Ok(())
}

impl Store {
    pub fn dispatch_issue(
        &mut self,
        attempt_id: &str,
        call: &ToolCall,
        work: &DispatchIssue,
    ) -> Result<Option<IssueDispatch>> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let source = tx
            .query_row(
                "SELECT a.seq FROM router_attempts a
             JOIN router_inputs i ON i.seq = a.seq
             WHERE a.id = ?1 AND a.thread_id = ?2 AND a.turn_id = ?3
               AND i.outcome IS NULL",
                params![attempt_id, call.thread_id, call.turn_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        let Some(source) = source else {
            return Ok(None);
        };
        let request = serde_json::to_string(work)?;
        let prior = tx
            .query_row(
                &format!("{DISPATCH_QUERY} WHERE d.attempt_id = ?1 AND d.call_id = ?2"),
                params![attempt_id, call.call_id],
                |row| Ok((row.get::<_, String>(11)?, read_dispatch(row)?)),
            )
            .optional()?;
        if let Some((original_request, prior)) = prior {
            return Ok((original_request == request).then_some(prior));
        }

        let issue_id = protocol::new_id();
        let task_id = protocol::new_id();
        let operation_id = protocol::new_id();
        tx.execute(
            "INSERT INTO issues (id, source_seq, title, objective) VALUES (?1, ?2, ?3, ?4)",
            params![issue_id, source, work.title, work.objective],
        )?;
        tx.execute(
            "INSERT INTO tasks (id, issue_id, category, status, repo_ids, required_capabilities)
                VALUES (?1, ?2, 'issue-router', 'awaiting_assignment', ?3, ?4)",
            params![
                task_id,
                issue_id,
                serde_json::to_string(&work.repo_ids)?,
                serde_json::to_string(&work.required_capabilities)?
            ],
        )?;
        tx.execute(
            "INSERT INTO router_dispatches (operation_id, issue_id, task_id, attempt_id, call_id, request)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![operation_id, issue_id, task_id, attempt_id, call.call_id, request],
        )?;
        let receipt = tx.query_row(
            &format!("{DISPATCH_QUERY} WHERE d.operation_id = ?1"),
            [&operation_id],
            read_dispatch,
        )?;
        tx.commit()?;
        Ok(Some(receipt))
    }

    pub fn issue(&self, id: &str) -> Result<Option<IssueDispatch>> {
        Ok(self
            .db
            .query_row(
                &format!("{DISPATCH_QUERY} WHERE i.id = ?1"),
                [id],
                read_dispatch,
            )
            .optional()?)
    }

    pub fn dispatch_context(&self, source: i64) -> Result<Option<String>> {
        let mut statement = self.db.prepare(&format!(
            "{DISPATCH_QUERY} WHERE i.source_seq = ?1 ORDER BY i.rowid"
        ))?;
        let dispatches = statement
            .query_map([source], read_dispatch)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if dispatches.is_empty() {
            Ok(None)
        } else {
            Ok(Some(format!(
                "The following Tend dispatches have already been saved for the current request. Use their receipts and continue any remaining work.\n{}",
                serde_json::to_string(&dispatches)?
            )))
        }
    }
}

fn read_dispatch(row: &rusqlite::Row<'_>) -> rusqlite::Result<IssueDispatch> {
    Ok(IssueDispatch {
        issue_id: row.get(0)?,
        task_id: row.get(1)?,
        operation_id: row.get(2)?,
        source_message_id: row.get(3)?,
        chat_id: row.get(4)?,
        category: row.get(5)?,
        status: row.get(6)?,
        work: DispatchIssue {
            title: row.get(7)?,
            objective: row.get(8)?,
            repo_ids: read_list(row, 9)?,
            required_capabilities: read_list(row, 10)?,
        },
    })
}

fn read_list(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Vec<String>> {
    serde_json::from_str(row.get_ref(index)?.as_str()?).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}
