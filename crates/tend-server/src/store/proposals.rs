use super::{Store, attention};
use anyhow::Result;
use protocol::{
    ApprovalDecision, ApproveProposal, Artifact, AttentionRequest, IssueApprovalEvent,
    IssueProposal, ProposalRequest, ProposalStatus, ProposalVersion, WorkOperation,
};
use rusqlite::{Connection, OptionalExtension, params};

pub(super) fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE INDEX attentions_by_issue ON attentions(issue_id);
         CREATE TABLE proposal_versions (
             attention_id TEXT NOT NULL REFERENCES attentions(id),
             version INTEGER NOT NULL,
             operation_id TEXT NOT NULL UNIQUE REFERENCES runner_operations(id),
             report_id TEXT NOT NULL UNIQUE REFERENCES issue_reports(id),
             artifact_id TEXT NOT NULL REFERENCES artifacts(id),
             question TEXT NOT NULL,
             title TEXT NOT NULL,
             highlights TEXT NOT NULL,
             action TEXT NOT NULL,
             updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
             PRIMARY KEY(attention_id, version)
         );
         CREATE TABLE proposal_decisions (
             attention_id TEXT PRIMARY KEY,
             version INTEGER NOT NULL,
             input_id TEXT NOT NULL UNIQUE REFERENCES issue_inputs(id),
             decided_at INTEGER NOT NULL DEFAULT (unixepoch()),
             after_issue_seq INTEGER NOT NULL,
             FOREIGN KEY(attention_id, version) REFERENCES proposal_versions(attention_id, version)
         );
         CREATE TABLE issue_report_publications (
             message_id TEXT PRIMARY KEY REFERENCES chat_messages(id),
             report_id TEXT NOT NULL REFERENCES issue_reports(id)
         );",
    )?;
    Ok(())
}

/// Initial asks always retain the ordinary wait if the candidate cannot be bound.
/// Revisions never create a new wait or replace a decision already made.
pub(super) fn register(
    db: &Connection,
    op: &WorkOperation,
    issue: &str,
    task: &str,
    source: i64,
    parent: Option<&str>,
    request: &AttentionRequest,
    proposal: &ProposalRequest,
) -> Result<()> {
    let valid = parent.is_none()
        && proposal.valid()
        && db.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM issue_reports r JOIN runner_operations o ON o.id = r.id
                JOIN task_runs run ON run.id = json_extract(o.operation, '$.run_id')
                JOIN report_artifacts link ON link.report_id = r.id
                JOIN artifacts file ON file.id = link.artifact_id
                WHERE r.id = ?1 AND r.issue_id = ?2 AND run.task_id = ?3 AND file.id = ?4
                  AND (file.published = 1 OR file.issue_id = ?2) AND (lower(file.name) GLOB '*.md' OR lower(file.name) GLOB '*.markdown')
                  AND NOT EXISTS (SELECT 1 FROM proposal_versions WHERE report_id = r.id)
            )",
            params![proposal.report_id, issue, task, proposal.artifact_id],
            |row| row.get::<_, bool>(0),
        )?;
    let id = if let Some(id) = &proposal.replaces {
        let revisable = valid && db.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM attentions a JOIN issues i ON i.id = a.issue_id
                 WHERE a.id = ?1 AND a.issue_id = ?2 AND a.task_id = ?3 AND a.answer_input_id IS NULL
                 AND i.state = 'open'
                 AND (SELECT max(version) FROM proposal_versions WHERE attention_id = a.id) = ?4 - 1
                 AND NOT EXISTS (SELECT 1 FROM proposal_versions v
                     WHERE v.attention_id = a.id AND v.artifact_id = ?5)
             )",
            params![id, issue, task, proposal.version, proposal.artifact_id],
            |row| row.get::<_, bool>(0),
        )?;
        if !revisable {
            eprintln!(
                "Proposal revision {} did not match an open candidate",
                op.operation_id
            );
            return Ok(());
        }
        id
    } else {
        let fallback;
        let request = if valid {
            request
        } else {
            fallback = AttentionRequest {
                question: "Which report and version should we review before proceeding?".into(),
                context: format!(
                    "The proposed candidate could not be bound. The original request was: {}",
                    request.question
                ),
                ..request.clone()
            };
            &fallback
        };
        attention::register(db, &op.operation_id, issue, task, source, parent, request)?;
        if !valid {
            return Ok(());
        }
        &op.operation_id
    };
    db.execute(
        "INSERT INTO proposal_versions
         (attention_id, version, operation_id, report_id, artifact_id, question, title, highlights, action)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![id, proposal.version, op.operation_id, proposal.report_id, proposal.artifact_id,
                request.question, proposal.title, serde_json::to_string(&proposal.highlights)?, proposal.action],
    )?;
    Ok(())
}

