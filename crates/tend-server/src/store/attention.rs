use super::{Store, coordination};
use anyhow::Result;
use codex_app::ToolCall;
use protocol::{
    AnswerIssueAttention, Attention, AttentionRequest, AttentionState, AttentionStatus,
    IssueAttentionReceipt, IssueInput, IssueInputCause,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

pub fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE attentions (
            id TEXT PRIMARY KEY REFERENCES runner_operations(id),
            issue_id TEXT NOT NULL REFERENCES issues(id),
            task_id TEXT NOT NULL REFERENCES tasks(id),
            source_seq INTEGER NOT NULL REFERENCES chat_messages(seq),
            request TEXT NOT NULL,
            question_seq INTEGER UNIQUE REFERENCES chat_messages(seq),
            answer_seq INTEGER REFERENCES chat_messages(seq),
            answer_input_id TEXT UNIQUE REFERENCES issue_inputs(id),
            answer TEXT
         );
         CREATE INDEX attentions_by_source ON attentions(source_seq);
         ALTER TABLE router_inputs ADD COLUMN attention_id TEXT REFERENCES attentions(id);
         CREATE UNIQUE INDEX router_attention_input ON router_inputs(attention_id);
         DROP INDEX router_user_input;
         CREATE UNIQUE INDEX router_user_input ON router_inputs(message_seq)
           WHERE report_id IS NULL AND attention_id IS NULL;",
    )?;
    Ok(())
}

pub(super) fn migrate_issue_replies(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE issue_attention_replies (
            operation_id TEXT PRIMARY KEY,
            scope_issue_id TEXT NOT NULL REFERENCES issues(id),
            owner_issue_id TEXT NOT NULL REFERENCES issues(id),
            attention_id TEXT NOT NULL UNIQUE REFERENCES attentions(id),
            input_id TEXT NOT NULL UNIQUE REFERENCES issue_inputs(id)
         );
         CREATE INDEX issue_attention_reply_history ON issue_attention_replies(owner_issue_id);",
    )?;
    Ok(())
}

pub(super) fn migrate_report_dependencies(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS attention_report_dependencies (
            attention_id TEXT PRIMARY KEY REFERENCES attentions(id),
            report_id TEXT NOT NULL REFERENCES issue_reports(id)
         );",
    )?;
    Ok(())
}

// The exact linked report has reached the Chat. Questions with no dependency
// retain their existing behavior. Publication, not an upload receipt, is the gate.
pub(super) const REPORT_DELIVERED: &str = "NOT EXISTS (
    SELECT 1 FROM attention_report_dependencies dep WHERE dep.attention_id = a.id
    AND NOT EXISTS (
        SELECT 1 FROM issue_report_publications pub WHERE pub.report_id = dep.report_id
    )
)";

// The live-target guard used by structured approvals and direct Issue replies.
// Ordinary Chat replies keep their existing Router validation and semantics.
pub(super) const LIVE_TARGET: &str =
    "i.state = 'open' AND t.status IN ('accepted','waiting','running')
    AND t.current_assignment IS NOT NULL AND a.question_seq IS NOT NULL
    AND NOT EXISTS (SELECT 1 FROM task_controls c WHERE c.assignment_id = t.current_assignment
                    AND c.state = 'pending' AND json_extract(c.action, '$.kind') = 'stop')";

pub(super) const ORDINARY_ROUTED: &str =
    "NOT EXISTS (SELECT 1 FROM proposal_versions v WHERE v.attention_id = a.id)
     AND EXISTS (SELECT 1 FROM router_inputs route WHERE route.attention_id = a.id)";

pub fn register(
    db: &Connection,
    id: &str,
    issue: &str,
    task: &str,
    source: i64,
    parent: Option<&str>,
    request: &AttentionRequest,
) -> Result<()> {
    db.execute(
        "INSERT INTO attentions (id, issue_id, task_id, source_seq, request) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, issue, task, source, serde_json::to_string(request)?],
    )?;
    if let Some(parent) = parent {
        if let Some(target) = coordination::target_task(db, parent)? {
            coordination::enqueue(
                db,
                &target,
                IssueInput {
                    input_id: id.to_owned(),
                    child_issue_id: issue.to_owned(),
                    child_task_id: task.to_owned(),
                    summary: request.question.clone(),
                    cause: IssueInputCause::AttentionRequested {
                        attention_id: id.to_owned(),
                        request: request.clone(),
                    },
                },
            )?;
        }
    } else {
        enqueue_router(db, id)?;
    }
    Ok(())
}

