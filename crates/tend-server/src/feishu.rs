mod agent;
mod api;
mod context;
pub(crate) mod evidence;
pub(crate) mod locator;
mod reads;

use crate::store::{
    Store,
    feishu::{FeishuMessage, RETENTION},
};
use anyhow::{Context, Result, ensure};
use protocol::{FeishuEvidence, FeishuMentionBinding, FeishuPerson, FeishuSource};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{self, RecvTimeoutError, SyncSender},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tiny_http::Request;

const DAY: i64 = 86_400;
const OVERLAP: i64 = 15 * 60;
const SCAN_INTERVAL: Duration = Duration::from_secs(180);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    owner_open_id: String,
    #[serde(default)]
    profile: Option<String>,
    #[serde(default = "default_cli")]
    cli: PathBuf,
    #[serde(default)]
    agent: Option<agent::Config>,
}

fn default_cli() -> PathBuf {
    "lark-cli".into()
}

pub fn load(
    path: Option<&Path>,
    router_home: &Path,
    discussion_home: Option<&Path>,
) -> Result<Option<Config>> {
    path.map(|path| -> Result<Config> {
        let path = path
            .canonicalize()
            .context("resolve Feishu suggestions config")?;
        let root = path.parent().context("invalid Feishu config path")?;
        let mut config: Config = serde_json::from_reader(File::open(&path)?)
            .context("parse Feishu suggestions config")?;
        ensure!(
            provider_id(&config.owner_open_id, "ou_"),
            "Feishu config must pin the user's open ID"
        );
        if config.cli.components().count() > 1 {
            config.cli = root.join(&config.cli).canonicalize()?;
        }
        if let Some(agent) = &mut config.agent {
            agent.resolve(root, router_home, discussion_home)?;
        }
        Ok(config)
    })
    .transpose()
}

impl Config {
    pub fn owner(&self) -> &str {
        &self.owner_open_id
    }

    pub fn private_home(&self) -> Option<&Path> {
        self.agent.as_ref().map(agent::Config::private_home)
    }
}

pub fn serve(mut config: Option<Config>, db: &Path) -> Result<SyncSender<Request>> {
    spawn_retention(db)?;
    let (owner, assessor) = if let Some(config) = &mut config {
        if let Some(assessment) = config.agent.take() {
            let name = assessment.id.clone();
            agent::spawn(db, config.reader(), assessment)?;
            (Some(config.owner_open_id.clone()), Some(name))
        } else {
            (Some(config.owner_open_id.clone()), None)
        }
    } else {
        (None, None)
    };
    let scan = config.map(|config| spawn_scanner(db, config)).transpose()?;
    api::serve(db, owner, assessor, scan)
}

pub(crate) fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn spawn_retention(db: &Path) -> Result<()> {
    let mut store = Store::open(db)?;
    store.prune_feishu(now())?;
    thread::spawn(move || {
        loop {
            thread::sleep(Duration::from_secs(30));
            if store.prune_feishu(now()).is_err() {
                eprintln!("Feishu retention could not prune expired material");
            }
        }
    });
    Ok(())
}

fn spawn_scanner(db: &Path, config: Config) -> Result<SyncSender<()>> {
    let mut store = Store::open(db)?;
    let (tx, rx) = mpsc::sync_channel(1);
    store.interrupt_feishu_scan(&config.owner_open_id)?;
    thread::spawn(move || {
        loop {
            match scan(&mut store, &config.reader(), now()) {
                Err(error) => {
                    // Never log provider stdout, stderr, cursors, identities or message bodies.
                    let _ = store.feishu_scan_phase(
                        &config.owner_open_id,
                        "failed",
                        Some(error.category()),
                    );
                }
                Ok(person) if refresh_context(&mut store, &config.reader(), &person).is_err() => {
                    // Context is a separately best-effort read, not evidence that discovery failed.
                    eprintln!("Feishu suggestion context could not be recorded");
                }
                Ok(_) => {}
            }
            if rx.recv_timeout(SCAN_INTERVAL) == Err(RecvTimeoutError::Disconnected) {
                return;
            }
        }
    });
    Ok(tx)
}

