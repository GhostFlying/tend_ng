use anyhow::{Result, ensure};
use protocol::{Artifact, WorkOperation};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

pub fn migrate_queue(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE issue_reports (
             id TEXT PRIMARY KEY REFERENCES runner_operations(id),
             issue_id TEXT NOT NULL REFERENCES issues(id),
             summary TEXT NOT NULL
         );
         CREATE TABLE report_artifacts (
             report_id TEXT NOT NULL REFERENCES issue_reports(id),
             artifact_id TEXT NOT NULL REFERENCES artifacts(id),
             PRIMARY KEY (report_id, artifact_id)
         );
         CREATE TABLE chat_artifacts (
             message_id TEXT NOT NULL REFERENCES chat_messages(id),
             artifact_id TEXT NOT NULL REFERENCES artifacts(id),
             PRIMARY KEY (message_id, artifact_id)
         );
         ALTER TABLE router_dispatches RENAME TO old_router_dispatches;
         ALTER TABLE router_attempts RENAME TO old_router_attempts;
         ALTER TABLE router_inputs RENAME TO old_router_inputs;
         CREATE TABLE router_inputs (
             seq INTEGER PRIMARY KEY,
             chat_id TEXT NOT NULL,
             message_seq INTEGER NOT NULL REFERENCES chat_messages(seq),
             report_id TEXT UNIQUE REFERENCES issue_reports(id),
             outcome TEXT CHECK (outcome IN ('completed', 'failed')),
             error TEXT
         );
         CREATE UNIQUE INDEX router_user_input ON router_inputs(message_seq) WHERE report_id IS NULL;
         CREATE TABLE router_attempts (
             seq INTEGER NOT NULL REFERENCES router_inputs(seq),
             number INTEGER NOT NULL,
             id TEXT NOT NULL UNIQUE,
             thread_id TEXT NOT NULL,
             turn_id TEXT,
             process_pid INTEGER,
             process_start TEXT,
             PRIMARY KEY (seq, number)
         );
         CREATE TABLE router_dispatches (
             operation_id TEXT PRIMARY KEY,
             issue_id TEXT NOT NULL UNIQUE REFERENCES issues(id),
             task_id TEXT NOT NULL UNIQUE REFERENCES tasks(id),
             attempt_id TEXT NOT NULL REFERENCES router_attempts(id),
             call_id TEXT NOT NULL,
             request TEXT NOT NULL,
             UNIQUE (attempt_id, call_id)
         );
         INSERT INTO router_inputs (seq, chat_id, message_seq, outcome, error)
             SELECT seq, chat_id, seq, outcome, error FROM old_router_inputs;
         INSERT INTO router_attempts SELECT * FROM old_router_attempts;
         INSERT INTO router_dispatches SELECT * FROM old_router_dispatches;
         DROP TABLE old_router_dispatches;
         DROP TABLE old_router_attempts;
         DROP TABLE old_router_inputs;
         CREATE INDEX pending_inputs ON router_inputs(chat_id, seq) WHERE outcome IS NULL;
         CREATE INDEX settled_inputs ON router_inputs(chat_id, message_seq)
             WHERE outcome IS NOT NULL AND report_id IS NULL;",
    )?;
    Ok(())
}

pub fn enqueue(
    db: &Connection,
    operation: &WorkOperation,
    issue: &str,
    source: i64,
    summary: &str,
    files: &[String],
) -> Result<()> {
    db.execute(
        "INSERT INTO issue_reports (id, issue_id, summary) VALUES (?1, ?2, ?3)",
        params![operation.operation_id, issue, summary],
    )?;
    for id in files {
        ensure!(
            db.execute(
                "INSERT INTO report_artifacts (report_id, artifact_id)
             SELECT ?1, a.id FROM artifacts a JOIN issues i ON i.id = a.issue_id
             WHERE a.id = ?2 AND a.published = 1 AND (i.id = ?3 OR i.parent_issue_id = ?3)",
                params![operation.operation_id, id, issue],
            )? == 1,
            "Report file does not belong to the Issue tree"
        );
    }
    db.execute(
        "INSERT INTO router_inputs (chat_id, message_seq, report_id)
         SELECT chat_id, seq, ?2 FROM chat_messages WHERE seq = ?1",
        params![source, operation.operation_id],
    )?;
    Ok(())
}

