use super::{
    Store,
    discussion::{self, Change},
    timing,
};
use anyhow::{Result, ensure};
use protocol::{
    DiscussionQuickHandoff, DiscussionQuickRequest, DiscussionReference, DiscussionSource,
    FeishuAssessment, FeishuDisposition, FeishuEvidence, FeishuSuggestionEvidence,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const RETENTION: i64 = 7 * 24 * 60 * 60;

pub fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS feishu_scan (
            owner TEXT PRIMARY KEY, checkpoint_at INTEGER, last_success_at INTEGER,
            phase TEXT NOT NULL, gap TEXT
         );
         CREATE TABLE IF NOT EXISTS feishu_suggestions (
            id TEXT PRIMARY KEY, owner TEXT NOT NULL, chat_id TEXT NOT NULL, topic TEXT,
            first_at INTEGER NOT NULL, last_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
            checked_at INTEGER NOT NULL DEFAULT 0, assessed_at INTEGER NOT NULL DEFAULT 0,
            ignored_material_revision INTEGER, material_revision INTEGER NOT NULL DEFAULT 1,
            context_cursor TEXT, evidence TEXT NOT NULL, assessment_state TEXT NOT NULL DEFAULT 'pending'
         );
         CREATE INDEX IF NOT EXISTS feishu_suggestions_topic ON feishu_suggestions(owner,chat_id,topic,last_at);
         CREATE INDEX IF NOT EXISTS feishu_suggestions_recent ON feishu_suggestions(owner,updated_at DESC,id DESC);
         CREATE INDEX IF NOT EXISTS feishu_suggestions_context ON feishu_suggestions(owner,checked_at);
         CREATE INDEX IF NOT EXISTS feishu_suggestions_expiry ON feishu_suggestions(first_at);
         CREATE INDEX IF NOT EXISTS feishu_suggestions_assessment ON feishu_suggestions(owner,assessment_state,assessed_at);
         CREATE TABLE IF NOT EXISTS feishu_mentions (
            owner TEXT NOT NULL, message_id TEXT NOT NULL, suggestion_id TEXT NOT NULL REFERENCES feishu_suggestions(id) ON DELETE CASCADE,
            created_at INTEGER NOT NULL, PRIMARY KEY(owner,message_id)
         );
         CREATE INDEX IF NOT EXISTS feishu_mentions_by_suggestion ON feishu_mentions(suggestion_id);
         CREATE INDEX IF NOT EXISTS feishu_mentions_expiry ON feishu_mentions(created_at);
         CREATE TABLE IF NOT EXISTS feishu_discussion_snapshots (
            chat_id TEXT NOT NULL REFERENCES discussions(chat_id), suggestion_id TEXT NOT NULL,
            revision INTEGER NOT NULL, evidence TEXT NOT NULL,
            PRIMARY KEY(chat_id,suggestion_id,revision)
         );
         CREATE TABLE IF NOT EXISTS feishu_discussion_links (
            owner TEXT NOT NULL, suggestion_id TEXT NOT NULL, chat_id TEXT NOT NULL REFERENCES discussions(chat_id),
            revision INTEGER NOT NULL, is_default INTEGER NOT NULL,
            PRIMARY KEY(owner,suggestion_id,chat_id)
         );
         CREATE UNIQUE INDEX IF NOT EXISTS feishu_discussion_new ON feishu_discussion_links(owner,suggestion_id) WHERE is_default=1;
         CREATE TABLE IF NOT EXISTS feishu_handoff_bridges (
            owner TEXT NOT NULL, suggestion_id TEXT NOT NULL, revision INTEGER NOT NULL,
            chat_id TEXT NOT NULL UNIQUE REFERENCES discussions(chat_id),
            PRIMARY KEY(owner,suggestion_id,revision)
         );",
    )?;
    Ok(())
}

