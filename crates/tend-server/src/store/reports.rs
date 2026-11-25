use anyhow::{Result, ensure};
use protocol::{
    Artifact, DeliverySource, IssueDeliveryReport, IssueOverviewArtifact, IssueOverviewChild,
    WorkOperation,
};
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
        "INSERT INTO issue_report_publications (message_id, report_id)
         SELECT ?1, report_id FROM router_inputs WHERE seq = ?2 AND report_id IS NOT NULL
         ON CONFLICT DO NOTHING",
        params![message, input],
    )?;
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

pub(super) fn delivery_reports(db: &Connection, issue: &str) -> Result<Vec<IssueDeliveryReport>> {
    // Older rows don't have an exact Router-input-to-answer key. An answer's
    // persisted attachment set must be covered by a completed report for this
    // Issue and original topic. The set may be a subset: attach() suppresses
    // repeat downloads in a topic. Never infer links for unattached replies.
    // Start from this Issue's reported files to avoid scanning other topics'
    // messages and attachments on every detail refresh.
    let mut statement = db.prepare(
        "WITH topic AS (
             SELECT i.id, source.seq, source.id AS source_id, source.chat_id
             FROM issues i JOIN chat_messages source ON source.seq = i.source_seq
             WHERE i.id = ?1 AND source.operation_id IS NOT NULL
         ), completed_reports AS MATERIALIZED (
             SELECT r.id FROM issue_reports r JOIN topic ON topic.id = r.issue_id
             JOIN router_inputs input ON input.report_id = r.id
             WHERE input.message_seq = topic.seq AND input.chat_id = topic.chat_id
               AND input.outcome = 'completed'
         ), submitted_messages AS (
             SELECT DISTINCT link.message_id
             FROM completed_reports r JOIN report_artifacts file ON file.report_id = r.id
             JOIN chat_artifacts link ON link.artifact_id = file.artifact_id
         )
         SELECT m.id, m.text, a.id, a.name, a.bytes, a.available, i.id, i.title, n.number, t.id
         FROM submitted_messages submitted JOIN chat_messages m ON m.id = submitted.message_id
         JOIN chat_artifacts c ON c.message_id = m.id
         JOIN artifacts a ON a.id = c.artifact_id
         JOIN issues i ON i.id = a.issue_id
         JOIN topic ON topic.seq = i.source_seq AND (i.id = topic.id OR i.parent_issue_id = topic.id)
           AND topic.chat_id = m.chat_id AND topic.source_id = m.reply_to
         JOIN tasks t ON t.id = a.task_id AND t.issue_id = i.id
         JOIN issue_short_numbers n ON n.issue_id = i.id
         WHERE a.published = 1 AND m.operation_id IS NULL
         AND NOT EXISTS (
             SELECT 1 FROM issue_report_publications p JOIN proposal_versions v ON v.report_id = p.report_id
             WHERE p.message_id = m.id
         )
         AND EXISTS (
             SELECT 1 FROM completed_reports r
             WHERE NOT EXISTS (
                 SELECT 1 FROM chat_artifacts c WHERE c.message_id = m.id
                   AND NOT EXISTS (
                       SELECT 1 FROM report_artifacts file
                       WHERE file.report_id = r.id AND file.artifact_id = c.artifact_id
                   )
             )
         )
         ORDER BY m.seq, c.rowid",
    )?;
    let mut rows = statement.query([issue])?;
    let mut reports: Vec<IssueDeliveryReport> = Vec::new();
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        if reports.last().is_none_or(|report| report.message_id != id) {
            reports.push(IssueDeliveryReport {
                message_id: id,
                text: row.get(1)?,
                artifacts: Vec::new(),
            });
        }
        reports
            .last_mut()
            .unwrap()
            .artifacts
            .push(IssueOverviewArtifact {
                artifact: Artifact {
                    id: row.get(2)?,
                    name: row.get(3)?,
                    bytes: row.get(4)?,
                    available: row.get(5)?,
                },
                issue_title: None,
                source: Some(DeliverySource {
                    issue: IssueOverviewChild {
                        id: row.get(6)?,
                        title: row.get(7)?,
                        short_number: row.get(8)?,
                    },
                    task_id: row.get(9)?,
                }),
            });
    }
    Ok(reports)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use codex_app::Answer;
    use std::path::Path;

    #[test]
    fn issue_details_only_promote_proven_reports_and_keep_historical_sources() -> Result<()> {
        let mut store = Store::initialize(Path::new(":memory:"))?;
        store.db.execute_batch(
            "INSERT INTO chat_messages (seq, id, chat_id, text, operation_id) VALUES
                 (1, 'request', 'main', 'Plan and design', 'send'),
                 (2, 'other-request', 'private', 'A different request', 'send-other');
             INSERT INTO issues (id, source_seq, title, objective, state, parent_issue_id) VALUES
                 ('parent', 1, 'Plan and design', 'Review originals', 'closed', NULL),
                 ('plan', 1, 'Prepare plan', 'Plan', 'closed', 'parent'),
                 ('design', 1, 'Prepare design', 'Design', 'closed', 'parent'),
                 ('sibling', 1, 'Another Issue in this topic', '', 'closed', NULL),
                 ('other-chat', 2, 'Private Issue', '', 'closed', NULL),
                 ('no-report', 1, 'No bound report', '', 'closed', NULL);
             INSERT INTO tasks (id, issue_id, category, status, repo_ids, required_capabilities)
                 SELECT id || '-task', id, 'implementation', 'completed', '[]', '[]' FROM issues;
             INSERT INTO runners (id, host_id, profile, last_seen) VALUES ('runner', 'host', '{}', 0);
             INSERT INTO runner_operations (id, runner_id, operation) VALUES
                 ('md', 'runner', '{}'), ('html', 'runner', '{}'), ('draft', 'runner', '{}'),
                 ('sibling-file', 'runner', '{}'), ('other-file', 'runner', '{}'),
                 ('report', 'runner', '{}'), ('repeat', 'runner', '{}'),
                 ('sibling-report', 'runner', '{}'), ('other-report', 'runner', '{}');
             INSERT INTO artifacts (id, issue_id, task_id, runner_id, name, bytes, published, available) VALUES
                 ('md', 'plan', 'plan-task', 'runner', 'plan.md', 10, 1, 1),
                 ('html', 'design', 'design-task', 'runner', 'design.html', 20, 1, 0),
                 ('draft', 'plan', 'plan-task', 'runner', 'plan.md', 30, 1, 1),
                 ('sibling-file', 'sibling', 'sibling-task', 'runner', 'plan.md', 10, 1, 1),
                 ('other-file', 'other-chat', 'other-chat-task', 'runner', 'plan.md', 10, 1, 1);
             INSERT INTO issue_reports VALUES
                 ('report', 'parent', 'Final files scoped to this Issue'),
                 ('repeat', 'parent', 'The same files can be mentioned again'),
                 ('sibling-report', 'sibling', 'Same topic, separate Issue'),
                 ('other-report', 'other-chat', 'Separate Chat');
             INSERT INTO report_artifacts VALUES
                 ('report', 'md'), ('report', 'html'), ('repeat', 'md'),
                 ('sibling-report', 'sibling-file'), ('other-report', 'other-file');
             INSERT INTO router_inputs (chat_id, message_seq, report_id) VALUES
                 ('main', 1, 'report'), ('main', 1, 'repeat'),
                 ('main', 1, 'sibling-report'), ('private', 2, 'other-report');
             INSERT INTO router_inputs (seq, chat_id, message_seq, outcome) VALUES
                 (5, 'main', 1, 'completed');
             INSERT INTO router_attempts (seq, number, id, thread_id) VALUES
                 (5, 1, 'dispatch-attempt', 'router-thread');
             INSERT INTO router_dispatches (operation_id, issue_id, task_id, attempt_id, call_id, request)
                 VALUES ('dispatched', 'parent', 'parent-task', 'dispatch-attempt', 'call', '{}');",
        )?;
        for (seq, item_id, text) in [
            (1, "report-item", "This is the published review message."),
            (
                2,
                "repeat-item",
                "This later reply has no attached original.",
            ),
            (3, "sibling-item", "Sibling report in same topic."),
            (4, "another-chat-item", "Report in another chat."),
        ] {
            store.finish(seq, &[Answer { item_id, text }], None)?;
        }
        // Defend the read boundary even if a reply or import contains a
        // mismatched topic or a mixed set of unproven attachments.
        store.db.execute_batch(
            "INSERT INTO chat_messages (id, chat_id, reply_to, text) VALUES
                 ('wrong-chat', 'private', 'request', 'Wrong Chat'),
                 ('mixed-message', 'main', 'request', 'Unproven attachment set');
             INSERT INTO chat_artifacts VALUES
                 ('wrong-chat', 'md'), ('mixed-message', 'md'), ('mixed-message', 'sibling-file');",
        )?;

        let Some(protocol::IssueDetail::Dispatched(direct)) = store.issue("parent")? else {
            panic!("expected the original task on the historical Issue");
        };
        assert!(direct.artifacts.is_empty());
        let view = store.issue_overview("parent")?.unwrap();
        assert!(matches!(view.stage, protocol::BoardStage::Done));
        assert_eq!(view.artifacts.len(), 3);
        assert_eq!(view.reports.len(), 1);
        let report = &view.reports[0];
        let published_id: String = store.db.query_row(
            "SELECT id FROM chat_messages WHERE native_item_id = 'report-item'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(report.message_id, published_id);
        assert_eq!(report.text, "This is the published review message.");
        assert_eq!(report.artifacts.len(), 2);
        let mut files = report
            .artifacts
            .iter()
            .map(|file| file.artifact.id.as_str())
            .collect::<Vec<_>>();
        files.sort_unstable();
        assert_eq!(files, ["html", "md"]);
        let original = report
            .artifacts
            .iter()
            .find(|file| file.artifact.id == "html")
            .unwrap();
        assert!(!original.artifact.available);
        let html = original.source.as_ref().unwrap();
        assert_eq!(html.task_id, "design-task");
        assert_eq!(html.issue.id, "design");
        assert_eq!(html.issue.short_number, Some(3));
        for issue in ["plan", "no-report"] {
            let empty = store.issue_overview(issue)?.unwrap();
            assert!(empty.reports.is_empty());
            assert!(empty.decisions.is_empty());
        }
        Ok(())
    }
}
