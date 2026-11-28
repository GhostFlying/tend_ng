use super::{
    Store,
    discussion::{self, Change},
    timing,
};
use crate::feishu::evidence::{canonical_mentions, in_topic, relationship_ids, sort, topic_thread};
use anyhow::{Result, ensure};
use protocol::{
    DiscussionQuickHandoff, DiscussionQuickRequest, DiscussionReference, DiscussionSource,
    FeishuAssessment, FeishuEvidence, FeishuObservation, FeishuPerson, FeishuReference,
    FeishuSuggestionEvidence,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap, HashSet};

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
         CREATE TABLE IF NOT EXISTS feishu_suggestion_aliases (
            owner TEXT NOT NULL, former_id TEXT NOT NULL,
            suggestion_id TEXT NOT NULL REFERENCES feishu_suggestions(id) ON DELETE CASCADE,
            PRIMARY KEY(owner,former_id)
         );
         CREATE INDEX IF NOT EXISTS feishu_suggestion_aliases_target ON feishu_suggestion_aliases(suggestion_id);
         CREATE TABLE IF NOT EXISTS feishu_handoff_bridges (
            owner TEXT NOT NULL, suggestion_id TEXT NOT NULL, revision INTEGER NOT NULL,
            chat_id TEXT NOT NULL UNIQUE REFERENCES discussions(chat_id),
            PRIMARY KEY(owner,suggestion_id,revision)
         );",
    )?;
    Ok(())
}

pub fn migrate_source_expiry(db: &Connection) -> Result<()> {
    let installed: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('feishu_suggestions') WHERE name='aux_first_at')",
        [], |row| row.get(0),
    )?;
    if !installed {
        db.execute(
            "ALTER TABLE feishu_suggestions ADD COLUMN aux_first_at INTEGER",
            [],
        )?;
    }
    db.execute_batch(
        "CREATE INDEX IF NOT EXISTS feishu_suggestions_aux_expiry ON feishu_suggestions(aux_first_at)
            WHERE aux_first_at IS NOT NULL;",
    )?;
    Ok(())
}

fn aux_first(e: &FeishuSuggestionEvidence) -> Option<i64> {
    e.direct
        .iter()
        .chain(&e.before)
        .chain(&e.cited)
        .map(|m| m.created_at)
        .filter(|at| *at < e.anchor.created_at)
        .min()
}

fn filter_expired(e: &mut FeishuSuggestionEvidence, cutoff: i64) {
    e.direct.retain(|m| m.created_at >= cutoff);
    e.before.retain(|m| m.created_at >= cutoff);
    e.mentions.retain(|m| m.created_at >= cutoff);
    e.replies.retain(|m| m.created_at >= cutoff);
    e.related_replies
        .retain(|id| e.replies.iter().any(|m| &m.id == id));
    e.cited.retain(|m| m.created_at >= cutoff);
    for reference in &mut e.references {
        if reference.status == "available" && !e.direct.iter().any(|m| m.id == reference.id) {
            reference.status = "out_of_window".into();
        }
    }
}

fn project_active_evidence(e: &mut FeishuSuggestionEvidence, topic: Option<&str>) {
    let thread = topic_thread(topic);
    if thread.is_some() {
        e.mentions
            .retain(|m| in_topic(m, thread) || m.id == e.anchor.id);
        e.before.retain(|m| in_topic(m, thread));
        e.replies.retain(|m| in_topic(m, thread));
        e.related_replies
            .retain(|id| e.replies.iter().any(|m| &m.id == id));
        // Change badges are also part of the active evidence view.
        e.changed.retain(|id| {
            id == &e.anchor.id
                || e.mentions
                    .iter()
                    .chain(&e.replies)
                    .chain(&e.before)
                    .chain(&e.direct)
                    .chain(&e.cited)
                    .any(|m| &m.id == id)
        });
    }
    if e.assessment.is_none() {
        e.cited.clear();
    }
}