const DECISION_COLUMNS: &str = "d.input_id, d.version, d.decided_at,
    CASE WHEN json_extract(run.observation, '$.turn_id') IS NOT NULL THEN run.id END,
    json_extract(run.observation, '$.turn_id') IS NULL
      AND (run.id IS NOT NULL OR t.status IN ('completed','failed','cancelled'))";
const DECISION_RUN: &str =
    "LEFT JOIN issue_inputs input ON input.id = d.input_id AND input.task_id = a.task_id
    LEFT JOIN task_runs run ON run.id = input.run_id AND run.task_id = a.task_id";
fn read_decision(
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> rusqlite::Result<Option<ApprovalDecision>> {
    let Some(id) = row.get(offset)? else {
        return Ok(None);
    };
    Ok(Some(ApprovalDecision {
        id,
        version: row.get(offset + 1)?,
        decided_at: row.get(offset + 2)?,
        continuation_run_id: row.get(offset + 3)?,
        continuation_blocked: row.get(offset + 4)?,
    }))
}

pub(super) fn status(db: &Connection, id: &str) -> Result<Option<ProposalStatus>> {
    use attention::LIVE_TARGET;
    Ok(db.query_row(
        &format!(
            "SELECT a.issue_id, v.version, v.question, v.action,
                ({LIVE_TARGET}) AND a.answer_input_id IS NULL AND file.available = 1, {DECISION_COLUMNS}
             FROM attentions a JOIN issues i ON i.id = a.issue_id JOIN tasks t ON t.id = a.task_id
             JOIN proposal_versions v ON v.attention_id = a.id JOIN artifacts file ON file.id = v.artifact_id
             LEFT JOIN proposal_decisions d ON d.attention_id = a.id
             {DECISION_RUN}
             WHERE a.id = ?1 ORDER BY v.version DESC LIMIT 1"
        ),
        [id],
        |row| {
            Ok(ProposalStatus {
                issue_id: row.get(0)?,
                version: row.get(1)?,
                question: row.get(2)?,
                action: row.get(3)?,
                can_approve: row.get(4)?,
                decision: read_decision(row, 5)?,
            })
        },
    ).optional()?)
}

pub(super) fn overview(db: &Connection, issue: &str) -> Result<Vec<IssueProposal>> {
    let mut facts = db.prepare(
        "SELECT a.id, q.id FROM attentions a LEFT JOIN chat_messages q ON q.seq = a.question_seq
         WHERE a.issue_id = ?1 AND EXISTS (SELECT 1 FROM proposal_versions WHERE attention_id = a.id)
         ORDER BY a.rowid DESC",
    )?;
    let mut versions = db.prepare(
        "SELECT v.version, v.report_id, v.title, v.highlights, v.updated_at, file.id, file.name, file.bytes, file.available,
            (SELECT p.message_id FROM issue_report_publications p JOIN chat_messages m ON m.id = p.message_id
             WHERE p.report_id = v.report_id ORDER BY m.seq DESC LIMIT 1)
         FROM proposal_versions v JOIN artifacts file ON file.id = v.artifact_id
         WHERE v.attention_id = ?1 ORDER BY v.version DESC",
    )?;
    let mut other_files = db.prepare(
        "SELECT a.id, a.name, a.bytes, a.available FROM report_artifacts link
         JOIN artifacts a ON a.id = link.artifact_id WHERE link.report_id = ?1 AND a.id != ?2 ORDER BY link.rowid",
    )?;
    let mut result = Vec::new();
    let mut rows = facts.query([issue])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let Some(status) = status(db, &id)? else {
            continue;
        };
        let mut history = Vec::new();
        let mut items = versions.query([&id])?;
        while let Some(item) = items.next()? {
            let report_id: String = item.get(1)?;
            let file_id: String = item.get(5)?;
            let other_artifacts = other_files
                .query_map(params![report_id, file_id], super::artifacts::artifact)?
                .collect::<rusqlite::Result<_>>()?;
            history.push(ProposalVersion {
                version: item.get(0)?,
                report_id,
                other_artifacts,
                message_id: item.get(9)?,
                title: item.get(2)?,
                highlights: serde_json::from_str(&item.get::<_, String>(3)?)?,
                updated_at: item.get(4)?,
                artifact: Artifact {
                    id: file_id,
                    name: item.get(6)?,
                    bytes: item.get(7)?,
                    available: item.get(8)?,
                },
            });
        }
        result.push(IssueProposal {
            attention_id: id,
            question_message_id: row.get(1)?,
            status,
            versions: history,
        });
    }
    Ok(result)
}

pub enum Approval {
    Recorded(ApprovalDecision),
    NotFound,
    Conflict,
    ArchivedChat,
}