#[derive(Clone, Copy, Debug)]
enum ScanError {
    Identity,
    Provider,
    Coverage,
    Storage,
}

impl ScanError {
    fn category(self) -> &'static str {
        match self {
            Self::Identity => "identity",
            Self::Provider => "provider_unavailable",
            Self::Coverage => "incomplete_search",
            Self::Storage => "storage",
        }
    }
}

type ScanResult<T> = std::result::Result<T, ScanError>;

fn provider_id(value: &str, prefix: &str) -> bool {
    value.starts_with(prefix) && value.len() > prefix.len() && protocol::valid_id(value)
}

#[derive(Clone)]
struct Reader {
    deadline: Option<Instant>,
    owner_open_id: String,
    cli: PathBuf,
    profile: Option<String>,
}

impl Config {
    fn reader(&self) -> Reader {
        Reader {
            deadline: None,
            owner_open_id: self.owner_open_id.clone(),
            cli: self.cli.clone(),
            profile: self.profile.clone(),
        }
    }
}

impl Reader {
    fn read(&self, args: &[&str]) -> ScanResult<Value> {
        // A healthy Server must be able to leave a hung CLI call and scan again.
        // GNU timeout owns and terminates the command's process group.
        let (limit, grace) = if let Some(deadline) = self.deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining <= Duration::from_millis(300) {
                return Err(ScanError::Provider);
            }
            ((remaining.as_secs_f64() - 0.25).min(25.0), "0.25s")
        } else {
            (25.0, "3s")
        };
        let mut command = Command::new("timeout");
        command
            .arg(format!("--kill-after={grace}"))
            .arg(format!("{limit:.3}s"))
            .arg(&self.cli)
            .args(args);
        if let Some(profile) = &self.profile {
            command.args(["--profile", profile]);
        }
        let output = command
            .env("TZ", "UTC")
            .env_remove("TEND_TOKEN")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map_err(|_| ScanError::Provider)?;
        if !output.status.success() {
            return Err(ScanError::Provider);
        }
        serde_json::from_slice(&output.stdout).map_err(|_| ScanError::Coverage)
    }

    fn verify_identity(&self) -> ScanResult<FeishuPerson> {
        let identity = self.read(&["whoami"]).map_err(|_| ScanError::Identity)?;
        if identity["brand"] != "feishu"
            || identity["identity"] != "user"
            || identity["available"] != true
            || identity["onBehalfOf"]["openId"] != self.owner_open_id
        {
            return Err(ScanError::Identity);
        }
        let person = &identity["onBehalfOf"];
        Ok(FeishuPerson {
            id: self.owner_open_id.clone(),
            name: person["userName"]
                .as_str()
                .or_else(|| person["name"].as_str())
                .or_else(|| person["displayName"].as_str())
                .unwrap_or_default()
                .chars()
                .take(100)
                .collect(),
        })
    }

    // Use the same native endpoints as the CLI shortcuts, without the
    // formatter that drops parent_id when thread_id is also present.
    fn message_api(&self, path: &str, params: Value) -> ScanResult<Value> {
        self.messages(&[
            "api",
            "GET",
            path,
            "--as",
            "user",
            "--params",
            &params.to_string(),
        ])
    }

    fn mget(&self, ids: &[&str]) -> ScanResult<Value> {
        self.message_api(
            "/open-apis/im/v1/messages/mget",
            json!({
                "message_ids": ids, "with_sender_name": true,
                "card_msg_content_type": "raw_card_content",
            }),
        )
    }

    fn messages(&self, args: &[&str]) -> ScanResult<Value> {
        let response = self.read(args)?;
        if response["ok"] != true || response["identity"] != "user" {
            return Err(ScanError::Provider);
        }
        Ok(response["data"].clone())
    }
}

