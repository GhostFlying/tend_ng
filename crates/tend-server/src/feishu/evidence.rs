use super::{RETENTION, ScanError, provider_id};
use protocol::{
    FeishuEvidence, FeishuMentionBinding, FeishuObservation, FeishuPerson, FeishuReference,
    FeishuSuggestionEvidence,
};
use serde_json::{Value, json};
use std::{cmp::Ordering, collections::HashSet};

pub(super) fn visible(e: &FeishuEvidence, through: i64) -> bool {
    let uncertainty = |minute, subsecond| if minute { 59 } else { i64::from(subsecond) };
    !e.source.edit_time_unknown
        && e.created_at >= through - RETENTION
        && e.created_at
            + uncertainty(
                e.source.created_minute_precision,
                e.source.created_subsecond,
            )
            <= through
        && e.updated_at + uncertainty(e.source.edited_minute_precision, e.source.edited_subsecond)
            <= through
}

pub(crate) fn topic_thread(topic: Option<&str>) -> Option<&str> {
    topic.filter(|id| provider_id(id, "omt_"))
}

// This is the ambient window, not the separate anchor/direct-reference
// section. The latter can contain explicit links into the same chat.
pub(crate) fn in_topic(message: &FeishuEvidence, thread: Option<&str>) -> bool {
    thread.is_none_or(|id| message.source.thread_id.as_deref() == Some(id))
}

pub(super) fn in_window(message: &FeishuEvidence, thread: Option<&str>, through: i64) -> bool {
    in_topic(message, thread) && visible(message, through)
}

pub(super) fn relative(e: &FeishuEvidence, anchor: &FeishuEvidence) -> Option<Ordering> {
    if e.id == anchor.id {
        return Some(Ordering::Equal);
    }
    if e.created_at.div_euclid(60) == anchor.created_at.div_euclid(60)
        && (e.source.created_minute_precision || anchor.source.created_minute_precision)
    {
        let order = e.source.position?.cmp(&anchor.source.position?);
        return (order != Ordering::Equal).then_some(order);
    }
    let order = e.created_at.cmp(&anchor.created_at);
    if order == Ordering::Equal {
        let order = e.source.position?.cmp(&anchor.source.position?);
        return (order != Ordering::Equal).then_some(order);
    }
    Some(order)
}

pub(crate) fn sort(evidence: &mut [FeishuEvidence]) {
    evidence.sort_by(|a, b| {
        let key =
            |m: &FeishuEvidence| (m.created_at.div_euclid(60), m.source.position, m.created_at);
        key(a).cmp(&key(b)).then_with(|| a.id.cmp(&b.id))
    });
}

pub(super) fn targets(e: &FeishuEvidence) -> impl Iterator<Item = (&str, &'static str)> {
    e.source
        .reply_to
        .iter()
        .filter(|id| **id != e.id)
        .map(|id| (id.as_str(), "reply_to"))
        .chain(e.source.quote_ids.iter().map(|id| (id.as_str(), "quotes")))
}

pub(crate) fn relationship_ids(e: &FeishuEvidence) -> impl Iterator<Item = &str> {
    let s = &e.source;
    // An implicit reply to the thread root is a container relation. It must
    // not merge two otherwise independent requests in that thread.
    let reply = s
        .reply_to
        .as_deref()
        .filter(|parent| s.thread_id.is_none() || s.root_id.as_deref() != Some(*parent));
    let root = s.root_id.as_deref().filter(|_| s.thread_id.is_none());
    reply
        .into_iter()
        .chain(s.quote_ids.iter().map(String::as_str))
        .chain(root)
        .filter(|id| *id != e.id)
}

pub(super) fn extend_descendants(related: &mut HashSet<String>, messages: &[&FeishuEvidence]) {
    loop {
        let before = related.len();
        for m in messages {
            if relationship_ids(m).any(|id| related.contains(id)) {
                related.insert(m.id.clone());
            }
        }
        if before == related.len() {
            break;
        }
    }
}

pub(super) fn gap(error: ScanError) -> &'static str {
    match error {
        ScanError::Identity => "identity_unavailable",
        ScanError::Coverage => "page_incomplete",
        ScanError::Provider => "source_unavailable",
        ScanError::Storage => "storage_unavailable",
    }
}

