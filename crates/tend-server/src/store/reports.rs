use anyhow::{Result, ensure};
use protocol::{
    Artifact, DeliverySource, IssueDeliveryReport, IssueDeliveryReportVersion,
    IssueOverviewArtifact, IssueOverviewChild, WorkOperation,
};
use rusqlite::{Connection, Row, params};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

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
             WHERE a.id = ?2 AND (i.id = ?3 OR (i.parent_issue_id = ?3 AND a.published = 1))",
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
    provenance: Option<&Value>,
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
    let mut body = db.query_row(
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
    super::label_handoff_origin(&mut body, provenance);
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

fn legacy_reports(db: &Connection, issue: &str) -> Result<Vec<(i64, IssueDeliveryReport)>> {
    // Older rows don't have an exact Router-input-to-answer key. An unmapped
    // answer's attachments must be covered by an unmapped, completed ordinary
    // report for this Issue and topic. Never infer links for unattached replies.
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
               AND NOT EXISTS (SELECT 1 FROM issue_report_publications p WHERE p.report_id = r.id)
               AND NOT EXISTS (SELECT 1 FROM proposal_versions v WHERE v.report_id = r.id)
               AND NOT EXISTS (
                   SELECT 1 FROM runner_operations
                   WHERE json_extract(operation, '$.action.kind') = 'ask_attention'
                     AND json_extract(operation, '$.action.proposal.report_id') = r.id
               )
         ), submitted_messages AS (
             SELECT DISTINCT link.message_id
             FROM completed_reports r JOIN report_artifacts file ON file.report_id = r.id
             JOIN chat_artifacts link ON link.artifact_id = file.artifact_id
         )
         SELECT m.id, m.text, a.id, a.name, a.bytes, a.available, i.id, i.title, n.number, t.id, m.seq
         FROM submitted_messages submitted JOIN chat_messages m ON m.id = submitted.message_id
         JOIN chat_artifacts c ON c.message_id = m.id
         JOIN artifacts a ON a.id = c.artifact_id
         JOIN issues i ON i.id = a.issue_id
         JOIN topic ON topic.seq = i.source_seq AND (i.id = topic.id OR i.parent_issue_id = topic.id)
           AND topic.chat_id = m.chat_id AND topic.source_id = m.reply_to
         JOIN tasks t ON t.id = a.task_id AND t.issue_id = i.id
         JOIN issue_short_numbers n ON n.issue_id = i.id
         WHERE a.published = 1 AND m.operation_id IS NULL
         AND NOT EXISTS (SELECT 1 FROM issue_report_publications p WHERE p.message_id = m.id)
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
    let mut reports: Vec<(i64, IssueDeliveryReport)> = Vec::new();
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        if reports
            .last()
            .is_none_or(|(_, report)| report.message_id != id)
        {
            reports.push((
                row.get(10)?,
                IssueDeliveryReport {
                    report_id: None,
                    message_id: id,
                    text: row.get(1)?,
                    artifacts: Vec::new(),
                    history: Vec::new(),
                },
            ));
        }
        reports
            .last_mut()
            .unwrap()
            .1
            .artifacts
            .push(delivery_file(row)?);
    }
    Ok(reports)
}

fn delivery_file(row: &Row<'_>) -> rusqlite::Result<IssueOverviewArtifact> {
    Ok(IssueOverviewArtifact {
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
    })
}

struct RevisionMember {
    seq: i64,
    previous: Option<String>,
    version: IssueDeliveryReportVersion,
}