fn scan(store: &mut Store, config: &Reader, end: i64) -> ScanResult<FeishuPerson> {
    let owner = &config.owner_open_id;
    store
        .feishu_scan_phase(owner, "running", None)
        .map_err(|_| ScanError::Storage)?;
    let person = config.verify_identity()?;
    let checkpoint = store
        .feishu_checkpoint(owner)
        .map_err(|_| ScanError::Storage)?;
    // Start at the existing checkpoint even after a long outage, not at a
    // freshly computed lookback. Replaying an overlap is safe by message ID.
    let mut cursor = checkpoint.unwrap_or(end - 3 * DAY).min(end);
    if checkpoint.is_some_and(|last| last < end - 3 * DAY) {
        store
            .feishu_scan_phase(owner, "catching_up", None)
            .map_err(|_| ScanError::Storage)?;
    }
    loop {
        let through = (cursor + DAY).min(end);
        let from = cursor - OVERLAP;
        let mut messages = search_window(store, config, from, through)?;
        store
            .record_feishu_window(owner, through, now(), through == end, &mut messages)
            .map_err(|_| ScanError::Storage)?;
        if through == end {
            return Ok(person);
        }
        cursor = through;
    }
}

fn search_window(
    store: &Store,
    config: &Reader,
    from: i64,
    through: i64,
) -> ScanResult<Vec<FeishuMessage>> {
    let start = store.feishu_iso(from).map_err(|_| ScanError::Storage)?;
    let end = store.feishu_iso(through).map_err(|_| ScanError::Storage)?;
    let body = json!({"query":"","filter":{"is_at_me":true,"time_range":{"start_time":start,"end_time":end}}}).to_string();
    let mut seen_pages = HashSet::new();
    let mut token = None::<String>;
    let mut matches = Vec::new();
    loop {
        // The search shortcut reports the number of *hydrated* rows, not the
        // raw number of hits. Compare IDs ourselves so a partially successful
        // mget cannot silently advance the scan cursor.
        let mut params = json!({"page_size":"50"});
        if let Some(token) = &token {
            params["page_token"] = json!(token);
        }
        let page = config.messages(&[
            "api",
            "POST",
            "/open-apis/im/v1/messages/search",
            "--as",
            "user",
            "--params",
            &params.to_string(),
            "--data",
            &body,
        ])?;
        let more = page["has_more"].as_bool().ok_or(ScanError::Coverage)?;
        let hits = match page["items"].as_array() {
            Some(items) => items.as_slice(),
            None if !more
                && page["items"].is_null()
                && page.as_object().is_some_and(|m| m.contains_key("has_more")) =>
            {
                &[]
            }
            None => return Err(ScanError::Coverage),
        };
        let mut expected = HashMap::new();
        for hit in hits {
            let meta = &hit["meta_data"];
            let id = meta["message_id"]
                .as_str()
                .filter(|id| provider_id(id, "om_"))
                .ok_or(ScanError::Coverage)?;
            expected.insert(id, meta);
        }
        if !expected.is_empty() {
            let mut ids: Vec<_> = expected.keys().copied().collect();
            ids.sort_unstable();
            let details = config.mget(&ids)?;
            for raw in rows(&details)? {
                let id = raw["message_id"].as_str().ok_or(ScanError::Coverage)?;
                let meta = expected.remove(id).ok_or(ScanError::Coverage)?;
                let direct = people(raw)
                    .iter()
                    .any(|person| person.id == config.owner_open_id);
                if direct && raw["deleted"] != true && sender_open_id(raw).is_none() {
                    return Err(ScanError::Coverage);
                }
                let is_direct = direct
                    && raw["deleted"] != true
                    && sender_open_id(raw) != Some(config.owner_open_id.as_str());
                if !is_direct
                    && !store
                        .feishu_known_mention(&config.owner_open_id, id)
                        .map_err(|_| ScanError::Storage)?
                {
                    continue;
                }
                let chat = meta["chat_id"].as_str().filter(|id| provider_id(id, "oc_"));
                let mut message = parse_message(store, raw, chat)?;
                if chat.is_some_and(|chat| chat != message.chat_id) {
                    return Err(ScanError::Coverage);
                }
                // The native mget row identifies the body and its version.
                // Search index timestamps can describe a different version.
                message.direct_mention = is_direct;
                if message.evidence.created_at >= from && message.evidence.created_at <= through {
                    if message.evidence.source.edit_time_unknown && is_direct {
                        // Discover this @ without admitting its current body as
                        // evidence. A later exact reread can fill the gap.
                        message.evidence.text = "（编辑时间不明，暂不能核实原文版本）".into();
                        message.evidence.source.reply_to = None;
                        message.evidence.source.root_id = None;
                        message.evidence.source.quote_ids.clear();
                        message.evidence.source.content_key = None;
                        message.evidence.source.mention_bindings.clear();
                        message.evidence.source.truncated = false;
                        matches.push(message);
                        continue;
                    }
                    if evidence::visible(&message.evidence, now()) {
                        matches.push(message);
                    }
                }
            }
            if !expected.is_empty() {
                return Err(ScanError::Coverage);
            }
        }
        // A server-side coverage notice is not success just because the page
        // was readable. Do not claim the missing range is complete.
        if page["notice"]
            .as_str()
            .is_some_and(|notice| !notice.is_empty())
        {
            return Err(ScanError::Coverage);
        }
        if !more {
            return Ok(matches);
        }
        let next = page["page_token"]
            .as_str()
            .filter(|token| !token.is_empty() && token.len() <= 4096)
            .ok_or(ScanError::Coverage)?;
        if !seen_pages.insert(next.to_owned()) || seen_pages.len() > 200 {
            return Err(ScanError::Coverage);
        }
        token = Some(next.to_owned());
    }
}

