mod quick;

use super::{Store, timing};
use anyhow::{Result, ensure};
use protocol::{
    CreateDiscussion, DiscussionDraft, DiscussionOrigin, DiscussionReference, DiscussionSend,
    DiscussionSource, Receipt, SendMessage,
};
pub(super) use quick::{HandoffMode, quick_status, request_quick_in_tx};
pub use quick::{HandoffSource, QuickInput};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::collections::HashSet;

const UNTITLED_DISCUSSION: &str = "未命名讨论";

pub fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE discussions (
            chat_id TEXT PRIMARY KEY, operation_id TEXT NOT NULL UNIQUE, creation TEXT NOT NULL,
            title TEXT NOT NULL, origin TEXT, quoted_text TEXT,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()), updated_at INTEGER NOT NULL DEFAULT (unixepoch())
         );
         CREATE INDEX discussions_by_activity ON discussions(updated_at DESC, chat_id);
         CREATE TABLE discussion_references (
            chat_id TEXT NOT NULL REFERENCES discussions(chat_id), id TEXT NOT NULL, reference TEXT NOT NULL,
            PRIMARY KEY(chat_id, id)
         );
         CREATE TABLE discussion_drafts (
            id TEXT PRIMARY KEY, chat_id TEXT NOT NULL REFERENCES discussions(chat_id),
            message_id TEXT NOT NULL UNIQUE REFERENCES chat_messages(id), draft TEXT
         );
         CREATE INDEX discussion_drafts_by_chat ON discussion_drafts(chat_id);
         CREATE TABLE discussion_handoffs (
            operation_id TEXT PRIMARY KEY, chat_id TEXT NOT NULL REFERENCES discussions(chat_id),
            draft_id TEXT NOT NULL UNIQUE REFERENCES discussion_drafts(id), intent TEXT NOT NULL,
            message_id TEXT NOT NULL UNIQUE REFERENCES chat_messages(id)
         );
         CREATE INDEX discussion_handoffs_by_chat ON discussion_handoffs(chat_id);",
    )?;
    Ok(())
}

pub fn migrate_archive(db: &Connection) -> Result<()> {
    db.execute_batch(
        "ALTER TABLE discussions ADD COLUMN archived INTEGER NOT NULL DEFAULT 0 CHECK (archived IN (0, 1));
         DROP INDEX discussions_by_activity;
         CREATE INDEX discussions_by_activity ON discussions(archived, updated_at);",
    )?;
    // In v32 only accepting an ordinary message or renaming could move
    // updated_at. When no such message exists, a changed timestamp proves a
    // rename even to the old default. A draft's internal input did not touch it.
    // Message activity and same-second renames cannot reliably prove intent.
    db.execute(
        "UPDATE discussions AS d SET title=?1 WHERE d.title='新讨论'
           AND json_extract(d.creation, '$.title') IS NULL
           AND (d.updated_at=d.created_at OR EXISTS (
               SELECT 1 FROM chat_messages m
                WHERE m.chat_id=d.chat_id AND m.operation_id IS NOT NULL
                  AND NOT EXISTS (
                      SELECT 1 FROM discussion_drafts draft WHERE draft.message_id=m.id
                  )
           ))",
        [UNTITLED_DISCUSSION],
    )?;
    Ok(())
}

/// A real prompt has a user operation ID and is not an internal draft input.
const REAL_PROMPT: &str = "m.operation_id IS NOT NULL
    AND NOT EXISTS(SELECT 1 FROM discussion_drafts d WHERE d.message_id=m.id)";

