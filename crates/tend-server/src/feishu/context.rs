use super::{
    RETENTION, Reader, ScanError, ScanResult, Store, evidence, locator, now, parse_excerpt,
    parse_message, provider_id, rows,
};
use crate::store::feishu::{ContextCursor, ContextJob, ContextResult};
use protocol::{FeishuEvidence, FeishuObservation, FeishuReference};
use serde_json::{Value, json};
use std::{
    cmp::Ordering,
    collections::{BTreeSet, HashMap, HashSet},
};

const PAGE_BUDGET: usize = 4;
const MAX_PAGES: usize = 200;
const MAX_KNOWN: usize = 512;
pub(super) const NEARBY: i64 = 2 * 3600;

pub(super) fn exact(
    store: &Store,
    reader: &Reader,
    chat: &str,
    ids: &[String],
    excerpt_limit: usize,
) -> ScanResult<Vec<FeishuEvidence>> {
    let mut messages = Vec::new();
    for chunk in ids.chunks(20) {
        let ids: Vec<_> = chunk.iter().map(String::as_str).collect();
        let data = reader.mget(&ids)?;
        for raw in rows(&data)? {
            if let Some(m) = parse_exact(store, raw, chat, excerpt_limit)
                && chunk.contains(&m.id)
            {
                messages.push(m);
            }
        }
    }
    Ok(messages)
}

// Exact reads must prove chat scope independently. Access to a target in
// some other chat is not permission to include it in this assessment.
fn parse_exact(store: &Store, raw: &Value, chat: &str, limit: usize) -> Option<FeishuEvidence> {
    let message = parse_excerpt(store, raw, None, limit).ok()?;
    (message.chat_id == chat).then_some(message.evidence)
}

pub(super) fn exact_one(
    store: &Store,
    reader: &Reader,
    chat: &str,
    id: &str,
    limit: usize,
) -> ScanResult<Option<(FeishuEvidence, Value)>> {
    let data = reader.mget(&[id])?;
    Ok(rows(&data)?.iter().find_map(|raw| {
        if raw["message_id"] != id {
            return None;
        }
        parse_exact(store, raw, chat, limit).map(|m| (m, raw.clone()))
    }))
}

#[derive(Clone, Copy)]
pub(super) struct PageQuery<'a> {
    pub chat: &'a str,
    pub thread: Option<&'a str>,
    pub from: i64,
    pub through: i64,
    pub end: Option<i64>,
    pub order: &'a str,
    pub size: &'a str,
    pub token: Option<&'a str>,
}

pub(super) struct Page {
    pub messages: Vec<FeishuEvidence>,
    pub next: Option<String>,
    pub gaps: BTreeSet<String>,
    pub read_from: Option<i64>,
    pub read_to: Option<i64>,
}

