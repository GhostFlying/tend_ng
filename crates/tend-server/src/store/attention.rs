use super::{Store, coordination};
use anyhow::Result;
use codex_app::ToolCall;
use protocol::{
    Attention, AttentionRequest, AttentionState, AttentionStatus, IssueInput, IssueInputCause,
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
           AND a.question_seq IS NULL",
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

fn answer_task(
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
        WHEN a.answer_input_id IS NOT NULL THEN 2 WHEN a.question_seq IS NOT NULL THEN 1 ELSE 0 END
   FROM attentions a JOIN chat_messages origin ON origin.seq = a.source_seq
   JOIN router_inputs route ON route.attention_id = a.id
   LEFT JOIN chat_messages q ON q.seq = a.question_seq
   LEFT JOIN chat_messages answer ON answer.seq = a.answer_seq
   LEFT JOIN issue_inputs i ON i.id = a.answer_input_id LEFT JOIN task_runs r ON r.id = i.run_id";

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
    Ok(db
        .query_row(
            &format!("{VIEW} WHERE origin.chat_id = ?1 AND a.id = ?2"),
            params![chat, id],
            view,
        )
        .optional()?)
}

#[derive(Debug)]
struct UserInput {
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
                i.report_id IS NULL AND i.attention_id IS NULL AND i.external_batch IS NULL, m.text
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
    pub fn attentions(&self, chat: &str, open_only: bool) -> Result<Vec<Attention>> {
        let mut statement = self.db.prepare(&format!(
            "{VIEW} WHERE origin.chat_id = ?1 AND (NOT ?2 OR a.answer_input_id IS NULL)
                     ORDER BY a.rowid DESC LIMIT 50"
        ))?;
        Ok(statement
            .query_map(params![chat, open_only], view)?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn router_attentions(&self, attempt: &str, call: &ToolCall) -> Result<Vec<Attention>> {
        match router_input(&self.db, attempt, call)? {
            Some(input) => self.attentions(&input.chat, true),
            None => Ok(Vec::new()),
        }
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
        if existing.status.state != AttentionState::Pending {
            return Ok(None);
        }
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
        tx.commit()?;
        Ok(result)
    }

    pub fn answer_attention(
        &mut self,
        attempt: &str,
        call: &ToolCall,
        id: &str,
        answer: &str,
    ) -> Result<Option<Attention>> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
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
        if existing.status.state != AttentionState::AwaitingUser
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
        tx.commit()?;
        Ok(result)
    }
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
            WHEN a.answer_input_id IS NOT NULL THEN 2 ELSE 1 END
         FROM chat_messages q JOIN attentions a ON a.question_seq = q.seq
         LEFT JOIN issue_inputs i ON i.id = a.answer_input_id LEFT JOIN task_runs r ON r.id = i.run_id
         WHERE q.chat_id = ?1 AND q.seq >= ?2 AND q.seq < ?3",
    )?;
    Ok(query
        .query_map(params![chat, oldest, before], |row| {
            Ok((
                row.get(0)?,
                AttentionStatus {
                    id: row.get(1)?,
                    state: state(row.get(2)?),
                },
            ))
        })?
        .collect::<rusqlite::Result<_>>()?)
}

pub(super) fn quoted(db: &Connection, chat: &str, message: &str) -> Result<Option<Attention>> {
    Ok(db
        .query_row(
            &format!("{VIEW} WHERE origin.chat_id = ?1 AND q.id = ?2"),
            params![chat, message],
            view,
        )
        .optional()?)
}