fn rows(page: &Value) -> ScanResult<&[Value]> {
    page["items"]
        .as_array()
        .map(Vec::as_slice)
        .ok_or(ScanError::Coverage)
}

fn people(raw: &Value) -> Vec<FeishuPerson> {
    let mut people: Vec<_> = raw["mentions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|mark| {
            let id = mention_open_id(mark)?;
            Some(FeishuPerson {
                id: id.into(),
                name: mark["name"]
                    .as_str()
                    .unwrap_or_default()
                    .chars()
                    .take(100)
                    .collect(),
            })
        })
        .collect();
    people.sort_by(|a, b| a.id.cmp(&b.id));
    people.dedup_by(|a, b| a.id == b.id);
    people
}

fn mention_open_id(mark: &Value) -> Option<&str> {
    mark["id"]
        .as_str()
        .or_else(|| mark["id"]["open_id"].as_str())
        .filter(|id| provider_id(id, "ou_"))
}

fn sender_open_id(raw: &Value) -> Option<&str> {
    let sender = &raw["sender"];
    sender["id"]["open_id"].as_str().or_else(|| {
        let id = sender["id"].as_str()?;
        // App sender IDs are not the current user, but are still valid authors.
        match sender["id_type"].as_str() {
            Some("open_id") if provider_id(id, "ou_") => Some(id),
            _ if sender["sender_type"] == "app" && !id.is_empty() => Some(id),
            _ => None,
        }
    })
}

fn time(store: &Store, value: &Value) -> ScanResult<i64> {
    if let Some(epoch) = value.as_i64().or_else(|| value.as_str()?.parse().ok()) {
        return Ok(if epoch > 100_000_000_000 {
            epoch / 1000
        } else {
            epoch
        });
    }
    store
        .feishu_epoch(value.as_str().ok_or(ScanError::Coverage)?)
        .map_err(|_| ScanError::Storage)?
        .ok_or(ScanError::Coverage)
}