#[derive(Clone)]
pub struct FeishuMessage {
    pub chat_id: String,
    pub chat_name: String,
    pub thread_id: Option<String>,
    pub root_id: Option<String>,
    pub parent_id: Option<String>,
    pub evidence: FeishuEvidence,
    pub deleted: bool,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct ContextCursor {
    pub from: i64,
    pub through: i64,
    pub token: String,
    pub pages: usize,
    pub complete: bool,
    pub known: Vec<String>,
}

pub struct ContextResult {
    pub replies: Vec<FeishuEvidence>,
    pub complete: bool,
    pub next: Option<ContextCursor>,
}

pub struct ContextJob {
    pub id: String,
    pub chat_id: String,
    pub topic: Option<String>,
    pub first_at: i64,
    pub cursor: Option<ContextCursor>,
    pub evidence: FeishuSuggestionEvidence,
}

fn read_evidence(
    db: &Connection,
    owner: &str,
    id: &str,
) -> Result<Option<FeishuSuggestionEvidence>> {
    db.query_row(
        "SELECT evidence FROM feishu_suggestions WHERE owner=?1 AND id=?2",
        [owner, id],
        |r| r.get::<_, String>(0),
    )
    .optional()?
    .map(|s| Ok(serde_json::from_str(&s)?))
    .transpose()
}

fn save_changed(
    db: &Connection,
    evidence: &mut FeishuSuggestionEvidence,
    now: i64,
    new_material: bool,
) -> Result<()> {
    evidence.revision += 1;
    evidence.assessment = None;
    db.execute(
        "UPDATE feishu_suggestions SET evidence=?2,updated_at=?3,material_revision=material_revision+?4,assessment_state='pending',assessed_at=0 WHERE id=?1",
        params![evidence.id, serde_json::to_string(evidence)?, now, new_material],
    )?;
    Ok(())
}

fn add_mention(db: &Connection, owner: &str, m: &FeishuMessage, now: i64) -> Result<()> {
    let e = &m.evidence;
    let recorded = db
        .query_row(
            "SELECT suggestion_id FROM feishu_mentions WHERE owner=?1 AND message_id=?2",
            params![owner, e.id],
            |r| r.get::<_, String>(0),
        )
        .optional()?;
    if let Some(id) = recorded {
        let mut saved = read_evidence(db, owner, &id)?.expect("mention foreign key");
        if let Some(original) = saved.mentions.iter_mut().find(|old| old.id == e.id)
            && original.updated_at <= e.updated_at
            && *original != *e
        {
            *original = e.clone();
            if saved.anchor.id == e.id {
                saved.anchor = e.clone();
            }
            db.execute(
                "UPDATE feishu_suggestions SET checked_at=0,context_cursor=NULL WHERE id=?1",
                [&id],
            )?;
            save_changed(db, &mut saved, now, true)?;
        }
        return Ok(());
    }
    let known_reply = m.root_id.as_ref().or(m.parent_id.as_ref());
    let topic = m.thread_id.as_ref().or(known_reply);
    let mut matching = None;
    if let Some(thread) = &m.thread_id {
        matching = db
            .query_row(
                "SELECT id FROM feishu_suggestions WHERE owner=?1 AND chat_id=?2 AND topic=?3
             ORDER BY last_at DESC LIMIT 1",
                params![owner, m.chat_id, thread],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
    }
    // Follow already recorded source identities in the same chat, not merely
    // the topic string. A reply to the second mention still belongs to the
    // first mention's suggestion even after a topic key has been established.
    for parent in [m.parent_id.as_ref(), m.root_id.as_ref()]
        .into_iter()
        .flatten()
    {
        if matching.is_some() {
            break;
        }
        let previous = db.query_row(
            "SELECT s.id FROM feishu_suggestions s
               WHERE s.owner=?1 AND s.chat_id=?3
                 AND (?4 IS NULL OR s.topic IS NULL OR substr(s.topic,1,4)!='omt_' OR s.topic=?4)
                 AND (s.id=(SELECT suggestion_id FROM feishu_mentions WHERE owner=?1 AND message_id=?2)
                      OR EXISTS(SELECT 1 FROM json_each(s.evidence,'$.replies') r WHERE json_extract(r.value,'$.id')=?2))
               ORDER BY s.last_at DESC,s.id LIMIT 1",
            params![owner,parent,m.chat_id,m.thread_id], |r| r.get::<_,String>(0),
        ).optional()?;
        if let Some(id) = previous {
            db.execute(
                "UPDATE feishu_suggestions SET topic=?2
                     WHERE id=?1 AND (topic IS NULL OR (?3 IS NOT NULL AND substr(topic,1,4)!='omt_'))",
                params![id,topic,m.thread_id],
            )?;
            matching = Some(id);
        }
    }
    if matching.is_none() {
        matching =
            if let Some(topic) = topic {
                db.query_row(
                    "SELECT id FROM feishu_suggestions WHERE owner=?1 AND chat_id=?2 AND topic=?3
                 ORDER BY last_at DESC LIMIT 1",
                    params![owner, m.chat_id, topic],
                    |r| r.get::<_, String>(0),
                )
                .optional()?
            } else {
                db.query_row(
                "SELECT id FROM feishu_suggestions WHERE owner=?1 AND chat_id=?2 AND topic IS NULL
                 AND last_at BETWEEN ?3-1800 AND ?3+1800 AND first_at BETWEEN ?3-7200 AND ?3+7200
                 AND MAX(last_at,?3)-MIN(first_at,?3)<=7200
                 ORDER BY last_at DESC,id DESC LIMIT 1",
                params![owner,m.chat_id,e.created_at], |r| r.get::<_,String>(0),
            ).optional()?
            };
    }
    let id = if let Some(id) = matching {
        let mut saved = read_evidence(db, owner, &id)?.expect("selected suggestion");
        saved.mentions.push(e.clone());
        saved
            .mentions
            .sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
        if saved.mentions.len() > 12 {
            saved.mentions.remove(0);
        }
        if (e.created_at, &e.id) < (saved.anchor.created_at, &saved.anchor.id) {
            saved.anchor = e.clone();
        }
        if !m.chat_name.is_empty() {
            saved.chat_name.clone_from(&m.chat_name);
        }
        saved.replies.retain(|old| old.id != e.id);
        db.execute(
            "UPDATE feishu_suggestions SET first_at=MIN(first_at,?2),last_at=MAX(last_at,?2),checked_at=0,context_cursor=NULL WHERE id=?1",
            params![id,e.created_at],
        )?;
        save_changed(db, &mut saved, now, true)?;
        id
    } else {
        let id = protocol::new_id();
        let saved = FeishuSuggestionEvidence {
            id: id.clone(),
            revision: 1,
            chat_name: m.chat_name.clone(),
            anchor: e.clone(),
            mentions: vec![e.clone()],
            replies: Vec::new(),
            context_status: "pending".into(),
            assessment: None,
        };
        db.execute(
            "INSERT INTO feishu_suggestions(id,owner,chat_id,topic,first_at,last_at,updated_at,evidence)
             VALUES(?1,?2,?3,?4,?5,?5,?6,?7)",
            params![id,owner,m.chat_id,topic,e.created_at,now,serde_json::to_string(&saved)?],
        )?;
        id
    };
    db.execute(
        "INSERT INTO feishu_mentions VALUES (?1,?2,?3,?4)",
        params![owner, e.id, id, e.created_at],
    )?;
    Ok(())
}

impl Store {
    pub fn feishu_epoch(&self, value: &str) -> Result<Option<i64>> {
        Ok(self
            .db
            .query_row("SELECT unixepoch(?1)", [value], |r| r.get(0))?)
    }

    pub fn feishu_iso(&self, epoch: i64) -> Result<String> {
        Ok(self.db.query_row(
            "SELECT strftime('%Y-%m-%dT%H:%M:%S+00:00',?1,'unixepoch')",
            [epoch],
            |r| r.get(0),
        )?)
    }

    pub fn feishu_checkpoint(&self, owner: &str) -> Result<Option<i64>> {
        Ok(self
            .db
            .query_row(
                "SELECT checkpoint_at FROM feishu_scan WHERE owner=?1",
                [owner],
                |r| r.get(0),
            )
            .optional()?
            .flatten())
    }

    pub fn interrupt_feishu_scan(&self, owner: &str) -> Result<()> {
        self.db.execute("UPDATE feishu_scan SET phase='interrupted',gap='interrupted' WHERE owner=?1 AND phase='running'", [owner])?;
        Ok(())
    }

    pub fn feishu_scan_phase(&self, owner: &str, phase: &str, gap: Option<&str>) -> Result<()> {
        self.db.execute(
            "INSERT INTO feishu_scan(owner,phase,gap) VALUES(?1,?2,?3)
             ON CONFLICT(owner) DO UPDATE SET phase=excluded.phase,gap=COALESCE(excluded.gap,feishu_scan.gap)",
            params![owner,phase,gap],
        )?;
        Ok(())
    }

    pub fn prune_feishu(&mut self, now: i64) -> Result<()> {
        let cutoff = now - RETENTION;
        let due: bool = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM feishu_suggestions WHERE first_at<?1)
                    OR EXISTS(SELECT 1 FROM feishu_mentions WHERE created_at<?1)",
            [cutoff],
            |r| r.get(0),
        )?;
        if !due {
            return Ok(());
        }
        let (tx, clock) = timing::begin(&mut self.db, "feishu_retention")?;
        tx.execute("DELETE FROM feishu_suggestions WHERE last_at<?1", [cutoff])?;
        let aged = tx
            .prepare("SELECT id,evidence FROM feishu_suggestions WHERE first_at<?1")?
            .query_map([cutoff], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (id, raw) in aged {
            let mut saved: FeishuSuggestionEvidence = serde_json::from_str(&raw)?;
            saved.mentions.retain(|m| m.created_at >= cutoff);
            saved.replies.retain(|m| m.created_at >= cutoff);
            if let Some(anchor) = saved.mentions.first() {
                saved.anchor = anchor.clone();
                tx.execute("UPDATE feishu_suggestions SET first_at=?2,context_cursor=NULL,checked_at=0 WHERE id=?1",
                    params![id,anchor.created_at])?;
                save_changed(&tx, &mut saved, now, false)?;
            } else {
                tx.execute("DELETE FROM feishu_suggestions WHERE id=?1", [id])?;
            }
        }
        tx.execute("DELETE FROM feishu_mentions WHERE created_at<?1", [cutoff])?;
        timing::commit(tx, clock)?;
        Ok(())
    }

    pub fn record_feishu_window(
        &mut self,
        owner: &str,
        through: i64,
        now: i64,
        final_window: bool,
        messages: &mut [FeishuMessage],
    ) -> Result<()> {
        let (tx, clock) = timing::begin(&mut self.db, "feishu_scan_window")?;
        let cutoff = now - RETENTION;
        messages.sort_by(|a, b| {
            (a.evidence.created_at, &a.evidence.id).cmp(&(b.evidence.created_at, &b.evidence.id))
        });
        for message in messages
            .iter()
            .filter(|m| m.evidence.created_at >= cutoff && !m.deleted)
        {
            add_mention(&tx, owner, message, now)?;
        }
        tx.execute(
            "INSERT INTO feishu_scan(owner,checkpoint_at,last_success_at,phase,gap) VALUES(?1,?2,?3,?4,NULL)
             ON CONFLICT(owner) DO UPDATE SET checkpoint_at=MAX(COALESCE(feishu_scan.checkpoint_at,0),excluded.checkpoint_at),
                last_success_at=excluded.last_success_at,phase=excluded.phase,
                gap=CASE WHEN ?5 THEN NULL ELSE feishu_scan.gap END",
            params![owner,through,now,if final_window { "idle" } else { "running" },final_window],
        )?;
        timing::commit(tx, clock)?;
        Ok(())
    }

    pub fn feishu_context_jobs(&self, owner: &str, now: i64) -> Result<Vec<ContextJob>> {
        let mut query = self.db.prepare(
            "SELECT id,chat_id,topic,first_at,evidence,context_cursor FROM feishu_suggestions
             WHERE owner=?1 AND first_at>=?2 AND (context_cursor IS NOT NULL OR checked_at<?3)
             ORDER BY checked_at,updated_at DESC LIMIT 8",
        )?;
        query
            .query_map(params![owner, now - RETENTION, now - 150], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, Option<String>>(5)?,
                ))
            })?
            .map(|row| {
                let (id, chat_id, topic, first_at, raw, cursor) = row?;
                Ok(ContextJob {
                    id,
                    chat_id,
                    topic,
                    first_at,
                    cursor: cursor.map(|raw| serde_json::from_str(&raw)).transpose()?,
                    evidence: serde_json::from_str(&raw)?,
                })
            })
            .collect()
    }

    pub fn record_feishu_context(
        &mut self,
        owner: &str,
        job: &ContextJob,
        name: Option<&str>,
        result: Option<ContextResult>,
        now: i64,
    ) -> Result<()> {
        let (tx, clock) = timing::begin(&mut self.db, "feishu_context")?;
        let Some(mut saved) = read_evidence(&tx, owner, &job.id)? else {
            return Ok(());
        };
        // Retention or ingestion can change source material during an external read.
        // An assessor updating only its output does not invalidate this context job.
        if saved.anchor != job.evidence.anchor
            || saved.mentions != job.evidence.mentions
            || saved.replies != job.evidence.replies
        {
            return Ok(());
        }
        let old_status = saved.context_status.clone();
        let old_name = saved.chat_name.clone();
        let mut next = None;
        let changed = if let Some(result) = result {
            saved.context_status = if result.complete { "ready" } else { "partial" }.into();
            let changed = saved.replies != result.replies;
            saved.replies = result.replies;
            next = result.next;
            changed
        } else {
            // Expired or invalid continuation tokens are retried from a fresh window.
            saved.context_status = "unavailable".into();
            false
        };
        if let Some(name) = name.filter(|name| !name.is_empty()) {
            saved.chat_name = name.to_owned();
        }
        if changed {
            save_changed(&tx, &mut saved, now, true)?;
        } else if old_status != saved.context_status || old_name != saved.chat_name {
            saved.revision += 1;
        }
        let cursor = next.as_ref().map(serde_json::to_string).transpose()?;
        tx.execute(
            "UPDATE feishu_suggestions SET checked_at=?2,evidence=?3,context_cursor=?4 WHERE id=?1",
            params![job.id, now, serde_json::to_string(&saved)?, cursor],
        )?;
        if old_status != saved.context_status
            && matches!(old_status.as_str(), "partial" | "unavailable")
            && saved.context_status == "ready"
            && !changed
            && saved
                .assessment
                .as_ref()
                .is_none_or(|a| a.disposition != FeishuDisposition::Resolved)
        {
            tx.execute("UPDATE feishu_suggestions SET assessment_state='pending',assessed_at=0 WHERE id=?1", [&job.id])?;
        }
        timing::commit(tx, clock)?;
        Ok(())
    }

    pub fn feishu_assessment_job(
        &self,
        owner: &str,
        now: i64,
    ) -> Result<Option<FeishuSuggestionEvidence>> {
        self.db.query_row(
            "SELECT evidence FROM feishu_suggestions WHERE owner=?1 AND first_at>=?2 AND checked_at>0 AND context_cursor IS NULL
             AND (assessment_state='pending' OR (assessment_state='failed' AND assessed_at<=?3))
             ORDER BY assessed_at,updated_at DESC,id LIMIT 1",
            params![owner, now-RETENTION, now-300], |r| r.get::<_,String>(0),
        ).optional()?.map(|s| Ok(serde_json::from_str(&s)?)).transpose()
    }

    pub fn record_feishu_assessment(
        &mut self,
        owner: &str,
        job: &FeishuSuggestionEvidence,
        result: Option<FeishuAssessment>,
        now: i64,
    ) -> Result<()> {
        let (tx, clock) = timing::begin(&mut self.db, "feishu_assessment")?;
        let Some(mut saved) = read_evidence(&tx, owner, &job.id)? else {
            return Ok(());
        };
        if saved.revision != job.revision || saved.context_status != job.context_status {
            return Ok(());
        }
        let state = if result.is_some() {
            "assessed"
        } else {
            "failed"
        };
        if saved.assessment != result {
            saved.revision += 1;
            saved.assessment = result;
        }
        tx.execute("UPDATE feishu_suggestions SET evidence=?2,assessment_state=?3,assessed_at=?4 WHERE id=?1",
            params![job.id,serde_json::to_string(&saved)?,state,now])?;
        timing::commit(tx, clock)?;
        Ok(())
    }

    pub fn feishu_status(&self, owner: Option<&str>) -> Result<Value> {
        let Some(owner) = owner else {
            return Ok(json!({"phase":"disabled"}));
        };
        Ok(self.db.query_row(
            "SELECT phase,checkpoint_at,last_success_at,gap FROM feishu_scan WHERE owner=?1", [owner], |r| Ok(json!({
                "phase":r.get::<_,String>(0)?,"checkpoint_at":r.get::<_,Option<i64>>(1)?,
                "last_success_at":r.get::<_,Option<i64>>(2)?,"gap":r.get::<_,Option<String>>(3)?,
            })),
        ).optional()?.unwrap_or_else(|| json!({"phase":"starting"})))
    }

    pub fn feishu_list(&self, owner: &str, before: Option<(i64, &str)>) -> Result<Value> {
        let (at, id) = before.unwrap_or((i64::MAX, ""));
        let mut stmt = self.db.prepare(
            "SELECT evidence,updated_at,ignored_material_revision,assessment_state,material_revision FROM feishu_suggestions
             WHERE owner=?1 AND first_at>=unixepoch()-?2 AND (updated_at,id)<(?3,?4)
             ORDER BY updated_at DESC,id DESC LIMIT 51",
        )?;
        let mut rows = stmt.query(params![owner, RETENTION, at, id])?;
        let mut items = Vec::new();
        let mut has_more = false;
        while let Some(row) = rows.next()? {
            if items.len() == 50 {
                has_more = true;
                break;
            }
            let item = self.feishu_card(owner, row)?;
            items.push(item);
        }
        let mut next = None;
        if has_more && let Some(last) = items.last() {
            next = Some(format!(
                "{}:{}",
                last["updated_at"].as_i64().unwrap_or_default(),
                last["evidence"]["id"].as_str().unwrap_or_default()
            ));
        }
        Ok(json!({"suggestions":items,"next_before":next}))
    }

    pub fn feishu_detail(&self, owner: &str, id: &str) -> Result<Option<Value>> {
        let mut stmt = self.db.prepare(
            "SELECT evidence,updated_at,ignored_material_revision,assessment_state,material_revision FROM feishu_suggestions
             WHERE owner=?1 AND id=?2 AND first_at>=unixepoch()-?3",
        )?;
        let mut rows = stmt.query(params![owner, id, RETENTION])?;
        rows.next()?
            .map(|row| self.feishu_card(owner, row))
            .transpose()
    }

    fn feishu_card(&self, owner: &str, row: &rusqlite::Row<'_>) -> Result<Value> {
        let evidence: FeishuSuggestionEvidence = serde_json::from_str(&row.get::<_, String>(0)?)?;
        let ignored = row.get::<_, Option<i64>>(2)? == Some(row.get::<_, i64>(4)?);
        let mut query = self.db.prepare(
            "SELECT l.chat_id,l.revision,d.title,d.archived,
                 EXISTS(SELECT 1 FROM discussion_references r WHERE r.chat_id=l.chat_id AND r.id=('feishu:'||l.suggestion_id))
               FROM feishu_discussion_links l JOIN discussions d ON d.chat_id=l.chat_id
              WHERE l.owner=?1 AND l.suggestion_id=?2 ORDER BY d.updated_at DESC",
        )?;
        let discussions = query.query_map(params![owner,evidence.id], |r| Ok(json!({
            "chat_id":r.get::<_,String>(0)?,"revision":r.get::<_,i64>(1)?,"title":r.get::<_,String>(2)?,
            "archived":r.get::<_,bool>(3)?,"attached":r.get::<_,bool>(4)?
        })))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut handoffs = Vec::new();
        let mut query = self.db.prepare(
            "SELECT revision,chat_id FROM feishu_handoff_bridges WHERE owner=?1 AND suggestion_id=?2 ORDER BY revision DESC",
        )?;
        for row in query.query_map(params![owner, evidence.id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })? {
            let (revision, chat) = row?;
            handoffs.push(json!({"revision":revision,"handoff":discussion::quick_status(&self.db,&chat,&bridge_operation(&chat))?}));
        }
        Ok(json!({
            "evidence":evidence,"updated_at":row.get::<_,i64>(1)?,"ignored":ignored,
            "assessment_status":row.get::<_,String>(3)?,"discussions":discussions,"handoffs":handoffs,
        }))
    }

    pub fn feishu_ignore(
        &mut self,
        owner: &str,
        id: &str,
        revision: i64,
        ignored: bool,
    ) -> Result<Change<()>> {
        let (tx, clock) = timing::begin(&mut self.db, "feishu_ignore")?;
        match current(&tx, owner, id, revision)? {
            Change::Ok(_) => (),
            Change::Missing => return Ok(Change::Missing),
            _ => return Ok(Change::Conflict),
        };
        tx.execute(
            "UPDATE feishu_suggestions SET ignored_material_revision=CASE WHEN ?2 THEN material_revision ELSE NULL END WHERE id=?1",
            params![id, ignored],
        )?;
        timing::commit(tx, clock)?;
        Ok(Change::Ok(()))
    }

    pub fn feishu_discuss(
        &mut self,
        owner: &str,
        id: &str,
        revision: i64,
        chat: Option<&str>,
    ) -> Result<Change<String>> {
        let (tx, clock) = timing::begin(&mut self.db, "feishu_discuss")?;
        let existing = match chat {
            Some(chat) => Some(chat.to_owned()),
            None => tx.query_row(
                "SELECT chat_id FROM feishu_discussion_links WHERE owner=?1 AND suggestion_id=?2 AND is_default=1",
                [owner,id], |r| r.get(0),
            ).optional()?,
        };
        // An exact retry can read the existing association after the selection changed.
        if let Some(existing) = &existing {
            let recorded: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM feishu_discussion_links l JOIN feishu_discussion_snapshots s
                   ON s.chat_id=l.chat_id AND s.suggestion_id=l.suggestion_id
                   WHERE l.owner=?1 AND l.suggestion_id=?2 AND l.chat_id=?3 AND s.revision=?4)",
                params![owner, id, existing, revision],
                |r| r.get(0),
            )?;
            if recorded {
                return Ok(Change::Ok(existing.clone()));
            }
        }
        let evidence = match current(&tx, owner, id, revision)? {
            Change::Ok(e) => e,
            Change::Missing => return Ok(Change::Missing),
            _ => return Ok(Change::Conflict),
        };
        let target = if let Some(chat) = existing {
            let visible: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM discussions d WHERE d.chat_id=?1
                   AND NOT EXISTS(SELECT 1 FROM feishu_handoff_bridges b WHERE b.chat_id=d.chat_id))",
                [&chat], |r| r.get(0),
            )?;
            if !visible {
                return Ok(Change::Missing);
            }
            chat
        } else {
            insert_chat(&tx, &evidence)?
        };
        attach(&tx, &target, &evidence)?;
        tx.execute(
            "INSERT INTO feishu_discussion_links VALUES(?1,?2,?3,?4,?5)
             ON CONFLICT(owner,suggestion_id,chat_id) DO UPDATE SET revision=excluded.revision",
            params![owner, id, target, revision, chat.is_none()],
        )?;
        timing::commit(tx, clock)?;
        Ok(Change::Ok(target))
    }

    pub fn feishu_handoff(
        &mut self,
        owner: &str,
        id: &str,
        revision: i64,
        retry_generation: Option<i64>,
    ) -> Result<Change<DiscussionQuickHandoff>> {
        let (tx, clock) = timing::begin(&mut self.db, "feishu_handoff")?;
        let chat = tx.query_row(
            "SELECT chat_id FROM feishu_handoff_bridges WHERE owner=?1 AND suggestion_id=?2 AND revision=?3",
            params![owner,id,revision], |r| r.get::<_,String>(0),
        ).optional()?;
        let chat = if let Some(chat) = chat {
            chat
        } else {
            if retry_generation.is_some() {
                return Ok(Change::Conflict);
            }
            let evidence = match current(&tx, owner, id, revision)? {
                Change::Ok(e) => e,
                Change::Missing => return Ok(Change::Missing),
                _ => return Ok(Change::Conflict),
            };
            let chat = insert_chat(&tx, &evidence)?;
            attach(&tx, &chat, &evidence)?;
            tx.execute(
                "INSERT INTO feishu_handoff_bridges VALUES(?1,?2,?3,?4)",
                params![owner, id, revision, chat],
            )?;
            chat
        };
        let receipt = discussion::request_quick_in_tx(
            &tx,
            &chat,
            &DiscussionQuickRequest {
                operation_id: bridge_operation(&chat),
                retry_generation,
            },
        )?;
        if matches!(receipt, Change::Ok(_)) {
            timing::commit(tx, clock)?;
        }
        Ok(receipt)
    }

    pub fn feishu_handoff_receipt(
        &self,
        owner: &str,
        id: &str,
        revision: i64,
    ) -> Result<Option<DiscussionQuickHandoff>> {
        self.db.query_row(
            "SELECT chat_id FROM feishu_handoff_bridges WHERE owner=?1 AND suggestion_id=?2 AND revision=?3",
            params![owner,id,revision], |r| r.get::<_,String>(0),
        ).optional()?.map(|chat| discussion::quick_status(&self.db,&chat,&bridge_operation(&chat))).transpose()
    }

    pub fn feishu_discussion_snapshot(
        &self,
        chat: &str,
        id: &str,
        revision: i64,
    ) -> Result<Option<Value>> {
        self.db.query_row(
            "SELECT evidence FROM feishu_discussion_snapshots WHERE chat_id=?1 AND suggestion_id=?2 AND revision=?3",
            params![chat,id,revision], |r| r.get::<_,String>(0),
        ).optional()?.map(|raw| Ok(serde_json::from_str(&raw)?)).transpose()
    }
}