fn enqueue_router(db: &Connection, id: &str) -> Result<()> {
    db.execute(
        "INSERT INTO router_inputs (chat_id, message_seq, attention_id)
         SELECT m.chat_id, a.source_seq, a.id FROM attentions a
         JOIN chat_messages m ON m.seq = a.source_seq
         WHERE a.id = ?1 AND a.answer_input_id IS NULL ON CONFLICT(attention_id) DO NOTHING",
        [id],
    )?;
    Ok(())
}

pub fn apply_parent(
    db: &Connection,
    actor: &str,
    id: &str,
    answer: Option<(&str, &str)>,
) -> Result<()> {
    let child = db
        .query_row(
            "SELECT a.issue_id, a.task_id FROM attentions a JOIN issues i ON i.id = a.issue_id
         WHERE a.id = ?1 AND i.parent_issue_id = ?2 AND a.answer_input_id IS NULL
           AND a.question_seq IS NULL
           AND NOT EXISTS (SELECT 1 FROM proposal_versions WHERE attention_id = a.id)",
            params![id, actor],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    if let Some((issue, task)) = child {
        if let Some((op, answer)) = answer {
            answer_task(db, id, &issue, &task, op, answer, None)?;
        } else {
            enqueue_router(db, id)?;
        }
    }
    Ok(())
}

pub(super) fn answer_task(
    db: &Connection,
    id: &str,
    issue: &str,
    task: &str,
    input_id: &str,
    answer: &str,
    source: Option<&UserInput>,
) -> Result<()> {
    coordination::enqueue(
        db,
        task,
        IssueInput {
            input_id: input_id.to_owned(),
            child_issue_id: issue.to_owned(),
            child_task_id: task.to_owned(),
            summary: answer.to_owned(),
            cause: IssueInputCause::AttentionAnswered {
                attention_id: id.to_owned(),
                answer: answer.to_owned(),
                user_message_id: source.map(|message| message.id.clone()),
                user_text: source.map(|message| message.text.clone()),
            },
        },
    )?;
    db.execute(
        "UPDATE attentions SET answer_input_id = ?2, answer_seq = ?3, answer = ?4 WHERE id = ?1",
        params![id, input_id, source.map(|message| message.seq), answer],
    )?;
    Ok(())
}

const VIEW: &str =
    "SELECT a.id, a.issue_id, a.task_id, a.request, origin.id, q.id, answer.id, a.answer,
   CASE WHEN json_extract(r.observation, '$.turn_id') IS NOT NULL THEN 3
        WHEN a.answer_input_id IS NOT NULL THEN 2 WHEN a.question_seq IS NOT NULL THEN 1 ELSE 0 END,
   CASE WHEN direct.input_id IS NOT NULL THEN a.answer END
   FROM attentions a JOIN chat_messages origin ON origin.seq = a.source_seq
   JOIN router_inputs route ON route.attention_id = a.id
   LEFT JOIN chat_messages q ON q.seq = a.question_seq
   LEFT JOIN chat_messages answer ON answer.seq = a.answer_seq
   LEFT JOIN issue_inputs i ON i.id = a.answer_input_id LEFT JOIN task_runs r ON r.id = i.run_id
   LEFT JOIN issue_attention_replies direct ON direct.input_id = a.answer_input_id";

fn state(code: i64) -> AttentionState {
    match code {
        3 => AttentionState::Delivered,
        2 => AttentionState::Answered,
        1 => AttentionState::AwaitingUser,
        _ => AttentionState::Pending,
    }
}

fn view(row: &rusqlite::Row<'_>) -> rusqlite::Result<Attention> {
    let encoded: String = row.get(3)?;
    Ok(Attention {
        status: AttentionStatus {
            id: row.get(0)?,
            state: state(row.get(8)?),
            proposal: None,
            issue_reply: row.get(9)?,
        },
        issue_id: row.get(1)?,
        task_id: row.get(2)?,
        request: serde_json::from_str(&encoded).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                3,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        source_message_id: row.get(4)?,
        question_message_id: row.get(5)?,
        user_message_id: row.get(6)?,
        answer: row.get(7)?,
    })
}

pub(super) fn lookup(db: &Connection, chat: &str, id: &str) -> Result<Option<Attention>> {
    let mut fact = db
        .query_row(
            &format!("{VIEW} WHERE origin.chat_id = ?1 AND a.id = ?2"),
            params![chat, id],
            view,
        )
        .optional()?;
    if let Some(fact) = &mut fact {
        fact.status.proposal = super::proposals::status(db, id)?;
    }
    Ok(fact)
}

#[derive(Debug)]
pub(super) struct UserInput {
    id: String,
    seq: i64,
    reply_to: Option<String>,
    text: String,
}

struct RouterInput {
    chat: String,
    user: Option<UserInput>,
}

fn router_input(db: &Connection, attempt: &str, call: &ToolCall) -> Result<Option<RouterInput>> {
    Ok(db
        .query_row(
            "SELECT i.chat_id, m.id, m.seq, m.reply_to,
                i.report_id IS NULL AND i.attention_id IS NULL AND i.external_batch IS NULL
                    AND NOT EXISTS(SELECT 1 FROM discussion_handoffs h
                        JOIN discussion_quick_handoffs q ON q.draft_id=h.draft_id WHERE h.message_id=m.id), m.text
         FROM router_attempts r JOIN router_inputs i ON i.seq = r.seq
         JOIN chat_messages m ON m.seq = i.message_seq
         WHERE r.id = ?1 AND r.thread_id = ?2 AND r.turn_id = ?3 AND i.outcome IS NULL",
            params![attempt, call.thread_id, call.turn_id],
            |row| {
                let user = if row.get::<_, bool>(4)? {
                    Some(UserInput {
                        id: row.get(1)?,
                        seq: row.get(2)?,
                        reply_to: row.get(3)?,
                        text: row.get(5)?,
                    })
                } else {
                    None
                };
                Ok(RouterInput {
                    chat: row.get(0)?,
                    user,
                })
            },
        )
        .optional()?)
}

impl Store {
    pub fn attention(&self, chat: &str, id: &str) -> Result<Option<Attention>> {
        lookup(&self.db, chat, id)
    }

    pub fn attentions(&self, chat: &str, open_only: bool) -> Result<Vec<Attention>> {
        let mut statement = self.db.prepare(&format!(
            "{VIEW} WHERE origin.chat_id = ?1 AND (NOT ?2 OR a.answer_input_id IS NULL)
                     ORDER BY a.rowid DESC LIMIT 50"
        ))?;
        let mut rows = statement
            .query_map(params![chat, open_only], view)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for fact in &mut rows {
            fact.status.proposal = super::proposals::status(&self.db, &fact.status.id)?;
        }
        Ok(rows)
    }

    pub fn router_attentions(&self, attempt: &str, call: &ToolCall) -> Result<Vec<Attention>> {
        match router_input(&self.db, attempt, call)? {
            Some(input) => self.attentions(&input.chat, true),
            None => Ok(Vec::new()),
        }
    }

    pub fn answer_issue_attention(
        &mut self,
        scope: &str,
        id: &str,
        request: &AnswerIssueAttention,
    ) -> Result<IssueReply> {
        let (tx, timing) = super::timing::begin(&mut self.db, "issue_attention_answer")?;
        let receipt = |answer| {
            IssueReply::Recorded(IssueAttentionReceipt {
                operation_id: request.operation_id.clone(),
                attention_id: id.to_owned(),
                answer,
            })
        };
        if let Some((saved_scope, saved_attention, answer)) = tx
            .query_row(
                "SELECT r.scope_issue_id, r.attention_id, a.answer
             FROM issue_attention_replies r JOIN attentions a ON a.id = r.attention_id
             WHERE r.operation_id = ?1",
                [&request.operation_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?
        {
            return Ok(
                if saved_scope == scope && saved_attention == id && answer == request.text {
                    receipt(answer)
                } else {
                    IssueReply::Conflict(
                        "operation ID already used for a different Attention reply",
                    )
                },
            );
        }
        let Some((issue, task, eligible)) = tx.query_row(
            &format!(
                "WITH RECURSIVE ancestors(id, parent_issue_id, source_seq) AS (
                    SELECT id, parent_issue_id, source_seq FROM issues
                    WHERE id = (SELECT issue_id FROM attentions WHERE id = ?2)
                    UNION ALL SELECT p.id, p.parent_issue_id, p.source_seq FROM issues p
                    JOIN ancestors child ON child.parent_issue_id = p.id
                 )
                 SELECT a.issue_id, a.task_id, ({LIVE_TARGET}) AND a.answer_input_id IS NULL
                 FROM attentions a JOIN issues i ON i.id = a.issue_id
                 JOIN tasks t ON t.id = a.task_id AND t.issue_id = i.id
                 JOIN chat_messages question ON question.seq = a.question_seq
                 JOIN chat_messages origin ON origin.seq = a.source_seq AND origin.chat_id = question.chat_id
                 JOIN ancestors scope ON scope.id = ?1
                 JOIN chat_messages source ON source.seq = scope.source_seq AND source.chat_id = origin.chat_id
                 WHERE a.id = ?2 AND ({ORDINARY_ROUTED})"
            ),
            params![scope, id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, bool>(2)?)),
        ).optional()? else {
            return Ok(IssueReply::NotFound);
        };
        if !eligible {
            return Ok(IssueReply::Conflict(
                "the Attention is already answered or its original Task can no longer receive a reply",
            ));
        }
        let input = protocol::new_id();
        answer_task(&tx, id, &issue, &task, &input, &request.text, None)?;
        tx.execute(
            "INSERT INTO issue_attention_replies (operation_id, scope_issue_id, owner_issue_id, attention_id, input_id)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![request.operation_id, scope, issue, id, input],
        )?;
        super::board::refresh(&tx)?;
        super::timing::commit(tx, timing)?;
        Ok(receipt(request.text.clone()))
    }

    pub fn publish_attention(
        &mut self,
        attempt: &str,
        call: &ToolCall,
        id: &str,
        text: &str,
    ) -> Result<Option<Attention>> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(input) = router_input(&tx, attempt, call)? else {
            return Ok(None);
        };
        let Some(existing) = lookup(&tx, &input.chat, id)? else {
            return Ok(None);
        };
        if existing.question_message_id.is_some() {
            return Ok(Some(existing));
        }
        if existing.status.state != AttentionState::Pending
            || !tx.query_row(
                &format!("SELECT ({REPORT_DELIVERED}) FROM attentions a WHERE a.id = ?1"),
                [id],
                |row| row.get::<_, bool>(0),
            )?
        {
            return Ok(None);
        }
        let text = existing
            .status
            .proposal
            .as_ref()
            .map_or(text, |proposal| proposal.question.as_str());
        tx.execute(
            "INSERT INTO chat_messages(id, chat_id, text, reply_to) VALUES (?1, ?2, ?3, ?4)",
            params![
                protocol::new_id(),
                input.chat,
                text,
                existing.source_message_id
            ],
        )?;
        tx.execute(
            "UPDATE attentions SET question_seq = ?2 WHERE id = ?1",
            params![id, tx.last_insert_rowid()],
        )?;
        let result = lookup(&tx, &input.chat, id)?;
        super::board::commit(tx)?;
        Ok(result)
    }

    pub fn answer_attention(
        &mut self,
        attempt: &str,
        call: &ToolCall,
        id: &str,
        answer: &str,
    ) -> Result<Option<Attention>> {
        let (tx, timing) = super::timing::begin(&mut self.db, "attention_answer")?;
        let Some(input) = router_input(&tx, attempt, call)? else {
            return Ok(None);
        };
        let Some(user) = input.user else {
            return Ok(None);
        };
        let Some(existing) = lookup(&tx, &input.chat, id)? else {
            return Ok(None);
        };
        if existing.user_message_id.as_deref() == Some(&user.id) {
            return Ok(Some(existing));
        }
        let posted: Option<i64> = tx.query_row(
            "SELECT question_seq FROM attentions WHERE id = ?1",
            [id],
            |row| row.get(0),
        )?;
        if existing.status.proposal.is_some()
            || existing.status.state != AttentionState::AwaitingUser
            || posted.is_none_or(|seq| seq >= user.seq)
            || user
                .reply_to
                .as_ref()
                .is_some_and(|message| Some(message) != existing.question_message_id.as_ref())
        {
            return Ok(None);
        }
        answer_task(
            &tx,
            id,
            &existing.issue_id,
            &existing.task_id,
            &protocol::new_id(),
            answer,
            Some(&user),
        )?;
        let result = lookup(&tx, &input.chat, id)?;
        super::board::refresh(&tx)?;
        super::timing::commit(tx, timing)?;
        Ok(result)
    }
}

