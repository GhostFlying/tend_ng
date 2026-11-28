use super::{RETENTION, Reader, Store, context, evidence, locator, source_position};
use crate::store::feishu::ContextJob;
use codex_app::{ToolCall, ToolOutput};
use protocol::{FeishuEvidence, FeishuObservation, FeishuReference};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    cmp::Ordering,
    collections::{BTreeSet, HashMap, HashSet},
};

pub(super) fn tools() -> [Value; 2] {
    [
        json!({
            "name":"feishu_read_material",
            "description":"Read a known evidence ID or a verified direct reply/quote target. Same chat and observation only. Does not open external links.",
            "inputSchema":{"type":"object","properties":{"id":{"type":"string"}},"required":["id"],"additionalProperties":false}
        }),
        json!({
            "name":"feishu_read_page",
            "description":"Read one page before or after this anchor, using the thread or nearby group context. Pass the same direction and next_cursor to continue. Group neighbors are not evidence of the same item without a concrete connection.",
            "inputSchema":{"type":"object","properties":{"direction":{"enum":["before","after"]},"cursor":{"type":"string"}},"required":["direction"],"additionalProperties":false}
        }),
    ]
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MaterialArgs {
    id: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Direction {
    Before,
    After,
}

impl Direction {
    fn side(self) -> Ordering {
        match self {
            Self::Before => Ordering::Less,
            Self::After => Ordering::Greater,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PageArgs {
    direction: Direction,
    cursor: Option<String>,
}

#[derive(Clone)]
struct Cursor {
    direction: Direction,
    token: String,
    seen: HashSet<String>,
    from: i64,
    end: i64,
}

pub(super) struct Reads<'a> {
    store: &'a Store,
    reader: &'a Reader,
    job: &'a ContextJob,
    pub delivered: HashMap<String, FeishuEvidence>,
    // Whether the model needs to establish a semantic connection itself.
    pub nearby: HashSet<String>,
    pub gaps: BTreeSet<String>,
    related: HashSet<String>,
    references: Vec<FeishuReference>,
    missing_reads: HashSet<String>,
    observation: FeishuObservation,
    targets: HashSet<String>,
    foreign: HashSet<String>,
    cursors: HashMap<String, Cursor>,
    read_budget: usize,
}

impl<'a> Reads<'a> {
    pub fn new(
        store: &'a Store,
        reader: &'a Reader,
        job: &'a ContextJob,
        packet: &evidence::Packet<'_>,
    ) -> anyhow::Result<Self> {
        let delivered = packet
            .messages()
            .map(|m| (m.id.clone(), m.clone()))
            .collect();
        let targets = packet
            .messages()
            .flat_map(evidence::targets)
            .map(|(id, _)| id.to_owned())
            .collect();
        let mut related: HashSet<_> = std::iter::once(packet.anchor)
            .chain(job.evidence.mentions.iter().filter(|m| {
                evidence::in_window(m, context::thread(job), packet.observation.through)
            }))
            .map(|m| m.id.clone())
            .collect();
        if context::thread(job).is_some() {
            related.extend(
                packet
                    .before
                    .iter()
                    .chain(&packet.after)
                    .map(|m| m.id.clone()),
            );
        } else {
            // The background graph includes verified descendants omitted
            // by the first packet's 2/4 excerpt limits.
            related.extend(job.evidence.related_replies.iter().cloned());
            // Earlier ancestors can be valid context, but their siblings
            // are not descendants of this request.
            let messages: Vec<_> = packet.before.iter().chain(&packet.after).copied().collect();
            evidence::extend_descendants(&mut related, &messages);
        }
        Ok(Self {
            store,
            reader,
            job,
            delivered,
            nearby: HashSet::new(),
            gaps: packet.observation.gaps.iter().cloned().collect(),
            related,
            references: packet.references.to_vec(),
            missing_reads: HashSet::new(),
            observation: packet.observation.clone(),
            targets,
            foreign: store.feishu_chat_mentions(&reader.owner_open_id, &job.chat_id, &job.id)?,
            cursors: HashMap::new(),
            read_budget: 24,
        })
    }

    pub fn tool(&mut self, call: ToolCall) -> ToolOutput {
        let answer = if call.namespace.is_some() {
            Err("unknown_tool")
        } else {
            match call.tool.as_str() {
                "feishu_read_material" => serde_json::from_value::<MaterialArgs>(call.arguments)
                    .map_err(|_| "invalid_arguments")
                    .and_then(|args| self.material(args)),
                "feishu_read_page" => serde_json::from_value::<PageArgs>(call.arguments)
                    .map_err(|_| "invalid_arguments")
                    .and_then(|args| self.page(args)),
                _ => Err("unknown_tool"),
            }
        };
        match answer {
            Ok(body) => ToolOutput {
                success: true,
                body,
            },
            Err(gap) => {
                if gap != "direct_reference_unavailable" {
                    self.gaps.insert(gap.into());
                }
                ToolOutput {
                    success: false,
                    body: json!({"gap":gap,"through":self.through()}),
                }
            }
        }
    }

    pub fn metadata(&self) -> (FeishuObservation, Vec<FeishuReference>) {
        let mut observation = self.observation.clone();
        observation.gaps = self.gaps.iter().cloned().collect();
        (observation, self.references.clone())
    }

    fn expand_observation(&mut self, from: Option<i64>, to: Option<i64>) {
        self.observation.read_from = self.observation.read_from.into_iter().chain(from).min();
        self.observation.read_to = self.observation.read_to.into_iter().chain(to).max();
    }

    pub fn is_related(&self, id: &str) -> bool {
        self.related.contains(id)
            && self
                .delivered
                .get(id)
                .is_some_and(|m| evidence::in_window(m, context::thread(self.job), self.through()))
    }

    fn set_reference_status(&mut self, id: &str, status: &str) {
        self.missing_reads.remove(id);
        for reference in self.references.iter_mut().filter(|r| r.id == id) {
            reference.status = status.into();
        }
        self.reconcile_reference_gaps();
    }

    fn reconcile_reference_gaps(&mut self) {
        for (status, gap) in [
            ("unavailable", "direct_reference_unavailable"),
            ("out_of_window", "direct_reference_outside_observation"),
        ] {
            if self.references.iter().any(|r| r.status == status)
                || (status == "unavailable" && !self.missing_reads.is_empty())
            {
                self.gaps.insert(gap.into());
            } else {
                self.gaps.remove(gap);
            }
        }
    }

    fn through(&self) -> i64 {
        self.job.evidence.observation.through
    }

    fn model(&self, message: &FeishuEvidence) -> FeishuEvidence {
        evidence::for_model(message, &self.reader.owner_open_id)
    }

    fn check_read(&mut self) -> Result<(), &'static str> {
        if self.read_budget == 0 {
            return Err("read_limit_reached");
        }
        self.read_budget -= 1;
        self.reader.verify_identity().map_err(evidence::gap)?;
        Ok(())
    }

    fn material(&mut self, args: MaterialArgs) -> Result<Value, &'static str> {
        let cached = self.delivered.get(&args.id).cloned();
        let scope = if self.nearby.contains(&args.id) {
            "nearby_unverified"
        } else if self
            .references
            .iter()
            .any(|reference| reference.id == args.id)
            || cached.is_none()
        {
            "direct_reference"
        } else {
            "related"
        };
        if let Some(m) = cached.as_ref().filter(|m| !m.source.truncated) {
            self.set_reference_status(&m.id, "available");
            return Ok(json!({"message":self.model(m),"through":self.through(),"scope":scope}));
        }
        if cached.is_none() && !self.targets.contains(&args.id) {
            return Err("outside_material_scope");
        }
        self.check_read()?;
        let result = context::exact_one(self.store, self.reader, &self.job.chat_id, &args.id, 4800)
            .map_err(evidence::gap)?;
        let Some((fresh, raw)) = result else {
            if self
                .references
                .iter()
                .any(|r| r.id == args.id && r.status == "out_of_window")
            {
                return Err("direct_reference_outside_observation");
            }
            self.missing_reads.insert(args.id);
            self.reconcile_reference_gaps();
            return Err("direct_reference_unavailable");
        };
        if !evidence::visible(&fresh, self.through())
            || (fresh.source.withdrawn && fresh.updated_at <= fresh.created_at)
        {
            if cached.is_none() {
                self.set_reference_status(&fresh.id, "out_of_window");
            }
            return Err("version_not_proven_at_cutoff");
        }
        let m = if let Some(old) = cached {
            expand_same_version(old, fresh, &raw, &self.job.chat_id)
                .ok_or("version_not_proven_at_cutoff")?
        } else {
            fresh
        };
        self.set_reference_status(&m.id, "available");
        self.expand_observation(Some(m.created_at), Some(m.created_at));
        // One hop from known related evidence, not an expansion of an
        // arbitrary chain of exact reads selected by the model.
        let response = json!({"message":self.model(&m), "through":self.through(), "scope":scope});
        self.delivered.insert(m.id.clone(), m);
        Ok(response)
    }

    fn page(&mut self, args: PageArgs) -> Result<Value, &'static str> {
        let cursor = args
            .cursor
            .as_ref()
            .map(|id| {
                self.cursors
                    .get(id)
                    .filter(|c| c.direction == args.direction)
                    .cloned()
                    .ok_or("invalid_cursor")
            })
            .transpose()?;
        self.check_read()?;
        let through = self.through();
        let thread = context::thread(self.job);
        let anchor = &self.job.evidence.anchor;
        let last_related = self
            .delivered
            .values()
            .filter(|m| self.related.contains(&m.id))
            .map(|m| m.created_at)
            .max()
            .unwrap_or(anchor.created_at);
        let oldest = through - RETENTION;
        let (from, end) = match (thread, args.direction) {
            (Some(_), _) => (oldest, through),
            (None, Direction::Before) => (
                (anchor.created_at - context::NEARBY).max(oldest),
                (anchor.created_at + 59).min(through),
            ),
            (None, Direction::After) => (
                (anchor.created_at - 59).max(oldest),
                (last_related + context::NEARBY).min(through),
            ),
        };
        // Bind the provider token to its original query range even if a later
        // tool call exposes more related evidence. On each continuation the
        // range is the range recorded with the opaque cursor.
        let (from, end) = cursor
            .as_ref()
            .map(|c| (c.from, c.end))
            .unwrap_or((from, end));
        let mut page = context::read_page(
            self.store,
            self.reader,
            context::PageQuery {
                chat: &self.job.chat_id,
                thread,
                from,
                through,
                end: Some(end),
                order: if args.direction == Direction::Before {
                    "desc"
                } else {
                    "asc"
                },
                size: "12",
                token: cursor.as_ref().map(|c| c.token.as_str()),
            },
        )
        .map_err(evidence::gap)?;
        let mut seen = cursor.map_or_else(HashSet::new, |c| c.seen);
        let mut next_cursor = None;
        if let Some(next) = page.next {
            if seen.insert(next.clone()) {
                let key = protocol::new_id();
                self.cursors.insert(
                    key.clone(),
                    Cursor {
                        direction: args.direction,
                        token: next,
                        seen,
                        from,
                        end,
                    },
                );
                next_cursor = Some(key);
            } else {
                page.gaps.insert("pagination_gap".into());
            }
        }
        // Only emit rows proved to be on the requested side. The provider
        // applies exact bounds to chat creation times but not to thread pages.
        page.messages.retain(|m| {
            if m.created_at > end || self.foreign.contains(&m.id) || !evidence::in_topic(m, thread)
            {
                return false;
            }
            match evidence::relative(m, anchor) {
                Some(side) => side == args.direction.side(),
                None => {
                    page.gaps.insert("message_order_unknown".into());
                    false
                }
            }
        });
        evidence::sort(&mut page.messages);
        let mut relevant = self.related.clone();
        let mut excluded = self.foreign.clone();
        loop {
            let before = excluded.len();
            for m in &page.messages {
                if evidence::relationship_ids(m).any(|id| excluded.contains(id)) {
                    excluded.insert(m.id.clone());
                }
            }
            if before == excluded.len() {
                break;
            }
        }
        if thread.is_none() {
            // Reuse the version already delivered for matching IDs so
            // promotion certifies the same evidence the model can cite.
            let messages: Vec<_> = self
                .delivered
                .values()
                .chain(
                    page.messages
                        .iter()
                        .filter(|m| !self.delivered.contains_key(&m.id)),
                )
                .filter(|m| !excluded.contains(&m.id))
                .collect();
            evidence::extend_descendants(&mut relevant, &messages);
        }
        let mut promoted_related_ids = Vec::new();
        for id in relevant.difference(&self.related) {
            if let Some(known) = self.delivered.get(id) {
                self.targets
                    .extend(evidence::targets(known).map(|(id, _)| id.to_owned()));
                if evidence::relative(known, anchor) == Some(Ordering::Greater)
                    && self.observation.after_status == "checked_empty"
                {
                    self.observation.after_status = "read".into();
                }
                self.nearby.remove(id);
                promoted_related_ids.push(id.clone());
            }
        }
        promoted_related_ids.sort_unstable();
        self.related.extend(relevant.iter().cloned());
        let mut tagged = Vec::new();
        for m in page.messages {
            if excluded.contains(&m.id) || self.delivered.contains_key(&m.id) {
                continue;
            }
            let direct = self.references.iter().any(|r| r.id == m.id);
            if direct {
                self.set_reference_status(&m.id, "available");
            }
            let nearby = !direct && thread.is_none() && !relevant.contains(&m.id);
            if nearby {
                self.nearby.insert(m.id.clone());
            } else if !direct || relevant.contains(&m.id) {
                self.related.insert(m.id.clone());
                if args.direction == Direction::After
                    && self.observation.after_status == "checked_empty"
                {
                    self.observation.after_status = "read".into();
                }
                self.targets
                    .extend(evidence::targets(&m).map(|(id, _)| id.to_owned()));
            }
            self.expand_observation(Some(m.created_at), Some(m.created_at));
            tagged.push(
                json!({"message":self.model(&m),"scope":if direct {"direct_reference"} else if nearby {"nearby_unverified"} else {"related"}}),
            );
            self.delivered.insert(m.id.clone(), m);
        }
        if next_cursor.is_none() && args.direction == Direction::After && end < through {
            page.gaps.insert("outside_nearby_window".into());
        }
        self.gaps.extend(page.gaps.iter().cloned());
        Ok(json!({
            "messages":tagged,"promoted_related_ids":promoted_related_ids,"through":through,"read_from":page.read_from,"read_to":page.read_to,
            "next_cursor":next_cursor,"gaps":page.gaps,
            "status":if next_cursor.is_some() || !page.gaps.is_empty() {"partial"} else if tagged.is_empty() && promoted_related_ids.is_empty() {"checked_no_additional_material"} else {"read"}
        }))
    }
}

// An expansion returns more of a previously delivered body. It is not a
// new observation or permission to replace its established source proof.
fn expand_same_version(
    mut old: FeishuEvidence,
    fresh: FeishuEvidence,
    raw: &Value,
    chat: &str,
) -> Option<FeishuEvidence> {
    fn disclosed(value: &Value) -> bool {
        !value.is_null() && !value.as_str().is_some_and(str::is_empty)
    }

    fn compatible_if_reported<T: PartialEq>(
        value: &Value,
        original: Option<T>,
        parsed: Option<T>,
    ) -> bool {
        !disclosed(value) || parsed.is_some() && parsed == original
    }

    let same_body = match (&old.source.content_key, &fresh.source.content_key) {
        (Some(old), Some(new)) => old == new,
        _ => fresh.text.starts_with(&old.text),
    };
    let a = &old.source;
    let b = &fresh.source;
    if old.id != fresh.id
        || !same_body
        || b.withdrawn
        || old.created_at != fresh.created_at
        || old.updated_at != fresh.updated_at
        || a.created_minute_precision != b.created_minute_precision
        || a.edited_minute_precision != b.edited_minute_precision
        || a.created_subsecond != b.created_subsecond
        || a.edited_subsecond != b.edited_subsecond
        || a.created_millis
            .zip(b.created_millis)
            .is_some_and(|(a, b)| a != b)
        || a.edited_millis
            .zip(b.edited_millis)
            .is_some_and(|(a, b)| a != b)
        || a.author_id
            .as_ref()
            .zip(b.author_id.as_ref())
            .is_some_and(|(a, b)| a != b)
        || !compatible_if_reported(
            &raw["thread_id"],
            a.thread_id.as_deref(),
            b.thread_id.as_deref(),
        )
        || !compatible_if_reported(&raw["root_id"], a.root_id.as_deref(), b.root_id.as_deref())
    {
        return None;
    }
    // Compare both accepted spellings directly. A missing optional field
    // is not a claim that the known relationship stopped existing.
    for field in ["parent_id", "reply_to"] {
        if !compatible_if_reported(&raw[field], a.reply_to.as_deref(), raw[field].as_str()) {
            return None;
        }
    }
    let quote_fields = ["quote_id", "quoted_message_id"];
    let invalid_id = |value: &Value| {
        !value
            .as_str()
            .is_some_and(|id| super::provider_id(id, "om_"))
    };
    if quote_fields
        .iter()
        .any(|field| disclosed(&raw[field]) && invalid_id(&raw[field]))
        || (disclosed(&raw["quoted_message_ids"])
            && raw["quoted_message_ids"]
                .as_array()
                .is_none_or(|ids| ids.iter().any(invalid_id)))
        || ((quote_fields.into_iter().any(|field| disclosed(&raw[field]))
            || disclosed(&raw["quoted_message_ids"]))
            && b.quote_ids != a.quote_ids)
    {
        return None;
    }
    // Native chat positions and positions within a topic are distinct
    // coordinate systems. Ignore a fallback chat position for topic rows.
    let position_field = if a.thread_id.is_some() {
        "thread_message_position"
    } else {
        "message_position"
    };
    if disclosed(&raw[position_field]) {
        let position = source_position(&raw[position_field])?;
        if a.position.is_some_and(|old| old != position) {
            return None;
        }
    }
    if disclosed(&raw["message_app_link"])
        && !locator::Link::parse(raw["message_app_link"].as_str()?)?.agrees(
            chat,
            a.thread_id.as_deref(),
            a.position,
        )
    {
        return None;
    }

    old.text = fresh.text;
    old.source.mention_bindings = fresh.source.mention_bindings;
    old.source.truncated = fresh.source.truncated;
    Some(old)
}
