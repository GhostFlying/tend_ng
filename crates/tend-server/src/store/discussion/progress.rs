use super::super::Store;
use anyhow::Result;
use protocol::DiscussionProgress;
use rusqlite::{Connection, params};
use serde_json::Value;
use std::collections::HashMap;

pub(in crate::store) fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS discussion_progress (
            ordinal INTEGER PRIMARY KEY,
            attempt_id TEXT NOT NULL REFERENCES router_attempts(id),
            item_id TEXT NOT NULL,
            text TEXT NOT NULL,
            UNIQUE(attempt_id, item_id)
         );",
    )?;
    Ok(())
}

impl Store {
    pub(crate) fn record_discussion_progress(
        &self,
        attempt: &str,
        thread: &str,
        turn: &str,
        item: &Value,
    ) {
        let Some((id, text)) = commentary(item) else {
            return;
        };
        if let Err(error) = self.insert_discussion_progress(attempt, thread, turn, id, text) {
            // Observing progress must never retry or fail a model response.
            eprintln!("Could not save Discussion progress for attempt {attempt}: {error:#}");
        }
    }

    fn insert_discussion_progress(
        &self,
        attempt: &str,
        thread: &str,
        turn: &str,
        id: &str,
        text: &str,
    ) -> Result<()> {
        self.db
            .prepare_cached(
                "INSERT INTO discussion_progress(attempt_id, item_id, text)
             SELECT a.id, ?4, ?5 FROM router_attempts a
             JOIN router_inputs i ON i.seq = a.seq
             JOIN chat_messages m ON m.seq = i.message_seq AND m.chat_id = i.chat_id
             JOIN discussions d ON d.chat_id = i.chat_id
             WHERE a.id = ?1 AND a.thread_id = ?2 AND a.turn_id = ?3 AND i.outcome IS NULL
               AND i.report_id IS NULL AND i.attention_id IS NULL AND i.external_batch IS NULL
               AND m.operation_id IS NOT NULL
               AND NOT EXISTS(SELECT 1 FROM discussion_drafts draft WHERE draft.message_id = m.id)
             ON CONFLICT(attempt_id, item_id) DO NOTHING",
            )?
            .execute(params![attempt, thread, turn, id, text])?;
        Ok(())
    }

    pub(in crate::store) fn discussion_progress_on_page(
        &self,
        chat: &str,
        oldest: i64,
        before: i64,
    ) -> Result<HashMap<i64, Vec<DiscussionProgress>>> {
        let mut statement = self.db.prepare_cached(
            "SELECT m.seq, a.id, p.item_id, p.text FROM chat_messages m
             JOIN router_inputs i ON i.message_seq = m.seq AND i.chat_id = m.chat_id
               AND i.report_id IS NULL AND i.attention_id IS NULL AND i.external_batch IS NULL
             JOIN router_attempts a ON a.seq = i.seq
             JOIN discussion_progress p ON p.attempt_id = a.id
             WHERE m.chat_id = ?1 AND m.seq >= ?2 AND m.seq < ?3
             ORDER BY m.seq, a.number, p.ordinal",
        )?;
        let rows = statement.query_map(params![chat, oldest, before], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                DiscussionProgress {
                    attempt_id: row.get(1)?,
                    item_id: row.get(2)?,
                    text: row.get(3)?,
                },
            ))
        })?;
        let mut progress = HashMap::<_, Vec<_>>::new();
        for row in rows {
            let (seq, item) = row?;
            progress.entry(seq).or_default().push(item);
        }
        Ok(progress)
    }
}

fn commentary(item: &Value) -> Option<(&str, &str)> {
    if item["type"] != "agentMessage"
        || item["phase"] != "commentary"
        || item["moderation"]["moderated"] == true
        || matches!(
            item["moderation"]["display"]["mode"].as_str(),
            Some("hide" | "block")
        )
    {
        return None;
    }
    let id = item["id"].as_str().filter(|id| !id.trim().is_empty())?;
    let text = item["text"].as_str()?.trim();
    if text.is_empty()
        || text.starts_with('{')
        || text.lines().any(|line| {
            let line = line.trim_start();
            line.starts_with("```") || line.starts_with("~~~")
        })
        || (text.starts_with('[') && matches!(serde_json::from_str(text), Ok(Value::Array(_))))
    {
        return None;
    }
    Some((id, text))
}