pub enum IssueReply {
    Recorded(IssueAttentionReceipt),
    NotFound,
    Conflict(&'static str),
}

pub(super) fn on_page(
    db: &Connection,
    chat: &str,
    oldest: i64,
    before: i64,
) -> Result<std::collections::HashMap<i64, AttentionStatus>> {
    let mut query = db.prepare(
        "SELECT a.question_seq, a.id,
            CASE WHEN json_extract(r.observation, '$.turn_id') IS NOT NULL THEN 3
            WHEN a.answer_input_id IS NOT NULL THEN 2 ELSE 1 END,
            CASE WHEN direct.input_id IS NOT NULL THEN a.answer END
         FROM chat_messages q JOIN attentions a ON a.question_seq = q.seq
         LEFT JOIN issue_inputs i ON i.id = a.answer_input_id LEFT JOIN task_runs r ON r.id = i.run_id
         LEFT JOIN issue_attention_replies direct ON direct.input_id = a.answer_input_id
         WHERE q.chat_id = ?1 AND q.seq >= ?2 AND q.seq < ?3",
    )?;
    let mut statuses = query
        .query_map(params![chat, oldest, before], |row| {
            Ok((
                row.get(0)?,
                AttentionStatus {
                    id: row.get(1)?,
                    state: state(row.get(2)?),
                    proposal: None,
                    issue_reply: row.get(3)?,
                },
            ))
        })?
        .collect::<rusqlite::Result<std::collections::HashMap<_, _>>>()?;
    for status in statuses.values_mut() {
        status.proposal = super::proposals::status(db, &status.id)?;
    }
    Ok(statuses)
}

pub(super) fn quoted(db: &Connection, chat: &str, message: &str) -> Result<Option<Attention>> {
    let mut fact = db
        .query_row(
            &format!("{VIEW} WHERE origin.chat_id = ?1 AND q.id = ?2"),
            params![chat, message],
            view,
        )
        .optional()?;
    if let Some(fact) = &mut fact {
        fact.status.proposal = super::proposals::status(db, &fact.status.id)?;
    }
    Ok(fact)
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{RunObservation, RunOutcome, RunPhase};
    use std::path::Path;

    #[test]
    fn failed_original_run_cannot_spawn_a_successor_for_an_issue_reply() -> Result<()> {
        let mut store = Store::initialize(Path::new(":memory:"))?;
        let tx = store.db.transaction()?;
        tx.execute_batch(
            r#"INSERT INTO chat_messages (seq, id, chat_id, text) VALUES
                (1, 'source', 'main', 'Synthetic request'), (2, 'question', 'main', 'Continue?');
             INSERT INTO issues (id, source_seq, title, objective) VALUES
                ('owner', 1, 'Owner', ''), ('unrelated', 1, 'Other Issue', '');
             INSERT INTO tasks (id, issue_id, category, status, repo_ids, required_capabilities)
                VALUES ('original', 'owner', 'fixture', 'accepted', '[]', '[]');
             INSERT INTO runners (id, host_id, profile, last_seen) VALUES ('runner', 'fixture', '{}', 0);
             INSERT INTO runner_assignments (id, task_id, runner_id, contract, state, expires_at)
                VALUES ('assignment', 'original', 'runner',
                  '{"placement":{"assignment_id":"assignment","runner_id":"runner","host_id":"fixture",
                    "agent":{"id":"agent","category":"fixture","revision":"1"}},"task_id":"original",
                    "issue_id":"owner","source_message_id":"source","chat_id":"main",
                    "work":{"title":"Fixture","objective":"Test only"}}', 'accepted', 0);
             UPDATE tasks SET current_assignment = 'assignment' WHERE id = 'original';
             INSERT INTO runner_operations (id, runner_id, operation) VALUES ('attention', 'runner', '{}');"#,
        )?;
        register(
            &tx,
            "attention",
            "owner",
            "original",
            1,
            None,
            &AttentionRequest {
                question: "Continue?".into(),
                context: String::new(),
                options: vec![],
                priority: None,
                due_at: None,
            },
        )?;
        tx.execute("UPDATE attentions SET question_seq = 2", [])?;
        let mut run = RunObservation {
            event_id: "started".into(),
            assignment_id: "assignment".into(),
            task_id: "original".into(),
            run_id: "run".into(),
            thread_id: Some("thread".into()),
            turn_id: Some("turn".into()),
            input_ids: vec![],
            branches: vec![],
            started_at: None,
            phase: RunPhase::Started,
        };
        assert!(super::super::run::apply(&tx, "runner", &run)?);
        super::super::board::commit(tx)?;
        // A regular delivery row with no reply_to is normally a candidate
        // user decision. Its persisted quick-handoff origin must override that.
        let discussion = store.create_discussion(&protocol::CreateDiscussion {
            operation_id: "create-discussion".into(),
            title: None,
            origin: None,
        })?;
        let super::super::discussion::Change::Ok(chat) = discussion else {
            panic!("fixture")
        };
        let operation_id = store.quick_handoff(&chat, None)?.operation_id;
        store.request_quick_handoff(
            &chat,
            &protocol::DiscussionQuickRequest {
                operation_id: operation_id.clone(),
                retry_generation: None,
            },
        )?;
        let (input_seq, input_message): (i64, String) = store.db.query_row(
            "SELECT i.seq,m.id FROM router_inputs i JOIN chat_messages m ON m.seq=i.message_seq WHERE i.chat_id=?1",
            [&chat], |row| Ok((row.get(0)?,row.get(1)?)),
        )?;
        let quick = store.quick_input(&input_message)?.unwrap();
        store.finish_quick_handoff(input_seq, &quick, "The discussion draft said to continue.")?;
        store.db.execute(
            "INSERT INTO router_attempts (seq,number,id,thread_id,turn_id)
             SELECT i.seq,1,'quick-attempt','router-thread','router-turn' FROM router_inputs i
               JOIN chat_messages m ON m.seq=i.message_seq WHERE m.operation_id=?1",
            [&operation_id],
        )?;
        let call = ToolCall {
            tool: "tend_answer_attention".into(),
            namespace: None,
            thread_id: "router-thread".into(),
            turn_id: "router-turn".into(),
            call_id: "generated-decision".into(),
            arguments: serde_json::json!({}),
        };
        assert!(
            store
                .answer_attention("quick-attempt", &call, "attention", "continue")?
                .is_none()
        );
        let unanswered = store.attention("main", "attention")?.unwrap();
        assert_eq!(unanswered.status.state, AttentionState::AwaitingUser);
        assert!(unanswered.user_message_id.is_none());

        let request = AnswerIssueAttention {
            operation_id: "reply".into(),
            text: "  keep this text\n exactly  ".into(),
        };
        assert!(matches!(
            store.answer_issue_attention("unrelated", "attention", &request)?,
            IssueReply::NotFound
        ));
        assert!(matches!(
            store.answer_issue_attention("owner", "attention", &request)?,
            IssueReply::Recorded(_)
        ));
        // The already-started Run fails without consuming the newly accepted input.
        run.event_id = "failed".into();
        run.phase = RunPhase::Stopped {
            outcome: RunOutcome::Failed,
            error: Some("Synthetic failure before takeover".into()),
            waiting_for_children: false,
            waiting_for_attention: false,
            waiting_for_external: false,
            queued_input: false,
        };
        let tx = store.db.transaction()?;
        assert!(super::super::run::apply(&tx, "runner", &run)?);
        super::super::board::commit(tx)?;
        let tasks: i64 = store
            .db
            .query_row("SELECT count(*) FROM tasks", [], |r| r.get(0))?;
        assert_eq!(tasks, 1);
        assert_eq!(
            store.attention("main", "attention")?.unwrap().status.state,
            AttentionState::Answered
        );

        // Separate follow-up work can proceed without picking up the reply.
        let successor = coordination::target_task(&store.db, "owner")?.unwrap();
        assert_ne!(successor, "original");
        let input: (String, Option<String>) = store.db.query_row(
            "SELECT task_id, run_id FROM issue_inputs WHERE id = (SELECT input_id FROM issue_attention_replies)",
            [], |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        assert_eq!(input, ("original".to_owned(), None));
        Ok(())
    }
}