fn parse_message(
    store: &Store,
    raw: &Value,
    expected_chat: Option<&str>,
) -> ScanResult<FeishuMessage> {
    parse_excerpt(store, raw, expected_chat, 1200)
}

fn parse_excerpt(
    store: &Store,
    raw: &Value,
    expected_chat: Option<&str>,
    excerpt_limit: usize,
) -> ScanResult<FeishuMessage> {
    let id = raw["message_id"]
        .as_str()
        .filter(|id| provider_id(id, "om_"))
        .ok_or(ScanError::Coverage)?;
    let chat = match raw["chat_id"].as_str() {
        Some(id) if provider_id(id, "oc_") => id,
        Some(_) => return Err(ScanError::Coverage),
        None => expected_chat.ok_or(ScanError::Coverage)?,
    };
    let created_at = time(store, &raw["create_time"])?;
    let edited = time(store, &raw["update_time"])
        .ok()
        .filter(|at| *at >= created_at);
    let updated_at = edited.unwrap_or(created_at);
    let edit_time_unknown = raw["updated"] == true
        && (edited.is_none()
            || timestamp_ms(&raw["create_time"])
                .zip(timestamp_ms(&raw["update_time"]))
                .is_some_and(|(created, edited)| edited <= created));
    let withdrawn = raw["deleted"] == true;
    let content = message_text(raw).filter(|body| !body.text.trim().is_empty());
    let display = content.as_ref().map(|body| body.text.as_str());
    let text = if withdrawn {
        "（原消息已撤回）".into()
    } else {
        display
            .unwrap_or("（非文本或来源未提供可显示的内容）")
            .chars()
            .take(excerpt_limit)
            .collect()
    };
    let id_field = |key: &str, prefix: &str| {
        raw[key]
            .as_str()
            .filter(|id| provider_id(id, prefix))
            .map(str::to_owned)
    };
    let reply_to = id_field("reply_to", "om_").or_else(|| id_field("parent_id", "om_"));
    // Trust only structured source IDs, never an ID or URL in rendered text.
    let mut quote_ids: Vec<_> = ["quote_id", "quoted_message_id"]
        .into_iter()
        .filter_map(|key| id_field(key, "om_"))
        .collect();
    if let Some(quotes) = raw["quoted_message_ids"].as_array() {
        quote_ids.extend(
            quotes
                .iter()
                .filter_map(Value::as_str)
                .filter(|id| provider_id(id, "om_"))
                .map(str::to_owned),
        );
    }
    quote_ids.retain(|quote| quote != id);
    quote_ids.sort_unstable();
    quote_ids.dedup();
    let quote_metadata_reported = ["quote_id", "quoted_message_id", "quoted_message_ids"]
        .iter()
        .any(|key| !raw[key].is_null() && !raw[key].as_str().is_some_and(str::is_empty));
    let mentions = if withdrawn { Vec::new() } else { people(raw) };
    let content_key = content.as_ref().filter(|_| !withdrawn).map(|body| {
        // Include structural positions so a literal marker cannot stand
        // in for an actual @, even when both look alike in normalized text.
        let (bound, positions) = evidence::bound_text(&body.text, &body.bindings);
        let key = json!([bound, positions]);
        format!("{:x}", Sha256::digest(key.to_string()))
    });
    let mention_bindings = content
        .as_ref()
        .filter(|_| !withdrawn)
        .into_iter()
        .flat_map(|body| &body.bindings)
        .filter(|binding| binding.end <= excerpt_limit)
        .cloned()
        .collect();
    let thread_id = id_field("thread_id", "omt_");
    let root_id = id_field("root_id", "om_");
    let (scope_position, href, chat_link_only) = locator::resolve(raw, chat, thread_id.as_deref());
    let links_unread = display.is_some_and(|s| {
        s.contains("https://") || s.contains("http://") || s.contains("<a ") || s.contains("www.")
    });
    Ok(FeishuMessage {
        chat_id: chat.to_owned(),
        chat_name: raw["chat_name"]
            .as_str()
            .unwrap_or_default()
            .chars()
            .take(120)
            .collect(),
        thread_id: thread_id.clone(),
        direct_mention: false,
        evidence: FeishuEvidence {
            id: id.to_owned(),
            created_at,
            updated_at,
            text,
            href,
            source: FeishuSource {
                author_id: sender_open_id(raw).map(str::to_owned),
                mentions,
                mention_bindings,
                reply_to,
                root_id,
                quote_ids,
                quote_metadata_reported,
                thread_id,
                position: scope_position,
                withdrawn,
                truncated: !withdrawn
                    && display.is_some_and(|text| text.chars().nth(excerpt_limit).is_some()),
                content_key,
                non_text: raw["msg_type"]
                    .as_str()
                    .is_some_and(|kind| !matches!(kind, "text" | "post"))
                    || content.is_none(),
                links_unread: !withdrawn && links_unread,
                chat_link_only,
                created_minute_precision: minute_label(&raw["create_time"]),
                edited_minute_precision: minute_label(&raw["update_time"])
                    && (updated_at != created_at || raw["updated"] == true),
                edit_time_unknown,
                created_subsecond: subsecond(&raw["create_time"]),
                edited_subsecond: (updated_at != created_at || raw["updated"] == true)
                    && subsecond(&raw["update_time"]),
                created_millis: timestamp_ms(&raw["create_time"]),
                edited_millis: edited
                    .and_then(|_| timestamp_ms(&raw["update_time"]))
                    .or_else(|| {
                        (!edit_time_unknown && updated_at == created_at)
                            .then(|| timestamp_ms(&raw["create_time"]))
                            .flatten()
                    }),
            },
            sender: raw["sender"]["sender_name"]
                .as_str()
                .or_else(|| raw["sender"]["name"].as_str())
                .filter(|v| !v.is_empty())
                .unwrap_or("未知发送者")
                .chars()
                .take(100)
                .collect(),
        },
    })
}