pub(super) fn read_page(store: &Store, reader: &Reader, q: PageQuery<'_>) -> ScanResult<Page> {
    let mut params = json!({
        "container_id_type": if q.thread.is_some() { "thread" } else { "chat" },
        "container_id": q.thread.unwrap_or(q.chat), "page_size": q.size,
        "sort_type": if q.order == "desc" { "ByCreateTimeDesc" } else { "ByCreateTimeAsc" },
        "with_sender_name": true, "card_msg_content_type": "raw_card_content",
    });
    if q.thread.is_none() {
        params["start_time"] = json!(q.from.to_string());
        params["end_time"] = json!(q.end.unwrap_or(q.through).to_string());
        params["only_thread_root_messages"] = json!(true);
    }
    if let Some(token) = q.token {
        params["page_token"] = json!(token);
    }
    let data = reader.message_api("/open-apis/im/v1/messages", params)?;
    let mut gaps = BTreeSet::new();
    if data["notice"].as_str().is_some_and(|s| !s.is_empty()) {
        gaps.insert("provider_notice".into());
    }
    let mut messages = Vec::new();
    let mut read_from = None::<i64>;
    let mut read_to = None::<i64>;
    let raw_rows = if data["has_more"] == false && data["items"].is_null() {
        &[]
    } else {
        rows(&data)?
    };
    for raw in raw_rows {
        let Ok(m) = parse_message(store, raw, Some(q.chat)) else {
            gaps.insert("message_unavailable".into());
            continue;
        };
        if m.chat_id != q.chat {
            continue;
        }
        let mut e = m.evidence;
        if let Some(thread) = q.thread {
            if m.thread_id
                .as_deref()
                .is_some_and(|actual| actual != thread)
            {
                continue;
            }
            e.source.thread_id = Some(thread.to_owned());
            (e.source.position, e.href, e.source.chat_link_only) =
                locator::resolve(raw, q.chat, Some(thread));
        }
        let at = e.created_at;
        if at < q.from || at > q.end.unwrap_or(q.through) {
            continue;
        }
        if !evidence::visible(&e, q.through) {
            gaps.insert("version_not_proven_at_cutoff".into());
            continue;
        }
        // A deletion without a timestamp can't prove it was already
        // withdrawn when an earlier observation closed.
        if e.source.withdrawn && e.updated_at <= e.created_at {
            gaps.insert("withdrawal_time_unknown".into());
            continue;
        }
        read_from = Some(read_from.map_or(at, |old| old.min(at)));
        read_to = Some(read_to.map_or(at, |old| old.max(at)));
        messages.push(e);
    }
    let next = match data["has_more"].as_bool() {
        Some(false) => None,
        Some(true) => data["page_token"]
            .as_str()
            .filter(|token| !token.is_empty() && token.len() <= 4096 && Some(*token) != q.token)
            .map(str::to_owned)
            .or_else(|| {
                gaps.insert("pagination_gap".into());
                None
            }),
        None => {
            gaps.insert("pagination_gap".into());
            None
        }
    };
    Ok(Page {
        messages,
        next,
        gaps,
        read_from,
        read_to,
    })
}

pub(super) fn thread(job: &ContextJob) -> Option<&str> {
    evidence::topic_thread(job.topic.as_deref())
}