pub(super) struct Packet<'a> {
    id: &'a str,
    revision: i64,
    chat_name: &'a str,
    thread: Option<&'a str>,
    owner: &'a FeishuPerson,
    pub anchor: &'a FeishuEvidence,
    pub direct: Vec<&'a FeishuEvidence>,
    pub references: &'a [FeishuReference],
    pub before: Vec<&'a FeishuEvidence>,
    pub after: Vec<&'a FeishuEvidence>,
    pub observation: FeishuObservation,
    changed: &'a [String],
    related_replies: &'a [String],
}

impl<'a> Packet<'a> {
    pub fn new(
        e: &'a FeishuSuggestionEvidence,
        thread: Option<&'a str>,
        owner: &'a FeishuPerson,
    ) -> Self {
        let mut seen = HashSet::from([e.anchor.id.as_str()]);
        let through = e.observation.through;
        let direct: Vec<_> = e
            .direct
            .iter()
            .filter(|m| visible(m, through) && seen.insert(&m.id))
            .collect();
        let relative: Vec<_> = e
            .before
            .iter()
            .chain(&e.mentions)
            .chain(&e.replies)
            .filter(|m| in_window(m, thread, through) && seen.insert(&m.id))
            .collect();
        let mut before: Vec<_> = relative
            .iter()
            .copied()
            .filter(|m| self::relative(m, &e.anchor) == Some(Ordering::Less))
            .collect();
        let mut after: Vec<_> = relative
            .into_iter()
            .filter(|m| self::relative(m, &e.anchor) == Some(Ordering::Greater))
            .collect();
        let sort_refs = |messages: &mut Vec<&FeishuEvidence>| {
            messages.sort_by_key(|m| {
                (
                    m.created_at.div_euclid(60),
                    m.source.position,
                    m.created_at,
                    m.id.clone(),
                )
            });
        };
        sort_refs(&mut before);
        sort_refs(&mut after);
        let mut observation = e.observation.clone();
        if before.len() > 2 {
            observation.can_read_before = true;
        }
        if after.len() > 4 {
            observation.can_read_after = true;
        }
        before.drain(..before.len().saturating_sub(2));
        after.drain(..after.len().saturating_sub(4));
        Self {
            id: &e.id,
            revision: e.revision,
            chat_name: &e.chat_name,
            thread,
            owner,
            anchor: &e.anchor,
            direct,
            references: &e.references,
            before,
            after,
            observation,
            changed: &e.changed,
            related_replies: &e.related_replies,
        }
    }

    pub fn input(&self) -> Value {
        let model_message = |m: &&FeishuEvidence| for_model(m, &self.owner.id);
        json!({
            "id":self.id, "revision":self.revision, "chat_name":self.chat_name,
            "thread":self.thread, "owner":self.owner, "anchor":model_message(&self.anchor),
            "direct":self.direct.iter().map(model_message).collect::<Vec<_>>(),
            "references":self.references,
            "before":self.before.iter().map(model_message).collect::<Vec<_>>(),
            "after":self.after.iter().map(model_message).collect::<Vec<_>>(),
            "observation":self.observation, "changed":self.changed,
            "related_reply_ids":self.after.iter().map(|m| &m.id)
                .filter(|id| self.related_replies.contains(id)).collect::<Vec<_>>(),
        })
    }

    pub fn messages(&self) -> impl Iterator<Item = &FeishuEvidence> {
        std::iter::once(self.anchor)
            .chain(self.direct.iter().copied())
            .chain(self.before.iter().copied())
            .chain(self.after.iter().copied())
    }
}

pub(super) fn bound_text(
    text: &str,
    bindings: &[FeishuMentionBinding],
) -> (String, Vec<FeishuMentionBinding>) {
    let mut rendered = String::new();
    let mut projected = Vec::with_capacity(bindings.len());
    let mut chars = text.chars();
    let mut end = 0;
    let mut offset = 0;
    for binding in bindings {
        if binding.start < end || binding.start >= binding.end {
            continue;
        }
        rendered.extend(chars.by_ref().take(binding.start - end));
        offset += binding.start - end;
        let marker = format!("@{{{}}}", binding.id);
        rendered.push_str(&marker);
        projected.push(FeishuMentionBinding {
            start: offset,
            end: offset + marker.chars().count(),
            id: binding.id.clone(),
        });
        offset = projected.last().unwrap().end;
        for _ in chars.by_ref().take(binding.end - binding.start) {}
        end = binding.end;
    }
    rendered.extend(chars);
    (rendered, projected)
}