fn source_position(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}

fn timestamp_ms(value: &Value) -> Option<i64> {
    let epoch = value.as_i64().or_else(|| value.as_str()?.parse().ok())?;
    if epoch > 100_000_000_000 {
        Some(epoch)
    } else {
        epoch.checked_mul(1000)
    }
}

fn subsecond(value: &Value) -> bool {
    timestamp_ms(value).is_some_and(|epoch| epoch % 1000 != 0)
}

#[derive(Default)]
struct MessageText {
    text: String,
    length: usize,
    bindings: Vec<FeishuMentionBinding>,
}

impl MessageText {
    fn push(&mut self, text: &str) {
        self.text.push_str(text);
        self.length += text.chars().count();
    }

    fn mention(&mut self, name: &str, id: Option<&str>) {
        let start = self.length;
        self.push("@");
        self.push(name);
        if let Some(id) = id {
            self.bindings.push(FeishuMentionBinding {
                start,
                end: self.length,
                id: id.into(),
            });
        }
    }
}

fn message_text(raw: &Value) -> Option<MessageText> {
    let body: Value = serde_json::from_str(raw["body"]["content"].as_str()?).ok()?;
    let marks = raw["mentions"].as_array().into_iter().flatten();
    if raw["msg_type"] == "text" {
        let mut remaining = body["text"].as_str()?;
        let mut marks: Vec<_> = marks
            .filter_map(|m| {
                Some((
                    m["key"].as_str()?,
                    m["name"].as_str().unwrap_or_default(),
                    mention_open_id(m),
                ))
            })
            .filter(|(key, _, _)| key.starts_with("@_user_"))
            .collect();
        marks.sort_by_key(|(key, _, _)| std::cmp::Reverse(key.len()));
        let mut output = MessageText::default();
        while let Some(at) = remaining.find('@') {
            output.push(&remaining[..at]);
            remaining = &remaining[at..];
            if let Some((key, name, id)) = marks.iter().find(|(key, _, _)| {
                remaining
                    .strip_prefix(key)
                    .is_some_and(|tail| !tail.as_bytes().first().is_some_and(u8::is_ascii_digit))
            }) {
                output.mention(
                    if name.is_empty() {
                        id.unwrap_or("未知")
                    } else {
                        name
                    },
                    *id,
                );
                remaining = &remaining[key.len()..];
            } else {
                output.push("@");
                remaining = &remaining[1..];
            }
        }
        output.push(remaining);
        return Some(output);
    }
    if raw["msg_type"] != "post" {
        return None;
    }
    let post = if body["content"].is_array() {
        &body
    } else {
        body.get("zh_cn")
            .or_else(|| body.get("en_us"))
            .filter(|post| post["content"].is_array())
            .or_else(|| {
                body.as_object()?
                    .values()
                    .find(|post| post["content"].is_array())
            })?
    };
    let mut output = MessageText::default();
    if let Some(title) = post["title"].as_str().filter(|t| !t.is_empty()) {
        output.push(title);
        output.push("\n");
    }
    for (index, line) in post["content"]
        .as_array()?
        .iter()
        .filter_map(Value::as_array)
        .enumerate()
    {
        if index != 0 {
            output.push("\n");
        }
        for part in line {
            match part["tag"].as_str() {
                Some("text" | "md") => output.push(part["text"].as_str().unwrap_or_default()),
                Some("a") => {
                    let href = part["href"].as_str().unwrap_or_default();
                    output.push(
                        part["text"]
                            .as_str()
                            .filter(|s| !s.is_empty())
                            .unwrap_or(href),
                    );
                    if !href.is_empty() {
                        output.push(&format!(" ({href})"));
                    }
                }
                Some("at") => {
                    let target = part["user_id"].as_str().unwrap_or_default();
                    let mark = marks
                        .clone()
                        .find(|m| m["key"] == target || mention_open_id(m) == Some(target));
                    let id = mark
                        .and_then(mention_open_id)
                        .or_else(|| provider_id(target, "ou_").then_some(target));
                    let name = part["user_name"]
                        .as_str()
                        .or_else(|| mark.and_then(|m| m["name"].as_str()))
                        .filter(|name| !name.is_empty())
                        .or(id)
                        .unwrap_or("未知");
                    output.mention(name, id);
                }
                _ => output.push("（非文本内容）"),
            }
        }
    }
    Some(output)
}