pub(super) fn delivery_reports(db: &Connection, issue: &str) -> Result<Vec<IssueDeliveryReport>> {
    let mut reports = legacy_reports(db, issue)?;
    // Exact publications do not need attachments or an edge. Use the pinned
    // messages for revision members and the latest exact message otherwise.
    let mut statement = db.prepare(
        "SELECT m.id, m.text, a.id, a.name, a.bytes, a.available, i.id, i.title, n.number, a.task_id,
                m.seq, r.id, incoming.previous_report_id
         FROM issue_reports r
         LEFT JOIN report_revisions incoming ON incoming.report_id = r.id
         LEFT JOIN report_revisions outgoing ON outgoing.previous_report_id = r.id
         JOIN chat_messages m ON m.id = (
             SELECT p.message_id FROM issue_report_publications p
             JOIN chat_messages published ON published.id = p.message_id
             WHERE p.report_id = r.id
               AND p.message_id = COALESCE(incoming.message_id, outgoing.previous_message_id, p.message_id)
             ORDER BY published.seq DESC LIMIT 1
         )
         LEFT JOIN report_artifacts f ON f.report_id = r.id
         LEFT JOIN artifacts a ON a.id = f.artifact_id
         LEFT JOIN issues i ON i.id = a.issue_id
         LEFT JOIN issue_short_numbers n ON n.issue_id = i.id
         WHERE r.issue_id = ?1
           AND NOT EXISTS (SELECT 1 FROM proposal_versions v WHERE v.report_id = r.id)
           AND NOT EXISTS (
               SELECT 1 FROM runner_operations
               WHERE json_extract(operation, '$.action.kind') = 'ask_attention'
                 AND json_extract(operation, '$.action.proposal.report_id') = r.id
           )
         ORDER BY f.rowid",
    )?;
    let mut rows = statement.query([issue])?;
    let mut members = HashMap::new();
    while let Some(row) = rows.next()? {
        let id: String = row.get(11)?;
        let member = match members.entry(id.clone()) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => entry.insert(RevisionMember {
                seq: row.get(10)?,
                previous: row.get(12)?,
                version: IssueDeliveryReportVersion {
                    report_id: id,
                    message_id: row.get(0)?,
                    text: row.get(1)?,
                    artifacts: Vec::new(),
                },
            }),
        };
        if row.get_ref(2)?.data_type() != rusqlite::types::Type::Null {
            member.version.artifacts.push(delivery_file(row)?);
        }
    }

    // If a member later becomes a structured proposal candidate, exclude it
    // from the ordinary projection and split the chain there.
    let historical: HashSet<_> = members
        .values()
        .filter_map(|m| m.previous.clone())
        .collect();
    let heads: Vec<_> = members
        .keys()
        .filter(|id| !historical.contains(*id))
        .cloned()
        .collect();
    for head in heads {
        let current = members.remove(&head).unwrap();
        let mut history = Vec::new();
        let mut previous = current.previous;
        while let Some(member) = previous.and_then(|id| members.remove(&id)) {
            history.push(member.version);
            previous = member.previous;
        }
        let v = current.version;
        reports.push((
            current.seq,
            IssueDeliveryReport {
                report_id: Some(v.report_id),
                message_id: v.message_id,
                text: v.text,
                artifacts: v.artifacts,
                history,
            },
        ));
    }
    reports.sort_unstable_by_key(|(seq, _)| *seq);
    Ok(reports.into_iter().map(|(_, report)| report).collect())
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
    fn direct_report_waits_for_its_original_without_blocking_chat() -> Result<()> {
        let mut store = Store::initialize(Path::new(":memory:"))?;
        store.db.execute_batch(
            "INSERT INTO chat_messages(seq,id,chat_id,text) VALUES
                (1,'original','main','Deliver the file'), (2,'unrelated','main','Continue chatting');
             INSERT INTO runners(id,host_id,profile,last_seen) VALUES ('runner','host','{}',0);
             INSERT INTO issues(id,source_seq,title,objective) VALUES ('root',1,'Title','Delivery');
             INSERT INTO tasks(id,issue_id,category,status,repo_ids,required_capabilities)
               VALUES ('task','root','implementation','completed','[]','[]');
             INSERT INTO runner_operations(id,runner_id,operation) VALUES
                ('file','runner','{}'), ('report','runner','{}');
             INSERT INTO artifacts(id,issue_id,task_id,runner_id,name,bytes)
               VALUES ('file','root','task','runner','file.md',5);
             INSERT INTO router_inputs(chat_id,message_seq) VALUES ('main',2);",
        )?;
        enqueue(
            &store.db,
            &WorkOperation {
                operation_id: "report".into(),
                assignment_id: "assignment".into(),
                run_id: "run".into(),
                action: protocol::WorkAction::ReportIssue {
                    summary: "Done".into(),
                    artifact_ids: vec!["file".into()],
                },
            },
            "root",
            1,
            "Done",
            &["file".into()],
        )?;
        let ordinary = store.next_input("main")?.unwrap();
        assert_eq!(ordinary.message_id, "unrelated");
        store.finish(ordinary.seq, &[], None)?;
        assert!(store.next_input("main")?.is_none());
        assert!(store.pending_chats()?.is_empty());
        store.finish_artifact(Artifact {
            id: "file".into(),
            name: "file.md".into(),
            bytes: 5,
            available: true,
        })?;
        let delivery = store.next_input("main")?.unwrap();
        assert_eq!(delivery.report_id.as_deref(), Some("report"));
        assert_eq!(delivery.message_id, "original");
        Ok(())
    }

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
                 ('report', 'runner', '{}'), ('repeat', 'runner', '{}'), ('text', 'runner', '{}'),
                 ('legacy', 'runner', '{}'), ('unpublished', 'runner', '{}'),
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
                 ('text', 'parent', 'No files needed'),
                 ('legacy', 'parent', 'Published before exact mappings'),
                 ('unpublished', 'parent', 'Still pending'),
                 ('sibling-report', 'sibling', 'Same topic, separate Issue'),
                 ('other-report', 'other-chat', 'Separate Chat');
             INSERT INTO report_artifacts VALUES
                 ('report', 'md'), ('report', 'html'), ('repeat', 'md'),
                 ('legacy', 'draft'), ('unpublished', 'md'),
                 ('sibling-report', 'sibling-file'), ('other-report', 'other-file');
             INSERT INTO router_inputs (chat_id, message_seq, report_id) VALUES
                 ('main', 1, 'report'), ('main', 1, 'repeat'),
                 ('main', 1, 'sibling-report'), ('private', 2, 'other-report');
             INSERT INTO router_inputs (seq, chat_id, message_seq, outcome) VALUES
                 (5, 'main', 1, 'completed');
             INSERT INTO router_attempts (seq, number, id, thread_id) VALUES
                 (5, 1, 'dispatch-attempt', 'router-thread');
             INSERT INTO router_dispatches (operation_id, issue_id, task_id, attempt_id, call_id, request)
                 VALUES ('dispatched', 'parent', 'parent-task', 'dispatch-attempt', 'call', '{}');
             INSERT INTO router_inputs (chat_id, message_seq, report_id, outcome) VALUES
                 ('main', 1, 'legacy', 'completed'), ('main', 1, 'unpublished', NULL),
                 ('main', 1, 'text', NULL);
             INSERT INTO chat_messages (id, chat_id, reply_to, text) VALUES
                 ('legacy-reply', 'main', 'request', 'Original legacy report');
             INSERT INTO chat_artifacts VALUES ('legacy-reply', 'draft');",
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
            (8, "text-item", "Text-only publication."),
        ] {
            store.finish(seq, &[Answer { item_id, text }], None)?;
        }
        // Defend the read boundary even if a reply or import contains a
        // mismatched topic or a mixed set of unproven attachments.
        store.db.execute_batch(
            "INSERT INTO chat_messages (id, chat_id, reply_to, text) VALUES
                 ('wrong-chat', 'private', 'request', 'Wrong Chat'),
                 ('mixed-message', 'main', 'request', 'Unproven attachment set'),
                 ('ordinary', 'main', 'request', 'Ordinary progress'),
                 ('ordinary-with-file', 'main', 'request', 'A file used by an exactly mapped report');
             INSERT INTO chat_artifacts VALUES
                 ('wrong-chat', 'draft'), ('mixed-message', 'draft'), ('mixed-message', 'sibling-file'),
                 ('ordinary-with-file', 'md');",
        )?;

        let Some(protocol::IssueDetail::Dispatched(direct)) = store.issue("parent")? else {
            panic!("expected the original task on the historical Issue");
        };
        assert!(direct.artifacts.is_empty());
        let view = store.issue_overview("parent")?.unwrap();
        assert!(matches!(view.stage, protocol::BoardStage::Done));
        assert_eq!(view.artifacts.len(), 3);
        let [legacy, report, repeat, text] = view.reports.as_slice() else {
            panic!(
                "expected exactly four reports in Chat order: {:?}",
                view.reports
            );
        };
        assert_eq!(legacy.report_id, None);
        assert_eq!(legacy.message_id, "legacy-reply");
        assert_eq!(legacy.artifacts[0].artifact.id, "draft");
        assert_eq!(report.report_id.as_deref(), Some("report"));
        let published_id: String = store.db.query_row(
            "SELECT id FROM chat_messages WHERE native_item_id = 'report-item'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(report.message_id, published_id);
        assert_eq!(report.text, "This is the published review message.");
        assert_eq!(report.artifacts.len(), 2);
        let files = report
            .artifacts
            .iter()
            .map(|file| file.artifact.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(files, ["md", "html"]);
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
        assert_eq!(repeat.report_id.as_deref(), Some("repeat"));
        assert_eq!(repeat.artifacts[0].artifact.id, "md");
        assert_eq!(text.report_id.as_deref(), Some("text"));
        assert_eq!(text.text, "Text-only publication.");
        assert!(text.artifacts.is_empty());
        assert!(view.reports.iter().all(|report| report.history.is_empty()));
        for issue in ["plan", "no-report"] {
            let empty = store.issue_overview(issue)?.unwrap();
            assert!(empty.reports.is_empty());
            assert!(empty.decisions.is_empty());
        }
        Ok(())
    }

    #[test]
    fn issue_reports_keep_backfilled_versions_and_exclude_proposal_candidates() -> Result<()> {
        let mut store = Store::initialize(Path::new(":memory:"))?;
        store.db.execute_batch(
            "INSERT INTO chat_messages (seq, id, chat_id, text, operation_id) VALUES
                 (1, 'request', 'main', 'Review request', 'send');
             INSERT INTO issues (id, source_seq, title, objective, state) VALUES
                 ('issue', 1, 'Review', '', 'open');
             INSERT INTO tasks (id, issue_id, category, status, repo_ids, required_capabilities)
                 VALUES ('task', 'issue', 'implementation', 'waiting', '[]', '[]');
             INSERT INTO runners (id, host_id, profile, last_seen) VALUES ('runner', 'host', '{}', 0);
             INSERT INTO runner_operations (id, runner_id, operation) VALUES
                 ('old', 'runner', '{}'), ('middle', 'runner', '{}'), ('new', 'runner', '{}'),
                 ('candidate', 'runner', '{}'), ('attention', 'runner', '{}'), ('md', 'runner', '{}'),
                 ('declaration', 'runner', '{\"action\":{\"kind\":\"ask_attention\",\"proposal\":{\"report_id\":\"candidate\"}}}');
             INSERT INTO issue_reports VALUES
                 ('old', 'issue', 'Old report'), ('middle', 'issue', 'Middle report'),
                 ('new', 'issue', 'New report'), ('candidate', 'issue', 'Unbound proposal');
             INSERT INTO chat_messages (id, chat_id, reply_to, text) VALUES
                 ('old-reply', 'main', 'request', 'Old version'),
                 ('middle-reply', 'main', 'request', 'Middle version'),
                 ('new-reply', 'main', 'request', 'New version'),
                 ('candidate-reply', 'main', 'request', 'Declared but not bound');
             INSERT INTO issue_report_publications VALUES
                 ('middle-reply', 'middle'), ('new-reply', 'new'), ('candidate-reply', 'candidate');
             INSERT INTO artifacts (id, issue_id, task_id, runner_id, name, bytes, published, available)
                 VALUES ('md', 'issue', 'task', 'runner', 'proposal.md', 20, 1, 1);
             INSERT INTO report_artifacts VALUES ('middle', 'md');",
        )?;
        for (operation, old, new, message) in [
            ("first-edge", "old", "middle", Some("old-reply")),
            ("second-edge", "middle", "new", None),
        ] {
            let receipt = store.backfill_report_revision(&protocol::BackfillReportRevision {
                operation_id: operation.into(),
                old_report_id: old.into(),
                new_report_id: new.into(),
                old_message_id: message.map(str::to_owned),
                new_message_id: None,
            })?;
            assert!(receipt.created);
        }
        let view = store.issue_overview("issue")?.unwrap();
        let [head] = view.reports.as_slice() else {
            panic!("expected only the head of the chain: {:?}", view.reports);
        };
        assert_eq!(head.message_id, "new-reply");
        assert!(head.artifacts.is_empty());
        let [middle, old] = head.history.as_slice() else {
            panic!("expected both historical versions: {:?}", head.history);
        };
        assert_eq!(middle.message_id, "middle-reply");
        assert_eq!(middle.artifacts[0].artifact.id, "md");
        assert_eq!(old.message_id, "old-reply");
        assert!(old.artifacts.is_empty());

        store.db.execute_batch(
            "INSERT INTO chat_messages (seq, id, chat_id, reply_to, text) VALUES
                 (6, 'question', 'main', 'request', 'Approve the middle version?');
             INSERT INTO attentions (id, issue_id, task_id, source_seq, question_seq, request)
                 VALUES ('attention', 'issue', 'task', 1, 6, '{\"question\":\"Approve?\"}');
             INSERT INTO proposal_versions
                 (attention_id, version, operation_id, report_id, artifact_id, question, title, highlights, action)
                 VALUES ('attention', 1, 'attention', 'middle', 'md', 'Approve?', 'Proposal', '[]', 'Continue');",
        )?;
        let view = store.issue_overview("issue")?.unwrap();
        let [old, new] = view.reports.as_slice() else {
            panic!(
                "expected the proposal to split the ordinary chain: {:?}",
                view.reports
            );
        };
        assert_eq!(old.message_id, "old-reply");
        assert_eq!(new.message_id, "new-reply");
        assert!(old.history.is_empty() && new.history.is_empty());
        assert_eq!(view.proposals[0].versions[0].report_id, "middle");
        Ok(())
    }
}