impl Store {
    pub fn approve_proposal(
        &mut self,
        chat: &str,
        id: &str,
        request: &ApproveProposal,
    ) -> Result<Approval> {
        let version = request.version;
        let (tx, timing) = super::timing::begin(&mut self.db, "proposal_approve")?;
        let Some(attention) = attention::lookup(&tx, chat, id)? else {
            return Ok(Approval::NotFound);
        };
        let Some(status) = attention.status.proposal else {
            return Ok(Approval::NotFound);
        };
        if status.version != version {
            return Ok(Approval::Conflict);
        }
        if let Some(decision) = status.decision {
            return Ok(Approval::Recorded(decision));
        }
        let source_archived = Self::chat_kind(&tx, chat)? == Some(protocol::ChatKind::Archived);
        if !request.from_issue && source_archived {
            return Ok(Approval::ArchivedChat);
        }
        if !status.can_approve
            || request.from_issue && requires_chat(&tx, &attention.issue_id, source_archived)?
        {
            return Ok(Approval::Conflict);
        }
        let input = protocol::new_id();
        // Legacy Runners receive an ordinary wake, with explicit version and
        // exact immutable report reference in the existing answer text.
        let (report_id, artifact_id): (String, String) = tx.query_row(
            "SELECT report_id, artifact_id FROM proposal_versions WHERE attention_id = ?1 AND version = ?2",
            params![id, version],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let answer = format!(
            "The user approved proposal v{version}: {}. Report ID: {report_id}. Markdown artifact ID: {artifact_id}.",
            status.action
        );
        attention::answer_task(
            &tx,
            id,
            &attention.issue_id,
            &attention.task_id,
            &input,
            &answer,
            None,
        )?;
        tx.execute(
            "INSERT INTO proposal_decisions (attention_id, version, input_id, after_issue_seq)
             VALUES (?1, ?2, ?3, coalesce((SELECT max(seq) FROM issue_messages), 0))",
            params![id, version, input],
        )?;
        let decision = status_decision(&tx, id)?;
        super::board::refresh(&tx)?;
        super::timing::commit(tx, timing)?;
        Ok(Approval::Recorded(decision))
    }
}

fn status_decision(db: &Connection, id: &str) -> Result<ApprovalDecision> {
    status(db, id)?
        .and_then(|status| status.decision)
        .ok_or_else(|| anyhow::anyhow!("missing saved proposal decision"))
}

pub(super) fn requires_chat(db: &Connection, issue: &str, source_archived: bool) -> Result<bool> {
    // Only a writable original Chat can serve as the multi-decision chooser.
    // With an archived source, the Issue offers each exact proposal instead.
    if source_archived {
        return Ok(false);
    }
    Ok(db.query_row(
        "WITH RECURSIVE tree(id) AS (
             SELECT id FROM issues WHERE id = ?1
             UNION ALL SELECT i.id FROM issues i JOIN tree p ON i.parent_issue_id = p.id
         )
         SELECT count(*) > 1 FROM (
             SELECT a.id FROM tree JOIN attentions a ON a.issue_id = tree.id
             JOIN tasks t ON t.id = a.task_id JOIN issues i ON i.id = a.issue_id
             WHERE a.answer_input_id IS NULL AND i.state = 'open'
               AND t.status NOT IN ('completed','failed','cancelled')
             LIMIT 2
         )",
        [issue],
        |row| row.get(0),
    )?)
}

pub(super) fn issue_events(
    db: &Connection,
    issue: &str,
    after: i64,
    before: i64,
) -> Result<Vec<(i64, IssueApprovalEvent)>> {
    let mut statement = db.prepare(&format!(
        "WITH RECURSIVE tree(id) AS (
             SELECT id FROM issues WHERE id = ?1
             UNION ALL SELECT i.id FROM issues i JOIN tree p ON i.parent_issue_id = p.id
         )
         SELECT d.after_issue_seq, a.issue_id, v.title, {DECISION_COLUMNS}
         FROM tree JOIN attentions a ON a.issue_id = tree.id
         JOIN tasks t ON t.id = a.task_id
         JOIN proposal_decisions d ON d.attention_id = a.id
         JOIN proposal_versions v ON v.attention_id = a.id AND v.version = d.version
         {DECISION_RUN}
         WHERE d.after_issue_seq >= ?2 AND d.after_issue_seq < ?3
         ORDER BY d.after_issue_seq, d.rowid"
    ))?;
    Ok(statement
        .query_map(params![issue, after, before], |row| {
            Ok((
                row.get(0)?,
                IssueApprovalEvent {
                    decision: read_decision(row, 3)?.expect("joined decision"),
                    report_title: row.get(2)?,
                    issue_id: row.get(1)?,
                    before_message_id: None,
                },
            ))
        })?
        .collect::<rusqlite::Result<_>>()?)
}