pub(super) fn fetch(
    store: &Store,
    reader: &Reader,
    job: &ContextJob,
    current: i64,
) -> ScanResult<ContextResult> {
    let mut gaps = BTreeSet::new();
    let stored_topic = thread(job);
    let mut ids = Vec::new();
    // Exact rereads cover edits and tombstones that no longer appear in the
    // incremental @ search, including the small set of cited extra evidence.
    for e in std::iter::once(&job.evidence.anchor)
        .chain(
            job.evidence
                .mentions
                .iter()
                .filter(|m| evidence::in_topic(m, stored_topic)),
        )
        .chain(&job.evidence.cited)
        .chain(&job.evidence.replies)
        .chain(&job.evidence.before)
    {
        if e.created_at >= current - RETENTION && !ids.contains(&e.id) {
            ids.push(e.id.clone());
        }
    }
    let mut cursor = job
        .cursor
        .as_ref()
        .filter(|c| c.thread.as_deref() == stored_topic);
    // Always check the anchor even during a continuation. A root can gain
    // a topic while an old chat listing still has more pages.
    let reread = cursor.is_some()
        || job.evidence.observation.through > 0
        || job.evidence.anchor.source.author_id.is_none();
    let mut anchor = job.evidence.anchor.clone();
    let ids = if cursor.is_some() {
        std::slice::from_ref(&anchor.id)
    } else {
        &ids
    };
    let mut verified = if reread {
        exact(store, reader, &job.chat_id, ids, 1200).unwrap_or_else(|err| {
            gaps.insert(evidence::gap(err).into());
            Vec::new()
        })
    } else {
        Vec::new()
    };

    let exact_at = now();
    if verified
        .iter()
        .any(|m| !evidence::visible(m, exact_at) && !m.source.withdrawn)
    {
        gaps.insert("version_not_proven_at_cutoff".into());
    }
    for m in &mut verified {
        observe_tombstone(m, exact_at);
    }
    verified.retain(|m| evidence::visible(m, exact_at));
    // Check routing against the verified new version before deciding if the
    // old fixed cutoff is still applicable.
    let mut fresh_anchor = anchor.clone();
    if let Some(fresh) = verified.iter().find(|m| m.id == anchor.id) {
        crate::store::feishu::replace_version(&mut fresh_anchor, fresh);
    } else if reread {
        gaps.insert("anchor_reread_unavailable".into());
    }
    let topic = fresh_anchor
        .source
        .thread_id
        .as_deref()
        .filter(|id| provider_id(id, "omt_"))
        .or(stored_topic)
        .map(str::to_owned);
    if topic.as_deref() != stored_topic {
        cursor = None;
    }
    if cursor.is_none() {
        anchor = fresh_anchor;
    } else {
        // The probe was only for routing. Do not insert today's version into
        // an existing observation, even if the cutoff is still in the past.
        verified.clear();
    }
    let topic = topic.as_deref();
    if anchor.source.edit_time_unknown {
        gaps.insert("version_not_proven_at_cutoff".into());
    }
    // If the provider indicates this is a thread reply but omitted its
    // direct target, don't turn a thread or root ID into a parent guess.
    if anchor.source.thread_id.is_some()
        && anchor.source.reply_to.is_none()
        && anchor.source.root_id.as_deref() != Some(anchor.id.as_str())
    {
        gaps.insert("thread_parent_unreported".into());
    }
    let mut target_ids: Vec<_> = evidence::targets(&anchor)
        .map(|(id, _)| id.to_string())
        .collect();
    target_ids.sort_unstable();
    target_ids.dedup();
    let mut targets = if cursor.is_some() {
        job.evidence.direct.clone()
    } else {
        exact(store, reader, &job.chat_id, &target_ids, 1200).unwrap_or_else(|err| {
            gaps.insert(evidence::gap(err).into());
            Vec::new()
        })
    };
    // A successful exact reread was completed during this observation, so it
    // can record a provider tombstone even if it has no modification time.
    let through = cursor.map_or_else(now, |c| c.through);
    if cursor.is_none() {
        for m in &mut targets {
            observe_tombstone(m, through);
        }
    }
    let mut direct = Vec::new();
    let mut direct_ids = HashSet::new();
    let mut references = Vec::new();
    for (id, relation) in evidence::targets(&anchor) {
        let known = targets.iter().find(|m| m.id == id);
        let cached = job.evidence.direct.iter().find(|m| m.id == id);
        let available = known.filter(|m| evidence::visible(m, through)).or_else(|| {
            // A cached version is usable if we only lack the current version
            // at the fixed cutoff. Otherwise report unread, not fresh facts.
            known.filter(|m| !evidence::visible(m, through))?;
            cached.filter(|m| evidence::visible(m, through))
        });
        let status = if let Some(m) = available {
            if direct_ids.insert(id.to_owned()) {
                direct.push(m.clone());
            }
            "available"
        } else if known.is_some() {
            gaps.insert("direct_reference_outside_observation".into());
            "out_of_window"
        } else {
            gaps.insert("direct_reference_unavailable".into());
            "unavailable"
        };
        references.push(FeishuReference {
            id: id.into(),
            relation: relation.into(),
            status: status.into(),
        });
    }
    if anchor.source.links_unread {
        gaps.insert("external_link_not_read".into());
    }

    let cutoff = through - RETENTION;
    let from = cursor.map_or_else(
        || {
            if topic.is_some() {
                cutoff
            } else if !direct.is_empty() {
                (job.first_at - NEARBY).max(cutoff)
            } else {
                job.first_at.max(cutoff)
            }
        },
        |c| c.from,
    );
    let mut token = cursor.map(|c| c.token.clone());
    let mut pages = cursor.map_or(0, |c| c.pages);
    let mut seen = HashSet::new();
    let mut found = HashMap::new();
    let received = || std::iter::once(anchor.created_at).chain(direct.iter().map(|m| m.created_at));
    let mut read_from = received().chain(cursor.and_then(|c| c.read_from)).min();
    let mut read_to = received().chain(cursor.and_then(|c| c.read_to)).max();
    let mut complete = cursor.is_none_or(|c| c.complete);
    for _ in 0..PAGE_BUDGET {
        let page = read_page(
            store,
            reader,
            PageQuery {
                chat: &job.chat_id,
                thread: topic,
                from,
                through,
                end: None,
                order: "asc",
                size: "50",
                token: token.as_deref(),
            },
        );
        if let Some(previous) = token.take() {
            seen.insert(previous);
        }
        let page = match page {
            Ok(page) => page,
            Err(err) => {
                gaps.insert(evidence::gap(err).into());
                complete = false;
                break;
            }
        };
        pages += 1;
        complete &= page.gaps.is_empty();
        gaps.extend(page.gaps);
        read_from = read_from.into_iter().chain(page.read_from).min();
        read_to = read_to.into_iter().chain(page.read_to).max();
        found.extend(page.messages.into_iter().map(|m| (m.id.clone(), m)));
        token = page.next;
        if token
            .as_ref()
            .is_some_and(|next| seen.contains(next) || pages >= MAX_PAGES)
        {
            gaps.insert("pagination_gap".into());
            complete = false;
            token = None;
        }
        if token.is_none() {
            break;
        }
    }

    // A provider can return the exact target on a container page even
    // when the batch lookup omitted it. Keep it in the separate direct
    // section, with the same ID and scope checks as the rest of the page.
    for reference in references.iter_mut().filter(|r| r.status != "available") {
        if let Some(message) = found.get(&reference.id) {
            reference.status = "available".into();
            if direct_ids.insert(reference.id.clone()) {
                direct.push(message.clone());
            }
        }
    }
    for (status, gap) in [
        ("unavailable", "direct_reference_unavailable"),
        ("out_of_window", "direct_reference_outside_observation"),
    ] {
        if !references.iter().any(|r| r.status == status) {
            gaps.remove(gap);
        }
    }

    // Use the same verified mention versions for coverage and packet
    // publication. An old membership must not survive a successful reread.
    let mut mentions = job.evidence.mentions.clone();
    for m in &mut mentions {
        if let Some(fresh) = verified.iter().find(|fresh| fresh.id == m.id) {
            crate::store::feishu::replace_version(m, fresh);
        }
    }
    let seeds: HashSet<_> = mentions
        .iter()
        .filter(|m| evidence::in_window(m, topic, through))
        .map(|m| m.id.clone())
        .chain([anchor.id.clone()])
        .collect();
    let foreign = store
        .feishu_chat_mentions(&reader.owner_open_id, &job.chat_id, &job.id)
        .map_err(|_| ScanError::Storage)?;
    let mut known = seeds.clone();
    if let Some(cursor) = cursor {
        known.extend(cursor.known.iter().cloned());
    }
    let mut candidates: HashMap<_, _> = job
        .evidence
        .replies
        .iter()
        .chain(&job.evidence.before)
        .filter(|m| evidence::in_window(m, topic, through) && !foreign.contains(&m.id))
        .map(|m| (m.id.clone(), m.clone()))
        .collect();
    for fresh in &verified {
        if let Some(cached) = candidates.get_mut(&fresh.id) {
            crate::store::feishu::replace_version(cached, fresh);
        }
    }
    candidates.retain(|_, m| evidence::in_window(m, topic, through));
    // Keep verified descendants across pages without promoting earlier
    // ancestors (or the other requests that reply to them).
    if topic.is_none() {
        evidence::extend_descendants(&mut known, &candidates.values().collect::<Vec<_>>());
    }
    let mut edges = Vec::new();
    let mut related = HashSet::new();
    // Direct targets are context, not permission to merge sibling requests
    // addressed to the same message. Only descend from the current requests.
    loop {
        let initial = known.len();
        for m in found.values() {
            if seeds.contains(&m.id) {
                continue;
            }
            for parent in evidence::relationship_ids(m)
                .filter(|id| known.contains(*id))
                .collect::<Vec<_>>()
            {
                edges.push((m.id.clone(), parent.to_owned()));
                related.insert(m.id.clone());
            }
            if related.contains(&m.id) && !foreign.contains(&m.id) {
                known.insert(m.id.clone());
            }
        }
        if initial == known.len() {
            break;
        }
    }
    // Follow verified ancestors of direct targets to find earlier referenced
    // context, without treating their other replies as part of this request.
    let mut ancestors = direct_ids.clone();
    loop {
        let before = ancestors.len();
        for m in direct.iter().chain(found.values()) {
            if ancestors.contains(&m.id) {
                let links: Vec<_> = evidence::relationship_ids(m).map(str::to_owned).collect();
                for link in links {
                    ancestors.insert(link);
                }
            }
        }
        if ancestors.len() == before {
            break;
        }
    }
    let mut outside = foreign.clone();
    loop {
        let before = outside.len();
        for m in found.values() {
            if !related.contains(&m.id)
                && evidence::relationship_ids(m).any(|id| outside.contains(id))
            {
                outside.insert(m.id.clone());
            }
        }
        if before == outside.len() {
            break;
        }
    }
    for m in found.into_values() {
        let previously_related = candidates.contains_key(&m.id);
        if seeds.contains(&m.id) || direct_ids.contains(&m.id) || outside.contains(&m.id) {
            continue;
        }
        if !previously_related && m.source.withdrawn && !related.contains(&m.id) {
            continue;
        }
        if topic.is_some()
            || related.contains(&m.id)
            || ancestors.contains(&m.id)
            || previously_related
        {
            if evidence::relative(&m, &anchor).is_none() {
                complete = false;
                gaps.insert("message_order_unknown".into());
                continue;
            }
            if let Some(cached) = candidates.get_mut(&m.id) {
                crate::store::feishu::replace_version(cached, &m);
            } else {
                candidates.insert(m.id.clone(), m);
            }
        }
    }
    let mut before = Vec::new();
    let mut replies = Vec::new();
    for m in candidates.into_values() {
        if seeds.contains(&m.id) || direct_ids.contains(&m.id) || outside.contains(&m.id) {
            continue;
        }
        match evidence::relative(&m, &anchor) {
            Some(Ordering::Less) => before.push(m),
            Some(Ordering::Greater) => replies.push(m),
            _ => (),
        }
    }
    evidence::sort(&mut direct);
    evidence::sort(&mut before);
    evidence::sort(&mut replies);
    let can_read_before = before.len() > 2 || topic.is_none() || token.is_some() || !complete;
    let can_read_after = replies.len() > 4 || topic.is_none() || token.is_some() || !complete;
    before.drain(..before.len().saturating_sub(2));
    replies.drain(..replies.len().saturating_sub(12));
    if token.is_some() {
        gaps.insert("pending_pages".into());
    } else if !complete && gaps.is_empty() {
        gaps.insert("page_incomplete".into());
    }
    let done = complete && token.is_none() && !anchor.source.edit_time_unknown;
    let any_after = !replies.is_empty()
        || mentions.iter().any(|m| {
            evidence::in_window(m, topic, through)
                && evidence::relative(m, &anchor) == Some(Ordering::Greater)
        });
    let after_status = if !any_after && done {
        "checked_empty"
    } else if done {
        "read"
    } else if pages == 0 {
        "unavailable"
    } else {
        "partial"
    };
    let related_replies = replies
        .iter()
        .filter(|m| known.contains(&m.id))
        .map(|m| m.id.clone())
        .collect();
    let next = token.map(|token| {
        let mut known: Vec<_> = known.into_iter().filter(|id| !seeds.contains(id)).collect();
        known.sort_unstable();
        if known.len() > MAX_KNOWN {
            known.truncate(MAX_KNOWN);
            complete = false;
        }
        ContextCursor {
            from,
            through,
            token,
            thread: topic.map(str::to_owned),
            pages,
            complete,
            known,
            read_from,
            read_to,
        }
    });
    Ok(ContextResult {
        thread: topic.map(str::to_owned),
        replies,
        related_replies,
        before,
        direct,
        references,
        verified,
        proven_edges: edges,
        observation: FeishuObservation {
            through,
            read_from,
            read_to,
            can_read_before,
            can_read_after,
            after_status: after_status.into(),
            gaps: gaps.into_iter().collect(),
        },
        complete: done,
        next,
    })
}

fn observe_tombstone(m: &mut FeishuEvidence, observed: i64) {
    if m.source.withdrawn && m.updated_at <= m.created_at {
        m.updated_at = observed;
        m.source.edited_minute_precision = false;
        m.source.edited_subsecond = false;
        m.source.edit_time_unknown = false;
    }
}