#[derive(Clone)]
pub struct FeishuMessage {
    pub chat_id: String,
    pub chat_name: String,
    pub thread_id: Option<String>,
    pub direct_mention: bool,
    pub evidence: FeishuEvidence,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct ContextCursor {
    pub from: i64,
    pub through: i64,
    pub token: String,
    #[serde(default)]
    pub thread: Option<String>,
    pub pages: usize,
    pub complete: bool,
    pub known: Vec<String>,
    #[serde(default)]
    pub read_from: Option<i64>,
    #[serde(default)]
    pub read_to: Option<i64>,
}

pub struct ContextResult {
    pub thread: Option<String>,
    pub replies: Vec<FeishuEvidence>,
    pub related_replies: Vec<String>,
    pub before: Vec<FeishuEvidence>,
    pub direct: Vec<FeishuEvidence>,
    pub references: Vec<FeishuReference>,
    pub verified: Vec<FeishuEvidence>,
    pub observation: FeishuObservation,
    pub proven_edges: Vec<(String, String)>,
    pub complete: bool,
    pub next: Option<ContextCursor>,
}

pub struct AssessmentResult {
    pub assessment: FeishuAssessment,
    pub cited: Vec<FeishuEvidence>,
    pub observation: FeishuObservation,
    pub references: Vec<FeishuReference>,
}

pub struct ContextJob {
    pub id: String,
    pub chat_id: String,
    pub topic: Option<String>,
    pub first_at: i64,
    pub material_revision: i64,
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

// Display names, links, provider timestamp enrichment and "last checked" are
// useful presentation, but do not in themselves change a judgment or ignore.
fn equivalent(a: &FeishuEvidence, b: &FeishuEvidence) -> bool {
    let same_body = match (&a.source.content_key, &b.source.content_key) {
        (Some(a), Some(b)) => a == b,
        _ => {
            canonical_mentions(&a.text, &a.source.mentions)
                == canonical_mentions(&b.text, &b.source.mentions)
        }
    };
    same_body
        && (a.source.author_id.is_none()
            || b.source.author_id.is_none()
            || a.source.author_id == b.source.author_id)
        && (a
            .source
            .mentions
            .iter()
            .map(|m| &m.id)
            .eq(b.source.mentions.iter().map(|m| &m.id))
            || (a.source.author_id.is_none() && a.source.mentions.is_empty()))
        && a.source.withdrawn == b.source.withdrawn
        && a.source.reply_to == b.source.reply_to
        && a.source.quote_ids == b.source.quote_ids
        && a.source.non_text == b.source.non_text
        && a.source.edit_time_unknown == b.source.edit_time_unknown
}

pub(crate) fn replace_version(old: &mut FeishuEvidence, new: &FeishuEvidence) -> bool {
    if (old.source.withdrawn && !new.source.withdrawn)
        || (!old.source.edit_time_unknown && new.source.edit_time_unknown)
    {
        return false;
    }
    if new.updated_at < old.updated_at
        && !(new.source.edited_minute_precision
            && new.updated_at.div_euclid(60) == old.updated_at.div_euclid(60))
        && !new.source.withdrawn
    {
        return false;
    }
    let mut new = new.clone();
    if !new.source.withdrawn
        && old.source.content_key.is_some()
        && old.source.content_key == new.source.content_key
        && new.source.truncated
        && (!old.source.truncated || old.text.chars().count() > new.text.chars().count())
    {
        new.text.clone_from(&old.text);
        new.source
            .mention_bindings
            .clone_from(&old.source.mention_bindings);
        new.source.truncated = old.source.truncated;
    }
    let omitted_topic = new.source.thread_id.is_none() && old.source.thread_id.is_some();
    if omitted_topic {
        // A missing field doesn't move a known message to another container.
        new.source.thread_id.clone_from(&old.source.thread_id);
    }
    if let Some(thread) = old.source.thread_id.as_deref()
        && new.source.thread_id.as_deref() == Some(thread)
    {
        let same_version = old.id == new.id
            && old.source.content_key.is_some()
            && old.source.content_key == new.source.content_key
            && old.created_at == new.created_at
            && old.updated_at == new.updated_at
            && old.source.created_millis == new.source.created_millis
            && old.source.edited_millis == new.source.edited_millis;
        // A row from another endpoint does not supersede the verified
        // location of this same version, even if its new locator is coherent.
        let old_locator = crate::feishu::locator::topic_position(&old.href, thread);
        let old_is_located = old_locator.is_some() && old_locator == old.source.position;
        if omitted_topic || (old_is_located && (same_version || new.source.position.is_none())) {
            new.source.position = old.source.position;
            new.href.clone_from(&old.href);
            new.source.chat_link_only = old.source.chat_link_only;
        }
        if same_version {
            if new.source.root_id.is_none() {
                new.source.root_id.clone_from(&old.source.root_id);
            }
            if new.source.reply_to.is_none() {
                new.source.reply_to.clone_from(&old.source.reply_to);
            }
            if !new.source.quote_metadata_reported {
                new.source.quote_ids.clone_from(&old.source.quote_ids);
                new.source.quote_metadata_reported = old.source.quote_metadata_reported;
            }
        }
    }
    let changed = !equivalent(old, &new);
    if !old.source.created_minute_precision
        && new.source.created_minute_precision
        && old.created_at.div_euclid(60) == new.created_at.div_euclid(60)
    {
        new.created_at = old.created_at;
        new.source.created_minute_precision = false;
    }
    new.source.author_id = new
        .source
        .author_id
        .or_else(|| old.source.author_id.clone());
    if new.source.withdrawn {
        // A tombstone may omit its old relation and source position. Retain
        // verified provenance, never retain its old body or mention text.
        new.source.reply_to = new.source.reply_to.or_else(|| old.source.reply_to.clone());
        new.source.quote_ids = old.source.quote_ids.clone();
        new.source.position = new.source.position.or(old.source.position);
        new.source.thread_id = new
            .source
            .thread_id
            .or_else(|| old.source.thread_id.clone());
    }
    new.updated_at = new.updated_at.max(old.updated_at);
    *old = new;
    changed
}

fn remember_changes(
    saved: &mut FeishuSuggestionEvidence,
    changed: impl IntoIterator<Item = String>,
) {
    for id in changed {
        saved.changed.retain(|old| old != &id);
        saved.changed.push(id);
    }
    saved
        .changed
        .drain(..saved.changed.len().saturating_sub(12));
}

fn save_changed(
    db: &Connection,
    evidence: &mut FeishuSuggestionEvidence,
    now: i64,
    new_material: bool,
) -> Result<()> {
    evidence.revision += 1;
    evidence.assessment = None;
    evidence.assessed_observation = None;
    evidence.assessed_references = None;
    db.execute(
        "UPDATE feishu_suggestions SET evidence=?2,updated_at=?3,material_revision=material_revision+1,assessment_state='pending',assessed_at=0,
         ignored_material_revision=CASE WHEN NOT ?4 AND ignored_material_revision=material_revision THEN material_revision+1 ELSE ignored_material_revision END,aux_first_at=?5
         WHERE id=?1",
        params![evidence.id, serde_json::to_string(evidence)?, now, new_material,aux_first(evidence)],
    )?;
    Ok(())
}

fn add_mention(db: &Connection, owner: &str, m: &FeishuMessage, now: i64) -> Result<()> {
    let e = &m.evidence;
    let recorded = db
        .query_row(
            "SELECT s.id,s.topic FROM feishu_mentions m
             JOIN feishu_suggestions s ON s.id=m.suggestion_id
             WHERE m.owner=?1 AND m.message_id=?2",
            params![owner, e.id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
        )
        .optional()?;
    if let Some((id, topic)) = recorded {
        let mut saved = read_evidence(db, owner, &id)?.expect("mention foreign key");
        let mut changed = false;
        if let Some(original) = saved.mentions.iter_mut().find(|old| old.id == e.id) {
            let before = in_topic(original, topic_thread(topic.as_deref()));
            let updated = replace_version(original, e);
            let after = in_topic(original, topic_thread(topic.as_deref()));
            changed |= before != after || (after && updated);
        }
        let mut discovered_thread = None;
        if saved.anchor.id == e.id {
            let previous = saved.anchor.source.thread_id.clone();
            changed |= replace_version(&mut saved.anchor, e);
            if saved.anchor.source.thread_id != previous {
                discovered_thread = saved.anchor.source.thread_id.clone();
            }
        }
        if changed || discovered_thread.is_some() {
            if discovered_thread.is_some() {
                saved.replies.clear();
                saved.before.clear();
                saved.direct.clear();
                saved.references.clear();
                saved.cited.clear();
                saved.changed.clear();
                saved.coverage_changed = true;
                saved.related_replies.clear();
                saved.context_status = "pending".into();
                saved.observation = FeishuObservation::default();
            }
            db.execute(
                "UPDATE feishu_suggestions SET checked_at=0,context_cursor=NULL,topic=COALESCE(?2,topic) WHERE id=?1",
                params![id,discovered_thread],
            )?;
            remember_changes(&mut saved, [e.id.clone()]);
            save_changed(db, &mut saved, now, true)?;
        } else {
            db.execute(
                "UPDATE feishu_suggestions SET evidence=?2 WHERE id=?1",
                params![id, serde_json::to_string(&saved)?],
            )?;
        }
        return Ok(());
    }
    if !m.direct_mention {
        return Ok(());
    }

    let known_reply = relationship_ids(e).next();
    let topic = m.thread_id.as_deref().or(known_reply);
    // Only a proven connection to a current source or a related reply can
    // extend an existing card. Common chat/thread/parent alone is not enough.
    let mut matching = None;
    for parent in relationship_ids(e) {
        let previous = db.query_row(
            "SELECT MIN(s.id) FROM feishu_suggestions s
               WHERE s.owner=?1 AND s.chat_id=?3
                 AND (substr(COALESCE(s.topic,''),1,4)!='omt_' OR s.topic=?4)
                 AND (s.id=(SELECT suggestion_id FROM feishu_mentions WHERE owner=?1 AND message_id=?2)
                    OR (substr(COALESCE(s.topic,''),1,4)!='omt_'
                        AND EXISTS(SELECT 1 FROM json_each(s.evidence,'$.replies') r WHERE json_extract(r.value,'$.id')=?2)))
               HAVING COUNT(*)=1",
            params![owner,parent,m.chat_id,m.thread_id], |r| r.get::<_,String>(0),
        ).optional()?;
        if let Some(id) = previous {
            if matching.as_ref().is_some_and(|old| old != &id) {
                matching = None;
                break;
            }
            matching = Some(id);
        }
    }
    let id = if let Some(id) = matching {
        let mut saved = read_evidence(db, owner, &id)?.expect("selected suggestion");
        saved.mentions.push(e.clone());
        sort(&mut saved.mentions);
        if saved.mentions.len() > 12 {
            saved.mentions.remove(0);
        }
        if (e.created_at, &e.id) < (saved.anchor.created_at, &saved.anchor.id) {
            saved.anchor = e.clone();
            saved.direct.clear();
            saved.references.clear();
            saved.before.clear();
            saved.replies.clear();
            saved.related_replies.clear();
        }
        if !m.chat_name.is_empty() {
            saved.chat_name.clone_from(&m.chat_name);
        }
        saved.replies.retain(|old| old.id != e.id);
        saved.related_replies.retain(|id| id != &e.id);
        db.execute(
            "UPDATE feishu_suggestions SET first_at=MIN(first_at,?2),last_at=MAX(last_at,?2),checked_at=0,context_cursor=NULL,
             topic=CASE WHEN ?3 IS NOT NULL THEN ?3 ELSE COALESCE(topic,?4) END WHERE id=?1",
            params![id,e.created_at,m.thread_id,topic],
        )?;
        if saved.assessment.is_some() {
            saved.changed.clear();
            saved.coverage_changed = false;
        }
        remember_changes(&mut saved, [e.id.clone()]);
        save_changed(db, &mut saved, now, true)?;
        id
    } else {
        if known_reply.is_some() {
            // The missing parent may already be a real follow-up to an older
            // card in this chat. Revisit that chat now even when its normal
            // context poll just ran. Keep any already accepted cursor.
            db.execute(
                "UPDATE feishu_suggestions SET checked_at=0 WHERE owner=?1 AND chat_id=?2
                   AND first_at>=?3 AND first_at<=?4",
                params![owner, m.chat_id, now - RETENTION, e.created_at],
            )?;
        }
        let id = protocol::new_id();
        let saved = FeishuSuggestionEvidence {
            id: id.clone(),
            revision: 1,
            chat_name: m.chat_name.clone(),
            anchor: e.clone(),
            mentions: vec![e.clone()],
            replies: Vec::new(),
            related_replies: Vec::new(),
            context_status: "pending".into(),
            assessment: None,
            owner: None,
            observation: FeishuObservation::default(),
            assessed_observation: None,
            assessed_references: None,
            coverage_changed: false,
            references: vec![],
            direct: vec![],
            before: vec![],
            changed: vec![],
            cited: vec![],
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

/// A later direct mention can have a parent that was not itself an @mention.
/// A scan may have to open a provisional card before context proves the link.
/// Fold only the provisional ordinary-reply topic actually evidenced by an
/// edge in this chat, leaving explicit thread topics and receipts untouched.
fn fold_proven_replies(
    db: &Connection,
    owner: &str,
    job: &ContextJob,
    saved: &mut FeishuSuggestionEvidence,
    scope: Option<&str>,
    edges: &[(String, String)],
) -> Result<bool> {
    let mut old_ids = BTreeSet::new();
    let mut query = db.prepare_cached(
        "SELECT s.id FROM feishu_mentions m JOIN feishu_suggestions s ON s.id=m.suggestion_id
         WHERE m.owner=?1 AND m.message_id=?2 AND s.owner=?1 AND s.chat_id=?3 AND s.id<>?4
           AND (s.topic=?5 OR (s.topic=?7 AND substr(s.topic,1,4)='omt_')) AND s.first_at>?6",
    )?;
    for (reply, parent) in edges {
        if parent.starts_with("om_")
            && let Some(id) = query
                .query_row(
                    params![
                        owner,
                        reply,
                        job.chat_id,
                        job.id,
                        parent,
                        job.first_at,
                        scope
                    ],
                    |r| r.get::<_, String>(0),
                )
                .optional()?
        {
            old_ids.insert(id);
        }
    }
    if old_ids.is_empty() {
        return Ok(false);
    }

    let mut direct: HashMap<_, _> = saved
        .mentions
        .drain(..)
        .map(|e| (e.id.clone(), e))
        .collect();
    for former in old_ids {
        let old = read_evidence(db, owner, &former)?.expect("selected Feishu source card");
        remember_changes(saved, [old.anchor.id.clone()]);
        for mention in old.mentions.into_iter().chain([old.anchor]) {
            direct.entry(mention.id.clone()).or_insert(mention);
        }
        // Do not import the provisional reply cache. It was captured against
        // a different anchor and may predate a tombstone seen by this pass.
        // The selected snapshots still retain exactly what the user saw.
        db.execute(
            "UPDATE feishu_suggestions SET last_at=MAX(last_at,(SELECT last_at FROM feishu_suggestions WHERE id=?2)) WHERE id=?1",
            params![job.id,former],
        )?;
        db.execute(
            "UPDATE feishu_mentions SET suggestion_id=?2 WHERE owner=?1 AND suggestion_id=?3",
            params![owner, job.id, former],
        )?;
        db.execute("UPDATE feishu_suggestion_aliases SET suggestion_id=?2 WHERE owner=?1 AND suggestion_id=?3",params![owner,job.id,former])?;
        db.execute(
            "INSERT INTO feishu_suggestion_aliases VALUES(?1,?2,?3)",
            params![owner, former, job.id],
        )?;
        // Discussion links, snapshots and handoff bridges are independent
        // records and continue to be addressed by the original ID/revision.
        db.execute("DELETE FROM feishu_suggestions WHERE id=?1", [former])?;
    }
    let mut mentions: Vec<_> = direct.into_values().collect();
    sort(&mut mentions);
    mentions.drain(..mentions.len().saturating_sub(12));
    saved.mentions = mentions;

    let mut direct_ids =
        db.prepare_cached("SELECT message_id FROM feishu_mentions WHERE suggestion_id=?1")?;
    let direct_ids: HashSet<_> = direct_ids
        .query_map([&job.id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    saved.replies.retain(|e| !direct_ids.contains(&e.id));
    saved.related_replies.retain(|id| !direct_ids.contains(id));
    Ok(true)
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

    pub fn feishu_known_mention(&self, owner: &str, id: &str) -> Result<bool> {
        Ok(self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM feishu_mentions WHERE owner=?1 AND message_id=?2)",
            params![owner, id],
            |r| r.get(0),
        )?)
    }

    pub fn feishu_chat_mentions(
        &self,
        owner: &str,
        chat: &str,
        id: &str,
    ) -> Result<HashSet<String>> {
        let mut stmt = self.db.prepare_cached(
            "SELECT m.message_id FROM feishu_suggestions s JOIN feishu_mentions m ON m.suggestion_id=s.id
             WHERE s.owner=?1 AND s.chat_id=?2 AND s.id<>?3"
        )?;
        Ok(stmt
            .query_map(params![owner, chat, id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
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
        self.db.execute(
            "UPDATE feishu_scan SET phase='interrupted'
             WHERE owner=?1 AND phase IN ('running','resuming','catching_up')",
            [owner],
        )?;
        Ok(())
    }

    // A running retry is not a new diagnostic outcome. Keep the last confirmed
    // gap while the phase changes, until a new failure or a full window commits.
    // A resumed scan keeps the public interruption hint in its phase, not gap.
    pub fn feishu_scan_phase(&self, owner: &str, phase: &str, failure: Option<&str>) -> Result<()> {
        self.db.execute(
            "INSERT INTO feishu_scan(owner,phase,gap) VALUES(?1,?2,?3)
             ON CONFLICT(owner) DO UPDATE SET
                phase=CASE WHEN excluded.phase='running' AND feishu_scan.phase IN ('interrupted','resuming')
                    THEN 'resuming' ELSE excluded.phase END,
                gap=COALESCE(excluded.gap,feishu_scan.gap)",
            params![owner,phase,failure],
        )?;
        Ok(())
    }

    pub fn prune_feishu(&mut self, now: i64) -> Result<()> {
        let cutoff = now - RETENTION;
        let due: bool = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM feishu_suggestions WHERE first_at<?1)
                    OR EXISTS(SELECT 1 FROM feishu_mentions WHERE created_at<?1)
                    OR EXISTS(SELECT 1 FROM feishu_suggestions WHERE aux_first_at<?1)",
            [cutoff],
            |r| r.get(0),
        )?;
        if !due {
            return Ok(());
        }
        let (tx, clock) = timing::begin(&mut self.db, "feishu_retention")?;
        tx.execute("DELETE FROM feishu_suggestions WHERE last_at<?1", [cutoff])?;
        let aged = tx
            .prepare(
                "SELECT id,evidence,topic FROM feishu_suggestions WHERE first_at<?1
                UNION SELECT id,evidence,topic FROM feishu_suggestions WHERE aux_first_at<?1",
            )?
            .query_map([cutoff], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (id, raw, topic) in aged {
            let mut saved: FeishuSuggestionEvidence = serde_json::from_str(&raw)?;
            filter_expired(&mut saved, cutoff);
            let mut anchored = saved.anchor.created_at >= cutoff;
            if !anchored {
                if let Some(anchor) = saved
                    .mentions
                    .iter()
                    .find(|m| in_topic(m, topic_thread(topic.as_deref())))
                {
                    saved.anchor = anchor.clone();
                    saved.direct.clear();
                    saved.references.clear();
                    saved.before.clear();
                    saved.related_replies.clear();
                    saved.context_status = "pending".into();
                    saved.observation = FeishuObservation::default();
                    anchored = true;
                } else {
                    tx.execute("DELETE FROM feishu_suggestions WHERE id=?1", [&id])?;
                }
            }
            if anchored {
                saved.changed.clear();
                saved.coverage_changed = false;
                tx.execute("UPDATE feishu_suggestions SET first_at=?2,context_cursor=NULL,checked_at=0 WHERE id=?1",
                    params![id,saved.anchor.created_at])?;
                save_changed(&tx, &mut saved, now, false)?;
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
        for message in messages.iter().filter(|m| m.evidence.created_at >= cutoff) {
            add_mention(&tx, owner, message, now)?;
        }
        let previous = tx
            .query_row(
                "SELECT checkpoint_at,last_success_at FROM feishu_scan WHERE owner=?1",
                [owner],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .unwrap_or_default();
        tx.execute(
            "INSERT INTO feishu_scan(owner,checkpoint_at,last_success_at,phase,gap) VALUES(?1,?2,?3,?4,NULL)
             ON CONFLICT(owner) DO UPDATE SET checkpoint_at=MAX(COALESCE(feishu_scan.checkpoint_at,0),excluded.checkpoint_at),
                last_success_at=excluded.last_success_at,phase=excluded.phase,
                gap=CASE WHEN ?5 THEN NULL ELSE feishu_scan.gap END",
            params![owner,through,now,if final_window { "idle" } else { "catching_up" },final_window],
        )?;
        super::maintenance::scan_committed(&tx, owner, previous, through, now, final_window)?;
        timing::commit(tx, clock)?;
        Ok(())
    }

    pub fn feishu_context_jobs(&self, owner: &str, now: i64) -> Result<Vec<ContextJob>> {
        let mut query = self.db.prepare(
            "SELECT id,chat_id,topic,first_at,evidence,context_cursor,material_revision FROM feishu_suggestions
             WHERE owner=?1 AND first_at>=?2 AND (context_cursor IS NOT NULL OR checked_at<?3)
             ORDER BY checked_at,updated_at DESC LIMIT 8",
        )?;
        query
            .query_map(params![owner, now - RETENTION, now - 150], context_row)?
            .map(decode_context)
            .collect()
    }

    pub fn record_feishu_context(
        &mut self,
        owner: &str,
        job: &ContextJob,
        person: &FeishuPerson,
        name: Option<&str>,
        result: Option<ContextResult>,
        now: i64,
    ) -> Result<()> {
        let (tx, clock) = timing::begin(&mut self.db, "feishu_context")?;
        let Some(mut saved) = read_evidence(&tx, owner, &job.id)? else {
            return Ok(());
        };
        if !current_material(&tx, owner, job)? {
            return Ok(());
        }
        if let Some(name) = name.filter(|name| !name.is_empty()) {
            saved.chat_name = name.to_owned();
        }
        let mut person = person.clone();
        if person.name.is_empty() {
            person.name = saved
                .anchor
                .source
                .mentions
                .iter()
                .find(|mark| mark.id == person.id && !mark.name.is_empty())
                .or_else(|| saved.owner.as_ref())
                .map(|p| p.name.clone())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| "本人（来源未提供姓名）".into());
        }
        saved.owner = Some(person);
        let mut changed = Vec::new();
        let mut next = None;
        let mut thread = None;
        let mut scope_changed = false;
        let had_observation = saved.observation.through > 0;
        let old_observation = saved
            .assessed_observation
            .as_ref()
            .unwrap_or(&saved.observation)
            .clone();
        let legacy_enrichment = saved.observation.through == 0
            && saved.assessment.as_ref().is_some_and(|a| a.card.is_none());
        let mut folded = false;
        if let Some(result) = result {
            let cutoff = now - RETENTION;
            let old: HashMap<_, _> = std::iter::once(&saved.anchor)
                .chain(&saved.mentions)
                .chain(&saved.replies)
                .chain(&saved.before)
                .chain(&saved.direct)
                .chain(&saved.cited)
                .map(|m| (m.id.clone(), m.clone()))
                .collect();
            saved.context_status = if result.complete { "ready" } else { "partial" }.into();
            for m in &result.verified {
                for original in std::iter::once(&mut saved.anchor)
                    .chain(&mut saved.mentions)
                    .filter(|original| original.id == m.id)
                {
                    replace_version(original, m);
                }
                for cited in saved.cited.iter_mut().filter(|cited| cited.id == m.id) {
                    let mut current = cited.clone();
                    let material_changed = replace_version(&mut current, m);
                    // Published citations describe that assessment. A real
                    // change invalidates it below, in this same transaction.
                    if material_changed || saved.assessment.is_none() {
                        *cited = current;
                    }
                    if material_changed {
                        changed.push(m.id.clone());
                    }
                }
            }
            saved.direct = result.direct;
            saved.before = result.before;
            saved.references = result.references;
            saved.replies = result.replies;
            saved.related_replies = result.related_replies;
            saved.observation = result.observation;
            next = result.next;
            thread = result.thread;
            scope_changed = thread.is_some() && thread.as_deref() != job.topic.as_deref();
            if scope_changed {
                saved.cited.clear();
                saved.changed.clear();
                changed.push(saved.anchor.id.clone());
            }
            let active_thread = topic_thread(thread.as_deref().or(job.topic.as_deref()));
            folded = fold_proven_replies(
                &tx,
                owner,
                job,
                &mut saved,
                thread.as_deref().or(job.topic.as_deref()),
                &result.proven_edges,
            )?;
            for m in std::iter::once(&saved.anchor)
                .chain(saved.mentions.iter().filter(|m| in_topic(m, active_thread)))
                .chain(&saved.direct)
                .chain(&saved.before)
                .chain(&saved.replies)
                .chain(&saved.cited)
                .filter(|m| m.created_at >= cutoff)
            {
                let different = if legacy_enrichment {
                    old.get(&m.id)
                        .map_or(m.created_at >= saved.anchor.created_at, |prior| {
                            prior.text != m.text || m.source.withdrawn
                        })
                } else {
                    old.get(&m.id).is_none_or(|prior| !equivalent(prior, m))
                };
                if different {
                    changed.push(m.id.clone());
                }
            }
        } else {
            saved.context_status = "unavailable".into();
            saved
                .observation
                .gaps
                .retain(|gap| gap != "source_unavailable");
            saved.observation.gaps.push("source_unavailable".into());
            saved.observation.after_status = "unavailable".into();
        }
        let coverage_completed = old_observation.through > 0
            && matches!(
                old_observation.after_status.as_str(),
                "partial" | "unavailable"
            )
            && matches!(
                saved.observation.after_status.as_str(),
                "read" | "checked_empty"
            )
            && (saved.assessment.is_none()
                || saved
                    .assessment
                    .as_ref()
                    .and_then(|a| a.card.as_ref())
                    .is_some_and(|card| {
                        card.question
                            .as_ref()
                            .is_some_and(|q| old_observation.gaps.contains(&q.basis))
                            || card.relationship == protocol::FeishuRelationship::Unclear
                    }));
        if !changed.is_empty() || folded || coverage_completed {
            if saved.assessment.is_some() {
                let folded_ids = if folded {
                    saved.changed.clone()
                } else {
                    Vec::new()
                };
                saved.changed = folded_ids;
            }
            saved.coverage_changed = coverage_completed || scope_changed;
            if had_observation {
                remember_changes(&mut saved, changed);
            }
            save_changed(&tx, &mut saved, now, true)?;
        }
        let cursor = next.as_ref().map(serde_json::to_string).transpose()?;
        tx.execute(
            "UPDATE feishu_suggestions SET checked_at=?2,evidence=?3,context_cursor=?4,aux_first_at=?5,topic=COALESCE(?6,topic) WHERE id=?1",
            params![job.id, now, serde_json::to_string(&saved)?, cursor,aux_first(&saved),thread],
        )?;
        timing::commit(tx, clock)?;
        Ok(())
    }

    pub fn feishu_assessment_job(&self, owner: &str, now: i64) -> Result<Option<ContextJob>> {
        self.db.query_row(
            "SELECT id,chat_id,topic,first_at,evidence,context_cursor,material_revision FROM feishu_suggestions
             WHERE owner=?1 AND first_at>=?2 AND checked_at>0
             AND json_extract(evidence,'$.observation.through')>0
             AND COALESCE(json_extract(evidence,'$.anchor.source.edit_time_unknown'),0)=0
             AND (assessment_state='pending' OR (assessment_state='failed' AND assessed_at<=?3))
             ORDER BY assessed_at,updated_at DESC,id LIMIT 1",
            params![owner, now-RETENTION, now-300], context_row,
        ).optional()?.map(Ok).map(decode_context).transpose()
    }

    pub fn record_feishu_assessment(
        &mut self,
        owner: &str,
        job: &ContextJob,
        result: Option<AssessmentResult>,
        now: i64,
    ) -> Result<()> {
        let (tx, clock) = timing::begin(&mut self.db, "feishu_assessment")?;
        let Some(mut saved) = read_evidence(&tx, owner, &job.id)? else {
            return Ok(());
        };
        if !current_material(&tx, owner, job)? || saved.anchor.created_at < now - RETENTION {
            return Ok(());
        }
        let state = if let Some(AssessmentResult {
            assessment,
            mut cited,
            observation,
            references,
        }) = result
        {
            cited.retain(|m| m.created_at >= now - RETENTION);
            if saved.assessment.as_ref() != Some(&assessment)
                || saved.assessed_references.as_ref() != Some(&references)
                || saved.cited != cited
            {
                saved.revision += 1;
            }
            saved.assessment = Some(assessment);
            saved.assessed_observation = Some(observation);
            saved.assessed_references = Some(references);
            saved.cited = cited;
            "assessed"
        } else {
            "failed"
        };
        tx.execute("UPDATE feishu_suggestions SET evidence=?2,assessment_state=?3,assessed_at=?4,aux_first_at=?5 WHERE id=?1",
            params![job.id,serde_json::to_string(&saved)?,state,now,aux_first(&saved)])?;
        timing::commit(tx, clock)?;
        Ok(())
    }

    pub fn feishu_status(&self, owner: Option<&str>) -> Result<Value> {
        let Some(owner) = owner else {
            return Ok(json!({"phase":"disabled"}));
        };
        Ok(self.db.query_row(
            "SELECT CASE WHEN phase IN ('catching_up','resuming') THEN 'running' ELSE phase END,
                checkpoint_at,last_success_at,
                CASE WHEN phase='catching_up' THEN 'catching_up'
                    WHEN phase IN ('interrupted','resuming') THEN 'interrupted' ELSE gap END
             FROM feishu_scan WHERE owner=?1", [owner], |r| Ok(json!({
                "phase":r.get::<_,String>(0)?,"checkpoint_at":r.get::<_,Option<i64>>(1)?,
                "last_success_at":r.get::<_,Option<i64>>(2)?,"gap":r.get::<_,Option<String>>(3)?,
            })),
        ).optional()?.unwrap_or_else(|| json!({"phase":"starting"})))
    }

    pub fn feishu_list(&self, owner: &str, before: Option<(i64, &str)>) -> Result<Value> {
        let (at, id) = before.unwrap_or((i64::MAX, ""));
        let mut stmt = self.db.prepare(
            "SELECT evidence,updated_at,ignored_material_revision,assessment_state,material_revision,topic FROM feishu_suggestions
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
            "SELECT evidence,updated_at,ignored_material_revision,assessment_state,material_revision,topic FROM feishu_suggestions
             WHERE owner=?1 AND id=COALESCE(
               (SELECT suggestion_id FROM feishu_suggestion_aliases WHERE owner=?1 AND former_id=?2),?2)
               AND first_at>=unixepoch()-?3",
        )?;
        let mut rows = stmt.query(params![owner, id, RETENTION])?;
        rows.next()?
            .map(|row| self.feishu_card(owner, row))
            .transpose()
    }

    fn feishu_card(&self, owner: &str, row: &rusqlite::Row<'_>) -> Result<Value> {
        let mut evidence: FeishuSuggestionEvidence =
            serde_json::from_str(&row.get::<_, String>(0)?)?;
        filter_expired(&mut evidence, crate::feishu::now() - RETENTION);
        let topic: Option<String> = row.get(5)?;
        project_active_evidence(&mut evidence, topic.as_deref());
        let ignored = row.get::<_, Option<i64>>(2)? == Some(row.get::<_, i64>(4)?);
        let mut aliases = self.db.prepare_cached(
            "SELECT former_id FROM feishu_suggestion_aliases WHERE owner=?1 AND suggestion_id=?2 ORDER BY former_id",
        )?;
        let aliases = aliases
            .query_map(params![owner, evidence.id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut query = self.db.prepare(
            "SELECT l.chat_id,l.revision,d.title,d.archived,
                 EXISTS(SELECT 1 FROM discussion_references r WHERE r.chat_id=l.chat_id AND r.id=('feishu:'||l.suggestion_id)),l.suggestion_id
               FROM feishu_discussion_links l JOIN discussions d ON d.chat_id=l.chat_id
              WHERE l.owner=?1 AND (l.suggestion_id=?2 OR l.suggestion_id IN (
                SELECT former_id FROM feishu_suggestion_aliases WHERE owner=?1 AND suggestion_id=?2))
              ORDER BY d.updated_at DESC,l.suggestion_id",
        )?;
        let discussions = query.query_map(params![owner,evidence.id], |r| Ok(json!({
            "chat_id":r.get::<_,String>(0)?,"revision":r.get::<_,i64>(1)?,"title":r.get::<_,String>(2)?,
            "archived":r.get::<_,bool>(3)?,"attached":r.get::<_,bool>(4)?,"suggestion_id":r.get::<_,String>(5)?
        })))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut handoffs = Vec::new();
        let mut query = self.db.prepare(
            "SELECT revision,chat_id,suggestion_id FROM feishu_handoff_bridges WHERE owner=?1
              AND (suggestion_id=?2 OR suggestion_id IN (
                SELECT former_id FROM feishu_suggestion_aliases WHERE owner=?1 AND suggestion_id=?2))
              ORDER BY (suggestion_id=?2) DESC,revision DESC,suggestion_id",
        )?;
        for row in query.query_map(params![owner, evidence.id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })? {
            let (revision, chat, suggestion) = row?;
            handoffs.push(json!({"revision":revision,"suggestion_id":suggestion,"handoff":discussion::quick_status(&self.db,&chat,&bridge_operation(&chat))?}));
        }
        Ok(json!({
            "evidence":evidence,"updated_at":row.get::<_,i64>(1)?,"ignored":ignored,
            "assessment_status":row.get::<_,String>(3)?,"discussions":discussions,"handoffs":handoffs,"aliases":aliases,
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

type ContextRow = (
    String,
    String,
    Option<String>,
    i64,
    String,
    Option<String>,
    i64,
);

fn context_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ContextRow> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
        r.get(6)?,
    ))
}

fn decode_context(row: rusqlite::Result<ContextRow>) -> Result<ContextJob> {
    let (id, chat_id, topic, first_at, raw, cursor, material_revision) = row?;
    Ok(ContextJob {
        id,
        chat_id,
        topic,
        first_at,
        material_revision,
        cursor: cursor.map(|raw| serde_json::from_str(&raw)).transpose()?,
        evidence: serde_json::from_str(&raw)?,
    })
}

fn current_material(db: &Connection, owner: &str, job: &ContextJob) -> Result<bool> {
    Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM feishu_suggestions WHERE owner=?1 AND id=?2 AND material_revision=?3)",
        params![owner,job.id,job.material_revision], |r| r.get(0))?)
}

fn current(
    db: &Connection,
    owner: &str,
    id: &str,
    revision: i64,
) -> Result<Change<FeishuSuggestionEvidence>> {
    let Some((raw, topic)) = db.query_row(
        "SELECT evidence,topic FROM feishu_suggestions WHERE owner=?1 AND id=?2 AND first_at>=unixepoch()-?3",
        params![owner,id,RETENTION], |r| Ok((r.get::<_,String>(0)?,r.get::<_,Option<String>>(1)?)),
    ).optional()? else { return Ok(Change::Missing) };
    let mut evidence: FeishuSuggestionEvidence = serde_json::from_str(&raw)?;
    filter_expired(&mut evidence, crate::feishu::now() - RETENTION);
    project_active_evidence(&mut evidence, topic.as_deref());
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
    let mut snapshot = e.clone();
    if let Some(assessment) = &e.assessment
        && assessment.card.is_some()
    {
        let needed = |m: &FeishuEvidence| assessment.evidence_ids.contains(&m.id);
        snapshot.mentions.retain(needed);
        snapshot.replies.retain(needed);
        snapshot.before.retain(needed);
        snapshot.direct.retain(needed);
        snapshot.cited.retain(needed);
    } else {
        // Older results and fact-only cards have no per-claim citations.
        snapshot
            .replies
            .drain(..snapshot.replies.len().saturating_sub(4));
        snapshot
            .mentions
            .drain(..snapshot.mentions.len().saturating_sub(4));
        snapshot.cited.clear();
    }
    db.execute(
        "INSERT OR IGNORE INTO feishu_discussion_snapshots VALUES(?1,?2,?3,?4)",
        params![chat, e.id, e.revision, serde_json::to_string(&snapshot)?],
    )?;
    db.execute(
        "INSERT INTO discussion_references(chat_id,id,reference) VALUES(?1,?2,?3)
         ON CONFLICT(chat_id,id) DO UPDATE SET reference=excluded.reference",
        params![chat, reference.id, serde_json::to_string(&reference)?],
    )?;
    Ok(())
}