pub fn context(
    db: &Connection,
    report: &str,
    original_request: &str,
    original_message_id: &str,
) -> Result<String> {
    let mut statement = db.prepare(
        "SELECT a.id, a.name, a.bytes FROM report_artifacts r
         JOIN artifacts a ON a.id = r.artifact_id WHERE r.report_id = ?1
         AND NOT EXISTS (
             SELECT 1 FROM chat_artifacts c JOIN chat_messages m ON m.id = c.message_id
             WHERE c.artifact_id = r.artifact_id AND m.reply_to = ?2
         ) ORDER BY r.rowid",
    )?;
    let files = statement
        .query_map([report, original_message_id], |row| {
            Ok(json!({
                "id": row.get::<_, String>(0)?,
                "name": row.get::<_, String>(1)?,
                "bytes": row.get::<_, u64>(2)?,
            }))
        })?
        .collect::<rusqlite::Result<Vec<Value>>>()?;
    let body = db.query_row(
        "SELECT issue_id, summary FROM issue_reports WHERE id = ?1",
        [report],
        |row| {
            Ok(json!({
                "issue_id": row.get::<_, String>(0)?,
                "summary": row.get::<_, String>(1)?,
                "original_message_id": original_message_id,
                "original_request": original_request,
                "report_id": report,
                "artifacts": files,
            }))
        },
    )?;
    Ok(format!(
        "Tend Issue report for the user's original topic.\n{body}"
    ))
}

pub fn attach(db: &Connection, message: &str, input: i64) -> Result<()> {
    db.execute(
        "INSERT INTO chat_artifacts (message_id, artifact_id)
         SELECT ?1, r.artifact_id FROM router_inputs i
         JOIN chat_messages source ON source.seq = i.message_seq
         JOIN report_artifacts r ON r.report_id = i.report_id WHERE i.seq = ?2
         AND NOT EXISTS (
             SELECT 1 FROM chat_artifacts c JOIN chat_messages m ON m.id = c.message_id
             WHERE c.artifact_id = r.artifact_id AND m.reply_to = source.id
         )",
        params![message, input],
    )?;
    Ok(())
}

pub fn chat_artifacts(
    db: &Connection,
    chat: &str,
    oldest: i64,
    before: i64,
) -> Result<std::collections::HashMap<i64, Vec<Artifact>>> {
    let mut statement = db.prepare(
        "SELECT m.seq, a.id, a.name, a.bytes, a.available FROM chat_messages m
         JOIN chat_artifacts c ON c.message_id = m.id JOIN artifacts a ON a.id = c.artifact_id
         WHERE m.chat_id = ?1 AND m.seq >= ?2 AND m.seq < ?3
         AND NOT EXISTS (
             SELECT 1 FROM chat_artifacts previous
             JOIN chat_messages first ON first.id = previous.message_id
             WHERE previous.artifact_id = c.artifact_id AND first.reply_to = m.reply_to AND first.seq < m.seq
         )
         ORDER BY m.seq, c.rowid",
    )?;
    let mut by_message = std::collections::HashMap::<_, Vec<_>>::new();
    for data in statement.query_map(params![chat, oldest, before], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            Artifact {
                id: row.get(1)?,
                name: row.get(2)?,
                bytes: row.get(3)?,
                available: row.get(4)?,
            },
        ))
    })? {
        let (seq, file) = data?;
        by_message.entry(seq).or_default().push(file);
    }
    Ok(by_message)
}