fn current(
    db: &Connection,
    owner: &str,
    id: &str,
    revision: i64,
) -> Result<Change<FeishuSuggestionEvidence>> {
    let Some(raw) = db.query_row(
        "SELECT evidence FROM feishu_suggestions WHERE owner=?1 AND id=?2 AND first_at>=unixepoch()-?3",
        params![owner,id,RETENTION], |r| r.get::<_,String>(0),
    ).optional()? else { return Ok(Change::Missing) };
    let evidence: FeishuSuggestionEvidence = serde_json::from_str(&raw)?;
    Ok(if evidence.revision == revision {
        Change::Ok(evidence)
    } else {
        Change::Conflict
    })
}

fn bridge_operation(chat: &str) -> String {
    format!("discussion-handoff-{chat}")
}

fn insert_chat(db: &Connection, e: &FeishuSuggestionEvidence) -> Result<String> {
    let chat = protocol::new_id();
    let operation = format!("feishu-discussion-{chat}");
    let title = format!(
        "飞书 · {}",
        e.assessment
            .as_ref()
            .map_or(e.chat_name.as_str(), |a| a.headline.as_str())
    )
    .chars()
    .take(120)
    .collect::<String>();
    db.execute(
        "INSERT INTO discussions(chat_id,operation_id,creation,title,origin,title_source)
         VALUES(?1,?2,'{}',?3,?4,'manual')",
        params![
            chat,
            operation,
            title,
            json!({"label":"飞书建议","href":format!("/feishu-suggestions?id={}",e.id)})
                .to_string()
        ],
    )?;
    Ok(chat)
}

fn attach(db: &Connection, chat: &str, e: &FeishuSuggestionEvidence) -> Result<()> {
    let reference = DiscussionReference {
        id: format!("feishu:{}", e.id),
        source: DiscussionSource::FeishuSuggestion {
            suggestion_id: e.id.clone(),
            revision: e.revision,
        },
        label: "飞书原文".into(),
        href: Some(e.anchor.href.clone()),
    };
    ensure!(e.revision > 0, "invalid evidence version");
    db.execute(
        "INSERT OR IGNORE INTO feishu_discussion_snapshots VALUES(?1,?2,?3,?4)",
        params![chat, e.id, e.revision, serde_json::to_string(e)?],
    )?;
    db.execute(
        "INSERT INTO discussion_references(chat_id,id,reference) VALUES(?1,?2,?3)
         ON CONFLICT(chat_id,id) DO UPDATE SET reference=excluded.reference",
        params![chat, reference.id, serde_json::to_string(&reference)?],
    )?;
    Ok(())
}
