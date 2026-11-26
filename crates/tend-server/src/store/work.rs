use super::Store;
use anyhow::Result;
use codex_app::ToolCall;
use protocol::{
    DispatchIssue, IssueDetail, IssueDispatch, RecordedIssue, RunObservation, TaskAssignment,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

pub(super) const DISPATCH_QUERY: &str =
    "SELECT i.id, t.id, coalesce(d.operation_id,
            (SELECT c.operation_id FROM tasks original JOIN child_dispatches c ON c.task_id = original.id
             WHERE original.issue_id = i.id LIMIT 1)),
            m.id, m.chat_id, t.category, t.status,
            i.title, i.objective, t.repo_ids, t.required_capabilities, d.request, a.contract,
            i.state,
            (SELECT run.observation FROM task_runs run
              WHERE run.task_id = t.id ORDER BY run.rowid DESC LIMIT 1),
            i.parent_issue_id, t.base_refs, t.requested_agent, n.number
       FROM tasks t
       JOIN issues i ON i.id = t.issue_id
       JOIN issue_short_numbers n ON n.issue_id = i.id
       LEFT JOIN router_dispatches d ON i.id = d.issue_id
       JOIN chat_messages m ON m.seq = i.source_seq
       LEFT JOIN runner_assignments a ON a.id = t.current_assignment";

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

pub fn migrate_backlog(db: &Connection) -> Result<()> {
    db.execute_batch(
        "ALTER TABLE router_dispatches RENAME TO old_router_dispatches;
         CREATE TABLE router_dispatches (
            operation_id TEXT PRIMARY KEY,
            issue_id TEXT NOT NULL UNIQUE REFERENCES issues(id),
            task_id TEXT UNIQUE REFERENCES tasks(id),
            attempt_id TEXT NOT NULL REFERENCES router_attempts(id),
            call_id TEXT NOT NULL,
            request TEXT NOT NULL,
            recorded INTEGER NOT NULL DEFAULT 0,
            UNIQUE (attempt_id, call_id)
         );
         INSERT INTO router_dispatches(operation_id, issue_id, task_id, attempt_id, call_id, request)
         SELECT operation_id, issue_id, task_id, attempt_id, call_id, request FROM old_router_dispatches;
         DROP TABLE old_router_dispatches;
         CREATE INDEX board_open_attentions ON attentions(issue_id)
          WHERE question_seq IS NOT NULL AND answer_input_id IS NULL;",
    )?;
    Ok(())
}

const RECORD_QUERY: &str =
    "SELECT i.id, d.operation_id, m.id, m.chat_id, i.state, d.request, n.number
       FROM router_dispatches d JOIN issues i ON i.id = d.issue_id
       JOIN issue_short_numbers n ON n.issue_id = i.id
       JOIN chat_messages m ON m.seq = i.source_seq";

