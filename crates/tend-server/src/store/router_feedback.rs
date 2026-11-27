use super::Store;
use anyhow::Result;
use codex_app::ToolCall;
use protocol::Receipt;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

pub(super) fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS router_chat_feedback (
            message_seq INTEGER PRIMARY KEY REFERENCES chat_messages(seq),
            issue_id TEXT REFERENCES issues(id),
            progress_input INTEGER UNIQUE REFERENCES router_inputs(seq),
            CHECK ((issue_id IS NULL) <> (progress_input IS NULL))
         );",
    )?;
    Ok(())
}

// The caller owns the dispatch or relay transaction and its replay identity.
// A committed acceptance does not imply a Runner claimed the work.
pub(super) fn accept_work(db: &Connection, source: &str, issue: &str, text: &str) -> Result<()> {
    if let Some((seq, _)) = insert_reply(db, source, text)? {
        db.execute(
            "INSERT INTO router_chat_feedback(message_seq, issue_id) VALUES (?1, ?2)",
            params![seq, issue],
        )?;
    }
    Ok(())
}

fn insert_reply(db: &Connection, source: &str, text: &str) -> Result<Option<(i64, String)>> {
    Ok(db
        .query_row(
            "INSERT INTO chat_messages(id, chat_id, text, reply_to)
             SELECT ?2, chat_id, ?3, id FROM chat_messages WHERE id=?1 AND chat_id='main'
             RETURNING seq, id",
            params![source, protocol::new_id(), text],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?)
}

impl Store {
    pub fn has_work_acceptance(&self, input: i64) -> Result<bool> {
        Ok(self.db.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM router_inputs i JOIN chat_messages original ON original.seq=i.message_seq
                JOIN chat_messages reply ON reply.chat_id=original.chat_id AND reply.seq>original.seq
                 AND reply.reply_to=original.id
                JOIN router_chat_feedback f ON f.message_seq=reply.seq AND f.issue_id IS NOT NULL
                WHERE i.seq=?1 AND i.chat_id='main'
                  AND i.report_id IS NULL AND i.attention_id IS NULL AND i.external_batch IS NULL
             )",
            [input],
            |row| row.get(0),
        )?)
    }

    pub fn publish_router_progress(
        &mut self,
        attempt: &str,
        call: &ToolCall,
        text: &str,
    ) -> Result<Option<Receipt>> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let input: Option<(i64, String)> = tx
            .query_row(
                "SELECT i.seq, m.id FROM router_attempts a JOIN router_inputs i ON i.seq=a.seq
                 JOIN chat_messages m ON m.seq=i.message_seq
                 WHERE a.id=?1 AND a.thread_id=?2 AND a.turn_id=?3 AND i.outcome IS NULL
                   AND i.chat_id='main' AND m.chat_id=i.chat_id
                   AND i.report_id IS NULL AND i.attention_id IS NULL AND i.external_batch IS NULL
                   AND NOT EXISTS(SELECT 1 FROM discussion_handoffs h WHERE h.message_id=m.id)",
                params![attempt, call.thread_id, call.turn_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((input, source)) = input else {
            return Ok(None);
        };
        if let Some(message_id) = tx
            .query_row(
                "SELECT m.id FROM router_chat_feedback f JOIN chat_messages m ON m.seq=f.message_seq
                 WHERE f.progress_input=?1",
                [input],
                |row| row.get(0),
            )
            .optional()?
        {
            return Ok(Some(Receipt { message_id }));
        }
        let Some((seq, message_id)) = insert_reply(&tx, &source, text)? else {
            return Ok(None);
        };
        tx.execute(
            "INSERT INTO router_chat_feedback(message_seq, progress_input) VALUES (?1, ?2)",
            params![seq, input],
        )?;
        tx.commit()?;
        Ok(Some(Receipt { message_id }))
    }
}