fn refresh_context(store: &mut Store, config: &Reader, person: &FeishuPerson) -> Result<()> {
    let current = now();
    let mut titles = HashMap::new();
    for job in store.feishu_context_jobs(&config.owner_open_id, current)? {
        let name = if job.evidence.chat_name.is_empty() {
            titles
                .entry(job.chat_id.clone())
                .or_insert_with(|| {
                    let info = config
                        .messages(&[
                            "im",
                            "chats",
                            "get",
                            "--as",
                            "user",
                            "--chat-id",
                            &job.chat_id,
                        ])
                        .ok()?;
                    info["name"]
                        .as_str()
                        .filter(|name| !name.is_empty())
                        .map(|name| name.chars().take(120).collect::<String>())
                })
                .as_deref()
        } else {
            None
        };
        let material = context::fetch(store, config, &job, now()).ok();
        store.record_feishu_context(
            &config.owner_open_id,
            &job,
            person,
            name,
            material,
            self::now(),
        )?;
    }
    Ok(())
}

fn minute_label(value: &Value) -> bool {
    value.as_str().is_some_and(|label| {
        label.len() == 16
            && label
                .as_bytes()
                .get(10)
                .is_some_and(|b| matches!(b, b' ' | b'T'))
            && label.as_bytes().get(13) == Some(&b':')
    })
}
