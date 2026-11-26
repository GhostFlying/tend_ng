use super::{Store, timing};
use anyhow::{Result, ensure};
use protocol::{
    CreateDiscussion, DiscussionDraft, DiscussionOrigin, DiscussionReference, DiscussionSend,
    DiscussionSource, Receipt, SendMessage,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

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

pub fn valid_title(title: &str) -> bool {
    !title.trim().is_empty() && title.chars().count() <= 120
}

pub fn valid_draft(draft: &DiscussionDraft) -> bool {
    !draft.goal.trim().is_empty()
        && [
            &draft.goal,
            &draft.decisions,
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
            "INSERT INTO discussions(chat_id,operation_id,creation,title,origin,quoted_text) VALUES(?1,?2,?3,?4,?5,?6)",
            params![id,request.operation_id,intent,request.title.as_deref().unwrap_or("新讨论").trim(),origin.map(|v|v.to_string()),quoted],
        )?;
        if let Some(reference) = reference {
            insert_ref(&tx, &id, &reference)?;
        }
        timing::commit(tx, timing)?;
        Ok(Change::Ok(id))
    }

    pub fn discussion_list(&self) -> Result<Vec<Value>> {
        let mut st = self.db.prepare("SELECT chat_id,title,created_at,updated_at FROM discussions ORDER BY updated_at DESC, rowid DESC")?;
        Ok(st
            .query_map([], discussion_header)?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn discussion_detail(&self, chat: &str) -> Result<Option<Value>> {
        let meta = self.db.query_row(
            "SELECT chat_id,title,created_at,updated_at,origin FROM discussions WHERE chat_id=?1",
            [chat], |r| Ok((discussion_header(r)?,r.get::<_,Option<String>>(4)?)),
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
            "UPDATE discussions SET title=?2,updated_at=unixepoch() WHERE chat_id=?1",
            [chat, title.trim()],
        )? == 1)
    }

    pub fn discussion_quote(&self, chat: &str) -> Result<Option<String>> {
        Ok(self.db.query_row(
            "SELECT quoted_text FROM discussions WHERE chat_id=?1",
            [chat],
            |r| r.get(0),
        )?)
    }

    pub fn discussion_refs(&self, chat: &str) -> Result<Vec<DiscussionReference>> {
        let mut st = self.db.prepare(
            "SELECT reference FROM discussion_references WHERE chat_id=?1 ORDER BY rowid",
        )?;
        st.query_map([chat], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
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
            "SELECT seq,id,operation_id IS NOT NULL,substr(text,1,8192),created_at,reply_to,length(text)>8192
             FROM chat_messages m WHERE chat_id=?1 AND seq<?2
               AND NOT EXISTS(SELECT 1 FROM discussion_drafts d WHERE d.message_id=m.id)
             ORDER BY seq DESC LIMIT 13",
        )?;
        let mut messages = st
            .query_map(params![chat, before], |r| {
                Ok(json!({"seq":r.get::<_,i64>(0)?,"id":r.get::<_,String>(1)?,
                "role":if r.get::<_,bool>(2)? {"user"} else {"assistant"},
                "text":r.get::<_,String>(3)?,"created_at":r.get::<_,i64>(4)?,
                "reply_to":r.get::<_,Option<String>>(5)?,"truncated":r.get::<_,bool>(6)?}))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let next = if messages.len() > 12 {
            messages.pop();
            messages.last().and_then(|v| v["seq"].as_i64())
        } else {
            None
        };
        messages.reverse();
        Ok(json!({"messages":messages,"next_before":next}))
    }

    pub fn discussion_issue(&self, issue: &str, before: i64) -> Result<Option<Value>> {
        let Some(detail) = self.issue_with_reports(issue)? else {
            return Ok(None);
        };
        let mut st = self.db.prepare(
            "SELECT seq,id,kind,author,text,created_at,reply_to FROM issue_messages
             WHERE issue_id=?1 AND seq<?2 ORDER BY seq DESC LIMIT 26",
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

    pub fn request_discussion_draft(&mut self, chat: &str, id: &str) -> Result<Change<()>> {
        let (tx, timing) = timing::begin(&mut self.db, "discussion_draft")?;
        let original: Option<String> = tx
            .query_row(
                "SELECT chat_id FROM discussion_drafts WHERE id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(original) = original {
            return Ok(if original == chat {
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
        if used {
            return Ok(Change::Conflict);
        }
        let request = SendMessage { operation_id:id.to_owned(),
            text:"Prepare the editable handoff draft now from this discussion and the selected references. Output the four JSON string fields goal, decisions, constraints, authorization_end only. Do not infer authorization, select execution targets, or send anything.".to_owned(),
            reply_to:None };
        let receipt = super::insert_chat_input(&tx, chat, &request)?;
        tx.execute(
            "INSERT INTO discussion_drafts(id,chat_id,message_id) VALUES(?1,?2,?3)",
            params![id, chat, receipt.message_id],
        )?;
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
             WHERE d.chat_id=?1 AND (?2 IS NULL OR d.id=?2) ORDER BY d.rowid DESC LIMIT 1",
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
        let original: Option<(String, String, String)> = tx
            .query_row(
                "SELECT chat_id,intent,message_id FROM discussion_handoffs WHERE operation_id=?1",
                [&request.operation_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if let Some((original_chat, original, message_id)) = original {
            return Ok(if original_chat == chat && original == intent {
                Change::Ok(Receipt { message_id })
            } else {
                Change::Conflict
            });
        }
        let used: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM chat_messages WHERE operation_id=?1)
                 OR EXISTS(SELECT 1 FROM discussion_handoffs WHERE draft_id=?2)",
            params![request.operation_id, request.draft_id],
            |r| r.get(0),
        )?;
        if used {
            return Ok(Change::Conflict);
        }
        let ready: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM discussion_drafts WHERE id=?1 AND chat_id=?2 AND draft IS NOT NULL)",
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
        let references = if refs.is_empty() {
            "未选取".into()
        } else {
            refs.into_iter()
                .map(|reference| {
                    format!(
                        "- {} [{}] {}",
                        reference.label,
                        reference.id,
                        reference.href.unwrap_or_default()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let text = format!(
            "从独立讨论转交。以下内容已由我确认。参考资料不是执行目标，请按目标和授权终点判断下一步。\n\n目标\n{}\n\n已明确决策\n{}\n\n约束\n{}\n\n授权终点\n{}\n\n指定推进的 Issue\n{target}\n\n参考资料\n{references}",
            request.draft.goal,
            request.draft.decisions,
            request.draft.constraints,
            request.draft.authorization_end,
        );
        let request_message = SendMessage {
            operation_id: request.operation_id.clone(),
            text,
            reply_to: None,
        };
        let receipt = super::insert_chat_input(&tx, "main", &request_message)?;
        tx.execute(
            "INSERT INTO discussion_handoffs(operation_id,chat_id,draft_id,intent,message_id) VALUES(?1,?2,?3,?4,?5)",
            params![request.operation_id,chat,request.draft_id,intent,receipt.message_id],
        )?;
        timing::commit(tx, timing)?;
        Ok(Change::Ok(receipt))
    }
}

fn discussion_header(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(
        json!({"chat_id":r.get::<_,String>(0)?, "title":r.get::<_,String>(1)?,
        "created_at":r.get::<_,i64>(2)?, "updated_at":r.get::<_,i64>(3)?}),
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