// Compatibility comparison for legacy excerpts without structured bindings.
pub(crate) fn canonical_mentions(body: &str, mentions: &[FeishuPerson]) -> String {
    // Replace longer rendered names first so "林" cannot consume "林青".
    let mut names: Vec<_> = mentions.iter().filter(|m| !m.name.is_empty()).collect();
    names.sort_by_key(|p| std::cmp::Reverse(p.name.len()));
    names.into_iter().fold(body.to_owned(), |text, person| {
        text.replace(&format!("@{}", person.name), &format!("@{{{}}}", person.id))
    })
}

pub(super) fn for_model(e: &FeishuEvidence, owner: &str) -> FeishuEvidence {
    let mut shown = e.clone();
    shown.source.content_key = None;
    if !e.source.mention_bindings.is_empty() {
        (shown.text, shown.source.mention_bindings) =
            bound_text(&e.text, &e.source.mention_bindings);
    }
    if shown.source.author_id.as_deref() == Some(owner) {
        shown.sender = "本人".into();
    }
    for person in &mut shown.source.mentions {
        if person.id == owner {
            person.name.clear();
        }
    }
    shown
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::FeishuSource;

    fn msg(id: &str, created: i64) -> FeishuEvidence {
        FeishuEvidence {
            id: id.into(),
            sender: "小林".into(),
            text: "内容".into(),
            href: "".into(),
            created_at: created,
            updated_at: created,
            source: FeishuSource::default(),
        }
    }

    #[test]
    fn selects_nearest_prior_and_latest_later_around_separate_targets() {
        let owner = FeishuPerson {
            id: "p1".into(),
            name: "小林".into(),
        };
        let anchor = msg("anchor", 600);
        let mut too_new = msg("too-new", 670);
        too_new.updated_at = 950;
        let mut ambiguous = msg("ambiguous", 600);
        ambiguous.source.created_minute_precision = true;
        let e: FeishuSuggestionEvidence = serde_json::from_value(json!({
            "id":"card","revision":1,"chat_name":"group",
            "anchor":anchor, "direct":[msg("quote",410),msg("quote",410)],
            "mentions":[anchor,msg("a",610),msg("f",700)],
            "before":[msg("quote",410),msg("p1",430),msg("p3",580),msg("p2",490)],
            "replies":[msg("b",620),msg("e",690),msg("c",630),msg("d",685),too_new,ambiguous],
            "context_status":"ready","observation":{"through":720},"assessment":null
        }))
        .unwrap();
        let packet = Packet::new(&e, None, &owner);
        let ids = |rows: &[&FeishuEvidence]| rows.iter().map(|m| m.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&packet.direct), ["quote"]);
        assert_eq!(ids(&packet.before), ["p2", "p3"]);
        assert_eq!(ids(&packet.after), ["c", "d", "e", "f"]);
        assert!(packet.observation.can_read_before && packet.observation.can_read_after);
    }

    #[test]
    fn a_minute_label_does_not_prove_a_visible_version_at_the_cutoff() {
        let mut m = msg("edited", 590);
        m.updated_at = 600;
        m.source.edited_minute_precision = true;
        assert!(!visible(&m, 640));
        assert!(visible(&m, 659));
        m.source.edited_minute_precision = false;
        assert!(visible(&m, 640));
        m.source.created_minute_precision = true;
        assert!(!visible(&m, 640));
    }

    #[test]
    fn an_unknown_edit_cannot_be_used_at_any_cutoff() {
        let mut message = msg("unknown-edit", 590);
        message.source.edit_time_unknown = true;
        assert!(!visible(&message, 900));
        message.source.edit_time_unknown = false;
        message.updated_at = 900;
        message.source.edited_subsecond = true;
        assert!(!visible(&message, 900));
        assert!(visible(&message, 901));
    }
}