pub fn migrate_titles_and_handoffs(db: &Connection) -> Result<()> {
    db.execute_batch(
        "ALTER TABLE discussions ADD COLUMN title_source TEXT NOT NULL DEFAULT 'manual'
           CHECK (title_source IN ('default','prompt','manual'));",
    )?;
    quick::migrate(db)?;
    // Legacy builds recorded the create intent and last activity but not
    // individual renames. Preserve all provably manual titles, including
    // renames after the latest ordinary prompt, before backfilling defaults.
    // The legacy message insert and activity update could straddle one second.
    let mut candidates = db.prepare(&format!(
        "SELECT d.chat_id,
           (SELECT m.text FROM chat_messages m WHERE m.chat_id=d.chat_id AND {REAL_PROMPT}
            ORDER BY m.seq LIMIT 1)
           FROM discussions d WHERE d.chat_id>?2 AND d.title=?1 AND json_extract(d.creation, '$.title') IS NULL
             AND d.updated_at<=COALESCE(
               (SELECT m.created_at FROM chat_messages m WHERE m.chat_id=d.chat_id AND {REAL_PROMPT}
                ORDER BY m.seq DESC LIMIT 1) + 1, d.created_at)
           ORDER BY d.chat_id LIMIT 64"
    ))?;
    let mut after = String::new();
    loop {
        let rows = candidates
            .query_map(params![UNTITLED_DISCUSSION, after], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if rows.is_empty() {
            break;
        }
        for (chat, prompt) in rows {
            let (title, source) = match prompt {
                Some(prompt) => (title_from_prompt(&prompt), "prompt"),
                None => (UNTITLED_DISCUSSION.to_owned(), "default"),
            };
            db.execute(
                "UPDATE discussions SET title=?2,title_source=?3 WHERE chat_id=?1",
                params![chat, title, source],
            )?;
            after = chat;
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn remove_quick_handoff_and_title_schema(db: &Connection) -> Result<()> {
    db.execute_batch(
        "DROP TABLE discussion_quick_handoffs;
         ALTER TABLE discussion_drafts DROP COLUMN manual;
         ALTER TABLE discussions DROP COLUMN title_source;",
    )?;
    Ok(())
}

fn title_from_prompt(text: &str) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        UNTITLED_DISCUSSION.to_owned()
    } else {
        normalized
    }
}

pub(super) fn accept_title(db: &Connection, chat: &str, prompt: &str) -> Result<()> {
    // Avoid normalizing every message; only a still-default title can change.
    if db.execute(
        "UPDATE discussions SET updated_at=unixepoch() WHERE chat_id=?1 AND title_source='default'",
        [chat],
    )? == 1
    {
        db.execute(
            "UPDATE discussions SET title=?2,title_source='prompt' WHERE chat_id=?1",
            params![chat, title_from_prompt(prompt)],
        )?;
    } else {
        db.execute(
            "UPDATE discussions SET updated_at=unixepoch() WHERE chat_id=?1",
            [chat],
        )?;
    }
    Ok(())
}

pub fn valid_title(title: &str) -> bool {
    !title.trim().is_empty() && title.chars().count() <= 120
}

pub fn valid_draft(draft: &DiscussionDraft) -> bool {
    !draft.goal.trim().is_empty()
        && [
            &draft.goal,
            &draft.decisions,
            &draft.pending_items,
            &draft.constraints,
            &draft.authorization_end,
        ]
        .into_iter()
        .all(|text| text.len() <= 8192)
}

#[derive(Debug)]
pub enum Change<T> {
    Ok(T),
    Missing,
    Conflict,
    Invalid,
}

pub struct WorkspaceChoice {
    pub task: String,
    pub issue: String,
    pub title: String,
    pub number: i64,
    pub repos: Vec<String>,
    pub runner: Option<String>,
    pub host: Option<String>,
    pub url: Option<String>,
    pub assignment: Option<String>,
}

fn read_choice(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkspaceChoice> {
    let repos: String = row.get(4)?;
    Ok(WorkspaceChoice {
        task: row.get(0)?,
        issue: row.get(1)?,
        title: row.get(2)?,
        number: row.get(3)?,
        repos: serde_json::from_str(&repos).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        runner: row.get(5)?,
        host: row.get(6)?,
        url: row.get(7)?,
        assignment: row.get(8)?,
    })
}

const WORKSPACE_CHOICES: &str = "SELECT t.id, i.id, i.title, n.number, t.repo_ids,
    r.id, r.host_id, json_extract(r.profile, '$.inspect_url'), a.id
    FROM tasks t JOIN issues i ON i.id=t.issue_id JOIN issue_short_numbers n ON n.issue_id=i.id
    LEFT JOIN runner_assignments a ON a.id=t.current_assignment AND a.state='accepted'
    LEFT JOIN runners r ON r.id=a.runner_id";

impl Store {
    pub fn discussion_thread_id(&self, chat: &str, revision: i64) -> Result<Option<String>> {
        // Older Discussion bindings stored an attachment-scope hash in model.
        // The binding is still valid; current permissions are applied on resume.
        Ok(self
            .db
            .query_row(
                "SELECT thread_id FROM router_threads WHERE chat_id = ?1 AND tool_revision = ?2",
                params![chat, revision],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn unbound_discussion_attempt_threads(&self, chat: &str) -> Result<Vec<String>> {
        // Do not bypass a binding with a different tool revision. Older releases
        // removed the binding on retry exhaustion but retained the attempts.
        let bound: bool = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM router_threads WHERE chat_id = ?1)",
            [chat],
            |row| row.get(0),
        )?;
        if bound {
            return Ok(Vec::new());
        }

        // Keep both branches on their per-chat partial indexes. In particular,
        // an old retry can have a newer ID without a native rollout of its own.
        let mut query = self.db.prepare(
            "SELECT a.thread_id, i.seq, a.number
               FROM router_inputs i JOIN router_attempts a ON a.seq = i.seq
              WHERE i.chat_id = ?1 AND i.outcome IS NULL
                AND NOT EXISTS(SELECT 1 FROM chat_messages m JOIN discussion_drafts d ON d.message_id=m.id
                   JOIN discussion_quick_handoffs q ON q.draft_id=d.id WHERE m.seq=i.message_seq)
             UNION ALL
             SELECT a.thread_id, i.seq, a.number
               FROM router_inputs i JOIN router_attempts a ON a.seq = i.seq
              WHERE i.chat_id = ?1 AND i.outcome IS NOT NULL
                AND NOT EXISTS(SELECT 1 FROM chat_messages m JOIN discussion_drafts d ON d.message_id=m.id
                   JOIN discussion_quick_handoffs q ON q.draft_id=d.id WHERE m.seq=i.message_seq)
             ORDER BY i.seq DESC, a.number DESC",
        )?;
        let mut threads = Vec::new();
        let mut seen = HashSet::new();
        for thread in query.query_map([chat], |row| row.get::<_, String>(0))? {
            let thread = thread?;
            if seen.insert(thread.clone()) {
                threads.push(thread);
            }
        }
        Ok(threads)
    }

    pub fn workspace_choices(&self, issue: &str) -> Result<Vec<WorkspaceChoice>> {
        let mut statement = self.db.prepare(&format!(
            "WITH RECURSIVE tree(id) AS (SELECT id FROM issues WHERE id=?1 UNION ALL
              SELECT i.id FROM issues i JOIN tree ON i.parent_issue_id=tree.id)
              {WORKSPACE_CHOICES} JOIN tree ON i.id=tree.id ORDER BY n.number, t.rowid",
        ))?;
        Ok(statement
            .query_map([issue], read_choice)?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn workspace_choice(&self, issue: &str, task: &str) -> Result<Option<WorkspaceChoice>> {
        Ok(self
            .db
            .query_row(
                &format!("{WORKSPACE_CHOICES} WHERE i.id=?1 AND t.id=?2"),
                [issue, task],
                read_choice,
            )
            .optional()?)
    }

    pub fn is_discussion(&self, chat: &str) -> Result<bool> {
        Ok(self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM discussions WHERE chat_id=?1)",
            [chat],
            |r| r.get(0),
        )?)
    }

    pub fn create_discussion(&mut self, request: &CreateDiscussion) -> Result<Change<String>> {
        let intent = serde_json::to_string(request)?;
        let (tx, timing) = timing::begin(&mut self.db, "discussion_create")?;
        let existing: Option<(String, String)> = tx
            .query_row(
                "SELECT chat_id, creation FROM discussions WHERE operation_id=?1",
                [&request.operation_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((chat, original)) = existing {
            return Ok(if original == intent {
                Change::Ok(chat)
            } else {
                Change::Conflict
            });
        }
        let (origin, quoted, reference) = match &request.origin {
            None => (None, None, None),
            Some(DiscussionOrigin::Chat {
                chat_id,
                message_id,
            }) if chat_id == "main" => {
                let quote: Option<String> = tx
                    .query_row(
                        "SELECT text FROM chat_messages WHERE chat_id=?1 AND id=?2",
                        [chat_id, message_id],
                        |r| r.get(0),
                    )
                    .optional()?;
                let Some(quote) = quote else {
                    return Ok(Change::Missing);
                };
                (
                    Some(json!({"href":format!("/#message-{message_id}"), "label":"主 Chat 消息"})),
                    Some(quote),
                    None,
                )
            }
            Some(DiscussionOrigin::Issue { issue_id }) => {
                let Some(reference) = issue_ref(&tx, issue_id)? else {
                    return Ok(Change::Missing);
                };
                (
                    Some(json!({"href": reference.href, "label": reference.label})),
                    None,
                    Some(reference),
                )
            }
            _ => return Ok(Change::Invalid),
        };
        let id = protocol::new_id();
        tx.execute(
            "INSERT INTO discussions(chat_id,operation_id,creation,title,origin,quoted_text,title_source) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![id,request.operation_id,intent,request.title.as_deref().unwrap_or(UNTITLED_DISCUSSION).trim(),origin.map(|v|v.to_string()),quoted,if request.title.is_some() { "manual" } else { "default" }],
        )?;
        if let Some(reference) = reference {
            insert_ref(&tx, &id, &reference)?;
        }
        timing::commit(tx, timing)?;
        Ok(Change::Ok(id))
    }

    pub fn discussion_list(&self, archived: bool) -> Result<Vec<Value>> {
        let mut st = self.db.prepare(
            "SELECT chat_id,title,created_at,updated_at,archived FROM discussions d
             WHERE archived=?1 AND NOT EXISTS(SELECT 1 FROM feishu_handoff_bridges b WHERE b.chat_id=d.chat_id)
             ORDER BY updated_at DESC, rowid DESC",
        )?;
        Ok(st
            .query_map([archived], discussion_header)?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn discussion_detail(&self, chat: &str) -> Result<Option<Value>> {
        let meta = self.db.query_row(
            "SELECT chat_id,title,created_at,updated_at,archived,origin FROM discussions WHERE chat_id=?1",
            [chat], |r| Ok((discussion_header(r)?,r.get::<_,Option<String>>(5)?)),
        ).optional()?;
        let Some((mut meta, origin)) = meta else {
            return Ok(None);
        };
        meta["origin"] = origin
            .map(|v| serde_json::from_str(&v))
            .transpose()?
            .unwrap_or(Value::Null);
        let mut st = self.db.prepare(
            "SELECT message_id, draft_id FROM discussion_handoffs WHERE chat_id=?1 ORDER BY rowid DESC",
        )?;
        meta["handoffs"] = json!(st.query_map([chat], |r| {
            let id: String = r.get(0)?;
            Ok(json!({"message_id":id, "draft_id":r.get::<_, String>(1)?, "href":format!("/#message-{id}")}))
        })?.collect::<rusqlite::Result<Vec<_>>>()?);
        Ok(Some(meta))
    }

    pub fn rename_discussion(&self, chat: &str, title: &str) -> Result<bool> {
        Ok(self.db.execute(
            "UPDATE discussions SET title=?2,title_source='manual',updated_at=unixepoch() WHERE chat_id=?1",
            [chat, title.trim()],
        )? == 1)
    }

    pub fn set_discussion_archived(&self, chat: &str, archived: bool) -> Result<()> {
        self.db.execute(
            "UPDATE discussions SET archived=?2 WHERE chat_id=?1 AND archived<>?2",
            params![chat, archived],
        )?;
        Ok(())
    }

    pub fn discussion_quote(&self, chat: &str) -> Result<Option<String>> {
        Ok(self.db.query_row(
            "SELECT quoted_text FROM discussions WHERE chat_id=?1",
            [chat],
            |r| r.get(0),
        )?)
    }

    pub fn discussion_refs(&self, chat: &str) -> Result<Vec<DiscussionReference>> {
        read_refs(&self.db, chat)
    }

    pub fn discussion_ref(&self, chat: &str, id: &str) -> Result<Option<DiscussionReference>> {
        self.db
            .query_row(
                "SELECT reference FROM discussion_references WHERE chat_id=?1 AND id=?2",
                [chat, id],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .map(|r| Ok(serde_json::from_str(&r)?))
            .transpose()
    }

    pub fn add_discussion_ref(&self, chat: &str, reference: &DiscussionReference) -> Result<()> {
        insert_ref(&self.db, chat, reference)
    }

    pub fn remove_discussion_ref(&self, chat: &str, id: &str) -> Result<bool> {
        Ok(self.db.execute(
            "DELETE FROM discussion_references WHERE chat_id=?1 AND id=?2",
            [chat, id],
        )? == 1)
    }

    pub fn discussion_issue_ref(&self, issue: &str) -> Result<Option<DiscussionReference>> {
        issue_ref(&self.db, issue)
    }

    pub fn discussion_candidates(&self, term: &str, target_only: bool) -> Result<Value> {
        let terms = term.trim().to_lowercase();
        let repos = if target_only {
            Vec::new()
        } else {
            self.registered_repo_ids()?
                .into_iter()
                .filter(|id| id.to_lowercase().contains(&terms))
                .take(12)
                .collect()
        };
        let number = protocol::parse_issue_number(&terms);
        let mut st = self.db.prepare(
            "SELECT i.id, i.title, n.number FROM issues i JOIN issue_short_numbers n ON n.issue_id=i.id
              JOIN chat_messages m ON m.seq=i.source_seq
             WHERE (NOT ?3 OR m.chat_id='main')
               AND (n.number=?2 OR i.id=?1 OR instr(lower(i.title),?1)>0)
             ORDER BY n.number=?2 DESC NULLS LAST, n.number DESC LIMIT 16",
        )?;
        let issues = st.query_map(params![terms,number,target_only], |r| {
            Ok(json!({"issue_id":r.get::<_,String>(0)?, "title":r.get::<_,String>(1)?, "short_number":r.get::<_,i64>(2)?}))
        })?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(json!({"repos":repos,"issues":issues}))
    }

    pub fn discussion_history(&self, chat: &str, before: i64) -> Result<Value> {
        let mut st = self.db.prepare(
            "SELECT seq,id,operation_id IS NOT NULL,text,created_at,reply_to
             FROM chat_messages m WHERE chat_id=?1 AND seq<?2
               AND NOT EXISTS(SELECT 1 FROM discussion_drafts d WHERE d.message_id=m.id)
             ORDER BY seq DESC LIMIT 13",
        )?;
        let mut rows = st.query(params![chat, before])?;
        let mut messages = Vec::new();
        let mut bytes = 0;
        let mut next = None;
        while let Some(r) = rows.next()? {
            let text = r.get::<_, String>(3)?;
            // Keep the normal page bounded without discarding the tail of a
            // single long message needed after native history loss.
            if messages.len() == 12 || (!messages.is_empty() && bytes + text.len() > 48 * 1024) {
                next = messages.last().and_then(|v: &Value| v["seq"].as_i64());
                break;
            }
            bytes += text.len();
            messages.push(json!({"seq":r.get::<_,i64>(0)?,"id":r.get::<_,String>(1)?,
                "role":if r.get::<_,bool>(2)? {"user"} else {"assistant"},
                "text":text,"created_at":r.get::<_,i64>(4)?,
                "reply_to":r.get::<_,Option<String>>(5)?,"truncated":false}));
        }
        messages.reverse();
        Ok(json!({"messages":messages,"next_before":next}))
    }

    pub fn discussion_issue(&self, issue: &str, before: i64) -> Result<Option<Value>> {
        let Some(detail) = self.issue_with_reports(issue)? else {
            return Ok(None);
        };
        let mut st = self.db.prepare(
            "SELECT seq,id,kind,author,text,created_at,reply_to FROM issue_messages
             WHERE issue_id=?1 AND seq<?2 AND kind != 'takeover' ORDER BY seq DESC LIMIT 26",
        )?;
        let mut messages = st
            .query_map(params![issue, before], |r| {
                Ok(json!({"seq":r.get::<_,i64>(0)?,"id":r.get::<_,String>(1)?,
                "kind":r.get::<_,String>(2)?,"author":r.get::<_,String>(3)?,
                "text":r.get::<_,String>(4)?,"created_at":r.get::<_,i64>(5)?,
                "reply_to":r.get::<_,Option<String>>(6)?}))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let next = if messages.len() > 25 {
            messages.pop();
            messages.last().and_then(|v| v["seq"].as_i64())
        } else {
            None
        };
        messages.reverse();
        Ok(Some(
            json!({"issue":detail, "messages":messages, "next_before":next, "artifacts":self.issue_artifacts(issue)?}),
        ))
    }

    pub fn request_discussion_draft(
        &mut self,
        chat: &str,
        id: &str,
        manual: bool,
    ) -> Result<Change<()>> {
        let (tx, timing) = timing::begin(&mut self.db, "discussion_draft")?;
        let original: Option<(String, bool, bool)> = tx.query_row(
            "SELECT chat_id, manual, EXISTS(SELECT 1 FROM discussion_quick_handoffs q WHERE q.draft_id=d.id)
               FROM discussion_drafts d WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).optional()?;
        if let Some((original, was_manual, quick)) = original {
            return Ok(if original == chat && was_manual == manual && !quick {
                Change::Ok(())
            } else {
                Change::Conflict
            });
        }
        let used: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM chat_messages WHERE operation_id=?1)",
            [id],
            |r| r.get(0),
        )?;
        if used || Self::reserved_chat_operation(&tx, id)? {
            return Ok(Change::Conflict);
        }
        insert_draft_input(&tx, chat, id, manual, DRAFT_PROMPT)?;
        timing::commit(tx, timing)?;
        Ok(Change::Ok(()))
    }

    pub fn discussion_draft(&self, chat: &str, id: Option<&str>) -> Result<Option<Value>> {
        let row = self
            .db
            .query_row(
                "SELECT d.id, d.draft, i.outcome, i.error, h.message_id FROM discussion_drafts d
             JOIN chat_messages m ON m.id=d.message_id JOIN router_inputs i ON i.message_seq=m.seq
             LEFT JOIN discussion_handoffs h ON h.draft_id=d.id
             WHERE d.chat_id=?1 AND (?2 IS NULL OR d.id=?2)
               AND NOT EXISTS(SELECT 1 FROM discussion_quick_handoffs q WHERE q.draft_id=d.id)
             ORDER BY d.rowid DESC LIMIT 1",
                params![chat, id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .optional()?;
        row.map(|(id, draft, outcome, error, sent)| {
            Ok(json!({
                "id":id, "draft": draft.map(|v|serde_json::from_str::<Value>(&v)).transpose()?,
                "status":outcome.unwrap_or_else(||"queued".into()),"error":error,"message_id":sent,
            }))
        })
        .transpose()
    }

    pub fn is_draft_input(&self, message_id: &str) -> Result<bool> {
        Ok(self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM discussion_drafts WHERE message_id=?1)",
            [message_id],
            |r| r.get(0),
        )?)
    }

    pub fn finish_discussion_draft(
        &mut self,
        seq: i64,
        message: &str,
        draft: &DiscussionDraft,
    ) -> Result<()> {
        let (tx, timing) = timing::begin(&mut self.db, "discussion_draft_finish")?;
        if tx.execute(
            "UPDATE router_inputs SET outcome='completed' WHERE seq=?1 AND outcome IS NULL",
            [seq],
        )? == 1
        {
            ensure!(
                tx.execute(
                    "UPDATE discussion_drafts SET draft=?2 WHERE message_id=?1",
                    params![message, serde_json::to_string(draft)?]
                )? == 1,
                "missing draft input"
            );
        }
        timing::commit(tx, timing)?;
        Ok(())
    }

    pub fn send_discussion(
        &mut self,
        chat: &str,
        request: &DiscussionSend,
    ) -> Result<Change<Receipt>> {
        let intent = serde_json::to_string(request)?;
        let (tx, timing) = timing::begin(&mut self.db, "discussion_handoff")?;
        let original: Option<(String, String, String, bool)> = tx
            .query_row(
                "SELECT h.chat_id,h.intent,h.message_id,q.operation_id IS NOT NULL FROM discussion_handoffs h
                    LEFT JOIN discussion_quick_handoffs q ON q.draft_id=h.draft_id WHERE h.operation_id=?1",
                [&request.operation_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        if let Some((original_chat, original, message_id, quick)) = original {
            return Ok(
                if !quick
                    && original_chat == chat
                    && serde_json::from_str::<DiscussionSend>(&original)? == *request
                {
                    Change::Ok(Receipt { message_id })
                } else {
                    Change::Conflict
                },
            );
        }
        let used: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM chat_messages WHERE operation_id=?1)
                 OR EXISTS(SELECT 1 FROM discussion_handoffs WHERE draft_id=?2)",
            params![request.operation_id, request.draft_id],
            |r| r.get(0),
        )?;
        if used || Self::reserved_chat_operation(&tx, &request.operation_id)? {
            return Ok(Change::Conflict);
        }
        let ready: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM discussion_drafts d WHERE id=?1 AND chat_id=?2 AND draft IS NOT NULL
                 AND NOT EXISTS(SELECT 1 FROM discussion_quick_handoffs q WHERE q.draft_id=d.id))",
            [&request.draft_id,chat], |r|r.get(0),
        )?;
        if !ready {
            return Ok(Change::Missing);
        }
        let mut refs = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for id in &request.reference_ids {
            if !seen.insert(id) {
                return Ok(Change::Invalid);
            }
            let raw: Option<String> = tx
                .query_row(
                    "SELECT reference FROM discussion_references WHERE chat_id=?1 AND id=?2",
                    [chat, id],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(raw) = raw else {
                return Ok(Change::Missing);
            };
            refs.push(serde_json::from_str::<DiscussionReference>(&raw)?);
        }
        let target = if let Some(id) = &request.target_issue_id {
            let target: Option<(String, i64)> = tx.query_row(
                "SELECT i.title,n.number FROM issues i JOIN issue_short_numbers n ON n.issue_id=i.id
                 JOIN chat_messages m ON m.seq=i.source_seq WHERE i.id=?1 AND m.chat_id='main'",
                [id], |r| Ok((r.get(0)?,r.get(1)?)),
            ).optional()?;
            let Some((title, number)) = target else {
                return Ok(Change::Missing);
            };
            Some(json!({"issue_id":id,"short_number":number,"title":title}))
        } else {
            None
        };
        let target = target.map_or_else(
            || "未指定".into(),
            |value| {
                format!(
                    "#{} {} ({})",
                    value["short_number"],
                    value["title"].as_str().unwrap_or_default(),
                    value["issue_id"].as_str().unwrap_or_default()
                )
            },
        );
        let receipt = insert_handoff(
            &tx,
            chat,
            &request.operation_id,
            &request.draft_id,
            &intent,
            &request.draft,
            &refs,
            &target,
        )?;
        timing::commit(tx, timing)?;
        Ok(Change::Ok(receipt))
    }
}

const DRAFT_PROMPT: &str = "Prepare an editable handoff draft from this discussion. Use the complete discussion context; page through tend_read_discussion_history if needed to recover earlier messages. Return only a JSON object with the five string fields goal, decisions, pending_items, constraints, authorization_end. Use the user's language. Distinguish explicit user decisions from suggestions and unresolved items. In authorization_end, state what the user explicitly authorized and where to stop, or explicitly say no implementation was authorized. Do not infer authorization, select execution targets, or send anything.";

fn insert_draft_input(
    db: &Connection,
    chat: &str,
    id: &str,
    manual: bool,
    prompt: &str,
) -> Result<Receipt> {
    let receipt = super::insert_chat_input(
        db,
        chat,
        &SendMessage {
            operation_id: id.to_owned(),
            text: prompt.to_owned(),
            reply_to: None,
        },
    )?;
    let draft = manual
        .then(|| serde_json::to_string(&DiscussionDraft::default()))
        .transpose()?;
    db.execute(
        "INSERT INTO discussion_drafts(id,chat_id,message_id,draft,manual) VALUES(?1,?2,?3,?4,?5)",
        params![id, chat, receipt.message_id, draft, manual],
    )?;
    if manual {
        db.execute(
            "UPDATE router_inputs SET outcome='completed' WHERE message_seq=(SELECT seq FROM chat_messages WHERE id=?1)",
            [&receipt.message_id],
        )?;
    }
    Ok(receipt)
}

fn read_refs(db: &Connection, chat: &str) -> Result<Vec<DiscussionReference>> {
    let mut st =
        db.prepare("SELECT reference FROM discussion_references WHERE chat_id=?1 ORDER BY rowid")?;
    st.query_map([chat], |r| r.get::<_, String>(0))?
        .map(|r| Ok(serde_json::from_str(&r?)?))
        .collect()
}

fn insert_handoff(
    db: &Connection,
    chat: &str,
    operation_id: &str,
    draft_id: &str,
    intent: &str,
    draft: &DiscussionDraft,
    refs: &[DiscussionReference],
    target: &str,
) -> Result<Receipt> {
    let references = reference_footer(refs);
    let text = format!(
        "讨论转交 · 上下文摘要。转交不是新的实施授权或命令。仅在原讨论中用户已明确授权的范围内推进；待定事项先与用户确认。参考资料不是执行目标。\n\n目标\n{}\n\n已明确决策\n{}\n\n待定事项\n{}\n\n约束\n{}\n\n授权终点\n{}\n\n指定推进的 Issue\n{target}\n\n参考资料\n{references}",
        draft.goal,
        draft.decisions,
        draft.pending_items,
        draft.constraints,
        draft.authorization_end,
    );
    record_handoff(db, chat, operation_id, draft_id, intent, text)
}

fn reference_footer(refs: &[DiscussionReference]) -> String {
    if refs.is_empty() {
        "未选取".into()
    } else {
        refs.iter()
            .map(|reference| {
                format!(
                    "- {} [{}] {}",
                    reference.label,
                    reference.id,
                    reference.href.as_deref().unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn record_handoff(
    db: &Connection,
    chat: &str,
    operation_id: &str,
    draft_id: &str,
    intent: &str,
    text: String,
) -> Result<Receipt> {
    let receipt = super::insert_chat_input(
        db,
        "main",
        &SendMessage {
            operation_id: operation_id.to_owned(),
            text,
            reply_to: None,
        },
    )?;
    db.execute(
        "INSERT INTO discussion_handoffs(operation_id,chat_id,draft_id,intent,message_id) VALUES(?1,?2,?3,?4,?5)",
        params![operation_id,chat,draft_id,intent,receipt.message_id],
    )?;
    Ok(receipt)
}

fn discussion_header(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(
        json!({"chat_id":r.get::<_,String>(0)?, "title":r.get::<_,String>(1)?,
        "created_at":r.get::<_,i64>(2)?, "updated_at":r.get::<_,i64>(3)?,
        "archived":r.get::<_,bool>(4)?}),
    )
}

fn issue_ref(db: &Connection, issue: &str) -> Result<Option<DiscussionReference>> {
    let (column, lookup) = match protocol::parse_issue_number(issue) {
        Some(number) => ("n.number", number.to_string()),
        None => ("i.id", issue.to_owned()),
    };
    Ok(db.query_row(
        &format!("SELECT i.id, i.title, n.number FROM issues i JOIN issue_short_numbers n ON n.issue_id=i.id WHERE {column}=?1"),
        [&lookup], |r| {
            let id: String = r.get(0)?;
            let title: String = r.get(1)?;
            let number: i64 = r.get(2)?;
            Ok(DiscussionReference { id: format!("issue:{id}"), source: DiscussionSource::Issue {issue_id:id},
                label:format!("#{number} {title}"),href:Some(format!("/issues/{number}")) })
        },
    ).optional()?)
}

fn insert_ref(db: &Connection, chat: &str, reference: &DiscussionReference) -> Result<()> {
    db.execute(
        "INSERT INTO discussion_references(chat_id,id,reference) VALUES(?1,?2,?3) ON CONFLICT(chat_id,id) DO NOTHING",
        params![chat,reference.id,serde_json::to_string(reference)?],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create(store: &mut Store, request: &CreateDiscussion) -> Result<String> {
        let Change::Ok(chat) = store.create_discussion(request)? else {
            anyhow::bail!("fixture creation failed");
        };
        Ok(chat)
    }

    fn new_discussion(store: &mut Store, title: Option<&str>) -> Result<String> {
        create(
            store,
            &CreateDiscussion {
                operation_id: protocol::new_id(),
                title: title.map(str::to_owned),
                origin: None,
            },
        )
    }

    fn send(store: &mut Store, chat: &str, operation: &str, text: &str) -> Result<String> {
        let super::super::Accepted::Received(receipt) = store.accept(
            chat,
            &SendMessage {
                operation_id: operation.to_owned(),
                text: text.to_owned(),
                reply_to: None,
            },
        )?
        else {
            anyhow::bail!("fixture send failed")
        };
        Ok(receipt.message_id)
    }

    fn title(store: &Store, chat: &str) -> Result<String> {
        Ok(store.discussion_detail(chat)?.expect("discussion")["title"]
            .as_str()
            .unwrap()
            .to_owned())
    }

    #[test]
    fn handoff_source_checks_exact_user_identity_and_click_boundary() -> Result<()> {
        let mut store = Store::initialize(std::path::Path::new(":memory:"))?;
        let chat = new_discussion(&mut store, None)?;
        let other = new_discussion(&mut store, None)?;
        let raw = "  Implement just the fixture.\n Do not deploy.  ";
        let first = send(&mut store, &chat, &protocol::new_id(), raw)?;
        let foreign = send(
            &mut store,
            &other,
            &protocol::new_id(),
            "A foreign Discussion",
        )?;
        let operation = store.quick_handoff(&chat, None)?.operation_id;
        store.request_quick_handoff(
            &chat,
            &protocol::DiscussionQuickRequest {
                operation_id: operation.clone(),
                retry_generation: None,
            },
        )?;
        let (hidden, seq): (String, i64) = store.db.query_row(
            "SELECT d.message_id,i.seq FROM discussion_quick_handoffs q JOIN discussion_drafts d ON d.id=q.draft_id
             JOIN chat_messages m ON m.id=d.message_id JOIN router_inputs i ON i.message_seq=m.seq
             WHERE q.operation_id=?1", [&operation], |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let later = send(
            &mut store,
            &chat,
            &protocol::new_id(),
            "Later unrelated user input",
        )?;
        let request = store.quick_input(&hidden)?.expect("quick request");
        store.finish_quick_handoff(seq, &request, "Agent's generated text")?;
        let handoff = store.next_input("main")?.expect("handoff in main");
        let source = handoff.handoff_source.expect("source for new handoff");
        assert_eq!(source.chat_id, chat);
        let page = store.handoff_history(&source, i64::MAX)?;
        let [message] = page["messages"]
            .as_array()
            .expect("message page")
            .as_slice()
        else {
            anyhow::bail!("expected only the original user message: {page}");
        };
        assert_eq!(message["id"], first);
        assert_eq!(message["text"], raw);
        assert_eq!(
            message["href"],
            format!("/discussions?chat={chat}#message-{first}")
        );
        let evidence = store
            .handoff_user_messages(&source, &[&first])?
            .expect("valid source");
        assert_eq!(evidence["user_messages"][0]["text"], raw);
        for invalid in [&foreign, &hidden, &later, &handoff.message_id] {
            assert!(
                store
                    .handoff_user_messages(&source, &[&first, invalid])?
                    .is_none()
            );
        }
        Ok(())
    }

    fn legacy_database(db: &Connection) -> Result<()> {
        remove_quick_handoff_and_title_schema(db)?;
        crate::store::remove_token_usage_overview_schema(db)?;
        db.pragma_update(None, "user_version", 41)?;
        Ok(())
    }

    #[test]
    fn complete_unicode_title_comes_only_from_first_prompt() -> Result<()> {
        let mut store = Store::initialize(std::path::Path::new(":memory:"))?;
        let chat = new_discussion(&mut store, None)?;
        assert_eq!(title(&store, &chat)?, UNTITLED_DISCUSSION);
        store.request_discussion_draft(&chat, "hidden", true)?;
        assert_eq!(title(&store, &chat)?, UNTITLED_DISCUSSION);
        let long = "完整标题 ".repeat(250);
        let raw = format!("   开头　\n\r\t{long}\u{2002}结尾  ");
        let expected = format!("开头 {long}结尾");
        assert_eq!(title_from_prompt(&raw), expected);
        assert_eq!(title_from_prompt(" \t\r\n　\u{2002} "), UNTITLED_DISCUSSION);
        let first = send(&mut store, &chat, "first", &raw)?;
        assert_eq!(title(&store, &chat)?, expected);
        assert_eq!(send(&mut store, &chat, "first", &raw)?, first);
        send(&mut store, &chat, "second", "Later input")?;
        assert_eq!(title(&store, &chat)?, expected);
        assert_eq!(store.discussion_list(false)?[0]["title"], expected);
        for origin in [Some(UNTITLED_DISCUSSION), Some("Explicit title"), None] {
            let chat = new_discussion(&mut store, origin)?;
            if origin.is_none() {
                assert!(store.rename_discussion(&chat, UNTITLED_DISCUSSION)?);
            }
            send(
                &mut store,
                &chat,
                &protocol::new_id(),
                "Must not override manual title",
            )?;
            assert_eq!(title(&store, &chat)?, origin.unwrap_or(UNTITLED_DISCUSSION));
        }
        store.rename_discussion(&chat, UNTITLED_DISCUSSION)?;
        assert_eq!(send(&mut store, &chat, "first", &raw)?, first);
        assert_eq!(title(&store, &chat)?, UNTITLED_DISCUSSION);
        Ok(())
    }

    #[test]
    fn migration_reads_first_real_prompt_and_preserves_known_manual_titles() -> Result<()> {
        let workspace = TempDb::new()?;
        let mut store = Store::initialize(&workspace.path)?;
        let backfill = new_discussion(&mut store, None)?;
        let draft_only = new_discussion(&mut store, None)?;
        let explicit_default = new_discussion(&mut store, Some(UNTITLED_DISCUSSION))?;
        let manual_default = new_discussion(&mut store, None)?;
        let manual_other = new_discussion(&mut store, None)?;
        let blank = new_discussion(&mut store, None)?;
        for chat in [&backfill, &draft_only] {
            store.request_discussion_draft(chat, &protocol::new_id(), true)?;
        }
        for chat in [&backfill, &explicit_default, &manual_default, &manual_other] {
            store.db.execute(
                "INSERT INTO chat_messages(id,chat_id,text,created_at) VALUES(?1,?2,'Agent text',100)",
                params![protocol::new_id(), chat],
            )?;
            send(&mut store, chat, &protocol::new_id(), " 首条\n　输入 ")?;
            send(&mut store, chat, &protocol::new_id(), "Later input")?;
        }
        send(&mut store, &blank, &protocol::new_id(), "　 \n")?;
        store.rename_discussion(&manual_other, "Other manual title")?;
        store.db.execute(
            "UPDATE discussions SET title=?1,updated_at=(SELECT MAX(m.created_at) FROM chat_messages m WHERE m.chat_id=discussions.chat_id)
              WHERE chat_id IN (?2,?3,?4)",
            params![UNTITLED_DISCUSSION, backfill, blank, manual_default],
        )?;
        store.db.execute(
            "UPDATE discussions SET updated_at=updated_at+5 WHERE chat_id=?1",
            [&manual_default],
        )?;
        store.db.execute(
            "UPDATE discussions SET updated_at=updated_at+1 WHERE chat_id=?1",
            [&backfill],
        )?;
        legacy_database(&store.db)?;
        drop(store);
        let mut store = Store::initialize(&workspace.path)?;
        assert_eq!(title(&store, &backfill)?, "首条 输入");
        assert_eq!(title(&store, &manual_other)?, "Other manual title");
        for chat in [&draft_only, &explicit_default, &manual_default, &blank] {
            assert_eq!(title(&store, chat)?, UNTITLED_DISCUSSION);
            send(&mut store, chat, &protocol::new_id(), "After migration")?;
        }
        assert_eq!(title(&store, &draft_only)?, "After migration");
        for chat in [&explicit_default, &manual_default, &blank] {
            assert_eq!(title(&store, chat)?, UNTITLED_DISCUSSION);
        }
        Ok(())
    }

    #[test]
    fn simultaneous_prompts_and_replays_use_acceptance_order() -> Result<()> {
        let workspace = TempDb::new()?;
        let mut store = Store::initialize(&workspace.path)?;
        let chat = new_discussion(&mut store, None)?;
        let operations = [
            ("op-one", "First concurrent prompt"),
            ("op-two", "Another concurrent prompt"),
            ("op-one", "First concurrent prompt"),
        ];
        let barrier = std::sync::Barrier::new(operations.len());
        let received = std::thread::scope(|scope| {
            operations
                .iter()
                .map(|&(id, text)| {
                    let (barrier, chat, path) = (&barrier, &chat, &workspace.path);
                    scope.spawn(move || -> Result<String> {
                        let mut store = Store::open(path)?;
                        store.enable_foreground_writes()?;
                        barrier.wait();
                        send(&mut store, chat, id, text)
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect::<Result<Vec<_>>>()
        })?;
        assert_eq!(received[0], received[2]);
        assert_ne!(received[0], received[1]);
        let page = store.messages(&chat, i64::MAX)?;
        assert_eq!(page.messages.len(), 2);
        assert_eq!(title(&store, &chat)?, page.messages[0].text);
        Ok(())
    }

    struct TempDb {
        directory: std::path::PathBuf,
        path: std::path::PathBuf,
    }

    impl TempDb {
        fn new() -> Result<Self> {
            let directory =
                std::env::temp_dir().join(format!("tend-discussion-{}", protocol::new_id()));
            std::fs::create_dir(&directory)?;
            let path = directory.join("server.sqlite");
            Ok(Self { directory, path })
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    #[test]
    fn archive_migration_distinguishes_legacy_defaults_from_explicit_titles() -> Result<()> {
        let mut store = Store::initialize(std::path::Path::new(":memory:"))?;
        store.db.execute_batch(
            "DROP INDEX discussions_by_activity;
             ALTER TABLE discussions DROP COLUMN archived;
             CREATE INDEX discussions_by_activity ON discussions(updated_at DESC, chat_id);",
        )?;
        let cases = [
            ("implicit", None, "新讨论", 100, UNTITLED_DISCUSSION),
            ("explicit", Some("新讨论"), "新讨论", 100, "新讨论"),
            ("different_title", None, "自选标题", 200, "自选标题"),
            (
                "changed_explicit",
                Some("其他标题"),
                "新讨论",
                200,
                "新讨论",
            ),
            ("same_name_rename", None, "新讨论", 200, "新讨论"),
            ("same_name_with_draft", None, "新讨论", 200, "新讨论"),
            (
                "untitled_with_message",
                None,
                "新讨论",
                200,
                UNTITLED_DISCUSSION,
            ),
        ];
        for (chat, requested, stored, updated, _) in cases {
            let request = CreateDiscussion {
                operation_id: format!("op-{chat}"),
                title: requested.map(str::to_owned),
                origin: None,
            };
            store.db.execute(
                "INSERT INTO discussions(chat_id,operation_id,creation,title,created_at,updated_at)
                 VALUES (?1,?2,?3,?4,100,?5)",
                params![
                    chat,
                    request.operation_id,
                    serde_json::to_string(&request)?,
                    stored,
                    updated
                ],
            )?;
        }
        store.db.execute(
            "INSERT INTO chat_messages (chat_id,id,text,operation_id,created_at)
             VALUES ('untitled_with_message','test-message','fixture','test-operation',199)",
            [],
        )?;
        assert!(matches!(
            store.request_discussion_draft("same_name_with_draft", "draft-op", false)?,
            Change::Ok(())
        ));
        let tx = store.db.transaction()?;
        migrate_archive(&tx)?;
        tx.commit()?;
        for (id, _, _, updated, title) in cases {
            let header = store.discussion_detail(id)?.expect("same ID");
            assert_eq!(header["title"], title, "{id}");
            assert_eq!(header["updated_at"], updated, "{id}");
            assert_eq!(header["archived"], false, "{id}");
        }
        let replay = CreateDiscussion {
            operation_id: "op-implicit".into(),
            title: None,
            origin: None,
        };
        assert_eq!(create(&mut store, &replay)?, "implicit");
        assert_eq!(store.discussion_list(false)?.len(), cases.len());
        assert!(store.discussion_list(true)?.is_empty());
        Ok(())
    }

    #[test]
    fn archive_is_a_replayable_list_move_not_a_new_discussion() -> Result<()> {
        let mut store = Store::initialize(std::path::Path::new(":memory:"))?;
        let request = CreateDiscussion {
            operation_id: "create-discussion".into(),
            title: None,
            origin: None,
        };
        let chat = create(&mut store, &request)?;
        assert_eq!(
            store.discussion_list(false)?[0]["title"],
            UNTITLED_DISCUSSION
        );
        assert!(store.rename_discussion(&chat, "新讨论")?);
        let before = store.discussion_detail(&chat)?.expect("created discussion");

        for archived in [true, true, false, false] {
            store.set_discussion_archived(&chat, archived)?;
            assert!(store.discussion_list(!archived)?.is_empty());
            let [header]: [_; 1] = store.discussion_list(archived)?.try_into().unwrap();
            assert_eq!(header["chat_id"], chat);
            assert_eq!(header["title"], "新讨论");
            assert_eq!(header["archived"], archived);
            assert_eq!(header["updated_at"], before["updated_at"]);
            assert_eq!(create(&mut store, &request)?, chat);
        }
        assert_eq!(store.discussion_detail(&chat)?, Some(before));
        Ok(())
    }
}