impl Store {
    pub fn dispatch_issue(
        &mut self,
        attempt_id: &str,
        call: &ToolCall,
        work: &DispatchIssue,
        record_only: bool,
        dispatcher_name: Option<&str>,
    ) -> Result<Option<IssueDetail>> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let source = tx
            .query_row(
                "SELECT i.message_seq FROM router_attempts a
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
                "SELECT operation_id, request, recorded FROM router_dispatches
             WHERE attempt_id = ?1 AND call_id = ?2",
                params![attempt_id, call.call_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, bool>(2)?,
                    ))
                },
            )
            .optional()?;
        if let Some((operation_id, saved, recorded)) = prior {
            return if saved == request && recorded == record_only {
                Ok(Some(receipt(&tx, &operation_id)?))
            } else {
                Ok(None)
            };
        }

        let issue_id = protocol::new_id();
        let operation_id = protocol::new_id();
        tx.execute(
            "INSERT INTO issues (id, source_seq, title, objective) VALUES (?1, ?2, ?3, ?4)",
            params![issue_id, source, work.title, work.objective],
        )?;
        tx.execute(
            "INSERT INTO router_dispatches (operation_id, issue_id, attempt_id, call_id, request, recorded)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![operation_id, issue_id, attempt_id, call.call_id, request, record_only],
        )?;
        let (source_id, source_chat): (String, String) = tx.query_row(
            "SELECT id, chat_id FROM chat_messages WHERE seq = ?1",
            [source],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        super::issue_chat::dispatch(
            &tx,
            &operation_id,
            &issue_id,
            "router",
            &work.objective,
            Some(&source_id),
            dispatcher_name.filter(|_| source_chat == "main"),
        )?;
        if !record_only {
            start_first_task(&tx, &issue_id, &operation_id, work)?;
        }
        let result = receipt(&tx, &operation_id)?;
        super::board::commit(tx)?;
        Ok(Some(result))
    }

    #[cfg(test)]
    pub fn start_recorded_issue(
        &mut self,
        id: &str,
        chat: Option<&str>,
    ) -> Result<Option<IssueDetail>> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = start_first_recorded(&tx, id, chat)?;
        super::board::commit(tx)?;
        Ok(result)
    }

    pub fn issue(&self, id: &str) -> Result<Option<IssueDetail>> {
        let issue = self
            .db
            .query_row(
                &format!("{DISPATCH_QUERY} WHERE i.id = ?1 ORDER BY t.rowid DESC LIMIT 1"),
                [id],
                read_dispatch,
            )
            .optional()?;
        if let Some(mut issue) = issue {
            issue.artifacts = self.issue_artifacts(id)?;
            return Ok(Some(IssueDetail::Dispatched(Box::new(issue))));
        }
        Ok(self
            .db
            .query_row(
                &format!("{RECORD_QUERY} WHERE i.id = ?1 AND d.task_id IS NULL"),
                [id],
                read_record,
            )
            .optional()?
            .map(IssueDetail::Recorded))
    }

    pub fn read_dispatch(
        &self,
        attempt_id: &str,
        call: &ToolCall,
        operation_id: &str,
    ) -> Result<Option<IssueDetail>> {
        let authorized = self.db.query_row(
            "SELECT EXISTS (SELECT 1 FROM router_dispatches d JOIN issues i ON i.id = d.issue_id
             JOIN router_inputs r ON r.message_seq = i.source_seq
             JOIN router_attempts a ON a.seq = r.seq
             WHERE d.operation_id = ?1 AND a.id = ?2 AND a.thread_id = ?3 AND a.turn_id = ?4)",
            params![operation_id, attempt_id, call.thread_id, call.turn_id],
            |row| row.get::<_, bool>(0),
        )?;
        authorized
            .then(|| receipt(&self.db, operation_id))
            .transpose()
    }

    pub fn router_chat(&self, attempt: &str, call: &ToolCall) -> Result<Option<String>> {
        Ok(self
            .db
            .query_row(
                "SELECT i.chat_id FROM router_attempts a JOIN router_inputs i ON i.seq = a.seq
             WHERE a.id = ?1 AND a.thread_id = ?2 AND a.turn_id = ?3 AND i.outcome IS NULL",
                params![attempt, call.thread_id, call.turn_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn dispatches_for_input(&self, source: i64) -> Result<Vec<IssueDetail>> {
        let mut statement = self.db.prepare(
            "SELECT d.operation_id FROM router_dispatches d JOIN issues i ON i.id = d.issue_id
             JOIN router_attempts o ON o.id = d.attempt_id WHERE o.seq = ?1 ORDER BY i.rowid",
        )?;
        statement
            .query_map([source], |row| row.get::<_, String>(0))?
            .map(|id| receipt(&self.db, &id?))
            .collect()
    }
}

pub(super) fn start_first_recorded(
    db: &Connection,
    id: &str,
    chat: Option<&str>,
) -> Result<Option<IssueDetail>> {
    let record = db
        .query_row(
            "SELECT d.operation_id, d.task_id, d.request, i.state
             FROM router_dispatches d JOIN issues i ON i.id = d.issue_id
             JOIN chat_messages m ON m.seq = i.source_seq
             WHERE i.id = ?1 AND d.recorded = 1 AND (?2 IS NULL OR m.chat_id = ?2)",
            params![id, chat],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((operation, task, raw, state)) = record else {
        return Ok(None);
    };
    if task.is_none() {
        if state != "open" {
            return Ok(None);
        }
        start_first_task(db, id, &operation, &serde_json::from_str(&raw)?)?;
    }
    Ok(Some(receipt(db, &operation)?))
}

fn start_first_task(
    db: &Connection,
    issue: &str,
    operation: &str,
    work: &DispatchIssue,
) -> Result<()> {
    let task = protocol::new_id();
    db.execute(
        "INSERT INTO tasks (id, issue_id, category, status, repo_ids, required_capabilities, base_refs,
                            requested_agent, pinned_runner_id, pinned_agent_id)
         VALUES (?1, ?2, 'issue-router', 'awaiting_assignment', ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            task, issue,
            serde_json::to_string(&work.repo_ids)?,
            serde_json::to_string(&work.required_capabilities)?,
            serde_json::to_string(&work.base_refs)?,
            work.agent.as_ref().map(serde_json::to_string).transpose()?,
            work.agent.as_ref().and_then(|agent| agent.runner_id.as_ref()),
            work.agent.as_ref().map(|agent| &agent.id),
        ],
    )?;
    db.execute(
        "UPDATE router_dispatches SET task_id = ?2 WHERE operation_id = ?1",
        params![operation, task],
    )?;
    let issue = db.query_row(
        &format!("{DISPATCH_QUERY} WHERE t.id = ?1"),
        [&task],
        read_dispatch,
    )?;
    super::runner::assign(db, issue)
}

fn receipt(db: &Connection, operation: &str) -> Result<IssueDetail> {
    let issue = db
        .query_row(
            &format!("{DISPATCH_QUERY} WHERE t.id = d.task_id AND d.operation_id = ?1"),
            [operation],
            read_dispatch,
        )
        .optional()?;
    if let Some(issue) = issue {
        return Ok(IssueDetail::Dispatched(Box::new(issue)));
    }
    Ok(IssueDetail::Recorded(db.query_row(
        &format!("{RECORD_QUERY} WHERE d.operation_id = ?1"),
        [operation],
        read_record,
    )?))
}

fn read_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<RecordedIssue> {
    let work = serde_json::from_str(row.get_ref(5)?.as_str()?).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(RecordedIssue {
        issue_id: row.get(0)?,
        short_number: row.get(6)?,
        operation_id: row.get(1)?,
        source_message_id: row.get(2)?,
        chat_id: row.get(3)?,
        issue_state: row.get(4)?,
        work,
    })
}

pub(super) fn read_dispatch(row: &rusqlite::Row<'_>) -> rusqlite::Result<IssueDispatch> {
    let placement = row
        .get::<_, Option<String>>(12)?
        .map(|contract| serde_json::from_str::<TaskAssignment>(&contract))
        .transpose()
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                12,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?
        .map(|assignment| assignment.placement);
    let run = row
        .get::<_, Option<String>>(14)?
        .map(|value| serde_json::from_str::<RunObservation>(&value))
        .transpose()
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                14,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?;
    Ok(IssueDispatch {
        issue_id: row.get(0)?,
        short_number: row.get(18)?,
        task_id: row.get(1)?,
        operation_id: row.get(2)?,
        source_message_id: row.get(3)?,
        chat_id: row.get(4)?,
        category: row.get(5)?,
        status: row.get(6)?,
        placement,
        issue_state: row.get(13)?,
        parent_issue_id: row.get(15)?,
        run,
        artifacts: Vec::new(),
        work: DispatchIssue {
            title: row.get(7)?,
            objective: row.get(8)?,
            repo_ids: read_list(row, 9)?,
            required_capabilities: read_list(row, 10)?,
            agent: row
                .get::<_, Option<String>>(17)?
                .map(|value| serde_json::from_str(&value))
                .transpose()
                .map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        17,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?,
            base_refs: serde_json::from_str(row.get_ref(16)?.as_str()?).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    16,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?,
        },
    })
}

pub(super) fn read_list(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Vec<String>> {
    serde_json::from_str(row.get_ref(index)?.as_str()?).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}
