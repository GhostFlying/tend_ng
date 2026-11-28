mod agent;
mod api;

use crate::store::{
    Store,
    feishu::{ContextCursor, ContextJob, ContextResult, FeishuMessage, RETENTION},
};
use anyhow::{Context, Result, ensure};
use protocol::FeishuEvidence;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{self, RecvTimeoutError, SyncSender},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tiny_http::Request;

const DAY: i64 = 86_400;
const OVERLAP: i64 = 15 * 60;
const SCAN_INTERVAL: Duration = Duration::from_secs(180);
const CONTEXT_PAGE_BUDGET: usize = 4;
const CONTEXT_MAX_PAGES: usize = 200;
const CONTEXT_MAX_IDS: usize = 512;

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
            agent::spawn(db, config.owner_open_id.clone(), assessment)?;
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

fn now() -> i64 {
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
            if let Err(error) = scan(&mut store, &config, now()) {
                // Never log provider stdout, stderr, cursors, identities or message bodies.
                let _ = store.feishu_scan_phase(
                    &config.owner_open_id,
                    "failed",
                    Some(error.category()),
                );
            } else if let Err(_error) = refresh_context(&mut store, &config) {
                // Context is a separately best-effort read, not evidence that discovery failed.
                eprintln!("Feishu suggestion context could not be recorded");
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

impl Config {
    fn read(&self, args: &[&str]) -> ScanResult<Value> {
        // A healthy Server must be able to leave a hung CLI call and scan again.
        // GNU timeout owns and terminates the command's process group.
        let mut command = Command::new("timeout");
        command
            .args(["--kill-after=3s", "25s"])
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

    fn verify_identity(&self) -> ScanResult<()> {
        let identity = self.read(&["whoami"]).map_err(|_| ScanError::Identity)?;
        if identity["brand"] != "feishu"
            || identity["identity"] != "user"
            || identity["available"] != true
            || identity["onBehalfOf"]["openId"] != self.owner_open_id
        {
            return Err(ScanError::Identity);
        }
        Ok(())
    }

    fn messages(&self, args: &[&str]) -> ScanResult<Value> {
        let response = self.read(args)?;
        if response["ok"] != true || response["identity"] != "user" {
            return Err(ScanError::Provider);
        }
        Ok(response["data"].clone())
    }
}

fn scan(store: &mut Store, config: &Config, end: i64) -> ScanResult<()> {
    let owner = &config.owner_open_id;
    store
        .feishu_scan_phase(owner, "running", None)
        .map_err(|_| ScanError::Storage)?;
    config.verify_identity()?;
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
            return Ok(());
        }
        cursor = through;
    }
}

fn search_window(
    store: &Store,
    config: &Config,
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
            let details = config.messages(&[
                "im",
                "+messages-mget",
                "--as",
                "user",
                "--message-ids",
                &ids.join(","),
                "--no-reactions",
                "--format",
                "json",
            ])?;
            for raw in rows(&details)? {
                let id = raw["message_id"].as_str().ok_or(ScanError::Coverage)?;
                let meta = expected.remove(id).ok_or(ScanError::Coverage)?;
                let direct = raw["mentions"].as_array().is_some_and(|mentions| {
                    mentions.iter().any(|mark| {
                        mark["id"]
                            .as_str()
                            .or_else(|| mark["id"]["open_id"].as_str())
                            == Some(config.owner_open_id.as_str())
                    })
                });
                if raw["deleted"] == true || !direct {
                    continue;
                }
                let sender = sender_open_id(raw).ok_or(ScanError::Coverage)?;
                if sender == config.owner_open_id {
                    continue;
                }
                let chat = meta["chat_id"].as_str().filter(|id| provider_id(id, "oc_"));
                let mut message = parse_message(store, raw, chat)?;
                if chat.is_some_and(|chat| chat != message.chat_id) {
                    return Err(ScanError::Coverage);
                }
                let created =
                    time(store, &meta["create_time"]).unwrap_or(message.evidence.created_at);
                let updated = time(store, &meta["update_time"])
                    .unwrap_or(message.evidence.updated_at)
                    .max(created);
                message.evidence.created_at = created;
                message.evidence.updated_at = updated;
                // Older CLI outputs can round to a minute. Keep the minute
                // containing the inclusive provider start bound.
                if created >= from - 59 && created <= through {
                    matches.push(message);
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
    page["messages"]
        .as_array()
        .or_else(|| page["items"].as_array())
        .map(Vec::as_slice)
        .ok_or(ScanError::Coverage)
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

fn applink(value: &str) -> Option<String> {
    let link = url::Url::parse(value).ok()?;
    (link.scheme() == "https"
        && link.host_str() == Some("applink.feishu.cn")
        && link.port().is_none()
        && link.username().is_empty()
        && link.password().is_none()
        && link.path().starts_with("/client/"))
    .then(|| link.to_string())
}

fn parse_message(
    store: &Store,
    raw: &Value,
    expected_chat: Option<&str>,
) -> ScanResult<FeishuMessage> {
    let id = raw["message_id"]
        .as_str()
        .filter(|id| provider_id(id, "om_"))
        .ok_or(ScanError::Coverage)?;
    let chat = raw["chat_id"]
        .as_str()
        .filter(|id| provider_id(id, "oc_"))
        .or(expected_chat)
        .ok_or(ScanError::Coverage)?;
    let created_at = time(store, &raw["create_time"])?;
    let updated_at = time(store, &raw["update_time"])
        .unwrap_or(created_at)
        .max(created_at);
    let text = raw["content"]
        .as_str()
        .filter(|text| !text.trim().is_empty())
        .unwrap_or("（非文本或来源未提供可显示的内容）")
        .chars()
        .take(1200)
        .collect::<String>();
    let href = raw["message_app_link"].as_str().and_then(applink).unwrap_or_else(|| {
        let position = raw["message_position"].as_i64().filter(|n| *n>=0)
            .or_else(|| raw["message_position"].as_str()?.parse::<i64>().ok().filter(|n| *n>=0));
        if let Some(position) = position {
            format!("https://applink.feishu.cn/client/chat/open?openChatId={chat}&position={position}")
        } else {
            format!("https://applink.feishu.cn/client/chat/open?openChatId={chat}")
        }
    });
    Ok(FeishuMessage {
        chat_id: chat.to_owned(),
        chat_name: raw["chat_name"]
            .as_str()
            .unwrap_or_default()
            .chars()
            .take(120)
            .collect(),
        thread_id: raw["thread_id"]
            .as_str()
            .filter(|id| provider_id(id, "omt_"))
            .map(str::to_owned),
        root_id: raw["root_id"]
            .as_str()
            .filter(|id| provider_id(id, "om_"))
            .map(str::to_owned),
        // The official shortcuts expose the parent of an ordinary reply as
        // reply_to. Accept the raw parent_id too, when present.
        parent_id: ["reply_to", "parent_id"]
            .into_iter()
            .filter_map(|key| raw[key].as_str())
            .find(|id| provider_id(id, "om_"))
            .map(str::to_owned),
        deleted: raw["deleted"] == true,
        evidence: FeishuEvidence {
            id: id.to_owned(),
            created_at,
            updated_at,
            text,
            href,
            sender: raw["sender"]["name"]
                .as_str()
                .filter(|v| !v.is_empty())
                .unwrap_or("未知发送者")
                .chars()
                .take(100)
                .collect(),
        },
    })
}

fn refresh_context(store: &mut Store, config: &Config) -> Result<()> {
    let now = now();
    let mut titles = HashMap::new();
    for job in store.feishu_context_jobs(&config.owner_open_id, now)? {
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
        let material = fetch_context(store, config, &job, now).ok();
        store.record_feishu_context(&config.owner_open_id, &job, name, material, now)?;
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

fn thread_position(href: &str, expected: &str) -> Option<u64> {
    let url = url::Url::parse(href).ok()?;
    if url.path() != "/client/thread/open" {
        return None;
    }
    let mut topic = false;
    let mut position = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "openthreadid" | "open_thread_id" if value == expected => topic = true,
            "thread_position" => position = value.parse().ok(),
            _ => {}
        }
    }
    topic.then_some(position).flatten()
}

fn fetch_context(
    store: &Store,
    config: &Config,
    job: &ContextJob,
    now: i64,
) -> ScanResult<ContextResult> {
    let thread = job
        .topic
        .as_deref()
        .filter(|topic| provider_id(topic, "omt_"));
    let from = job
        .cursor
        .as_ref()
        .map_or(job.first_at.max(now - RETENTION), |c| c.from);
    let through = job.cursor.as_ref().map_or(now, |c| c.through);
    let since = store.feishu_iso(from).map_err(|_| ScanError::Storage)?;
    let until = store.feishu_iso(through).map_err(|_| ScanError::Storage)?;
    // Ascending order lets a parent be discovered before a child across a page
    // or a poll boundary. For ordinary chats the cursor uses the same time bounds.
    let common = [
        "--as",
        "user",
        "--no-reactions",
        "--format",
        "json",
        "--order",
        "asc",
        "--page-size",
        "50",
    ];
    let mut args = if let Some(thread) = thread {
        vec!["im", "+threads-messages-list", "--thread", thread]
    } else {
        vec![
            "im",
            "+chat-messages-list",
            "--chat-id",
            &job.chat_id,
            "--start",
            &since,
            "--end",
            &until,
        ]
    };
    args.extend(common);
    let mut token = job.cursor.as_ref().map(|c| c.token.clone());
    let mut seen = HashSet::new();
    let mut complete = job.cursor.as_ref().is_none_or(|c| c.complete);
    let mut pages = job.cursor.as_ref().map_or(0, |c| c.pages);
    let mut found = Vec::new();
    let mut deleted = HashSet::new();
    let lower = job.first_at.max(now - RETENTION);
    for _ in 0..CONTEXT_PAGE_BUDGET {
        let mut query = args.clone();
        if let Some(token) = &token {
            seen.insert(token.clone());
            query.extend(["--page-token", token]);
        }
        let page = config.messages(&query)?;
        pages += 1;
        for row in rows(&page)? {
            complete &= row["thread_replies_error"] != true && row["thread_has_more"] != true;
            let embedded = row["thread_replies"].as_array().into_iter().flatten();
            for (raw, top_level) in
                std::iter::once((row, true)).chain(embedded.map(|row| (row, false)))
            {
                if raw["deleted"] == true {
                    if let Some(id) = raw["message_id"].as_str() {
                        deleted.insert(id.to_owned());
                    }
                    continue;
                }
                let Ok(message) = parse_message(store, raw, Some(&job.chat_id)) else {
                    complete = false;
                    continue;
                };
                if message.chat_id != job.chat_id || message.evidence.id == job.evidence.anchor.id {
                    continue;
                }
                let at = message.evidence.created_at;
                let coarse = minute_label(&raw["create_time"]);
                if at > through {
                    continue;
                }
                if now > through && thread.is_some() && coarse && at + 59 > through {
                    complete = false;
                    continue;
                }
                if let Some(thread) = thread
                    && lower == job.first_at
                    && at.div_euclid(60) == job.first_at.div_euclid(60)
                {
                    // A thread query has no precise start. A tied minute
                    // needs a provider position even when the stored anchor
                    // is itself rounded to the beginning of that minute.
                    match thread_position(&job.evidence.anchor.href, thread)
                        .zip(thread_position(&message.evidence.href, thread))
                    {
                        Some((anchor, reply)) if reply > anchor => {}
                        Some(_) => continue,
                        None => {
                            complete = false;
                            continue;
                        }
                    }
                } else if at < lower {
                    if !coarse || at + 59 < lower {
                        continue;
                    }
                    // The chat endpoint filters top-level rows before
                    // formatting. Its nested expansions do not share the
                    // precise lower bound.
                    if thread.is_some() || !top_level || from < lower {
                        complete = false;
                        continue;
                    }
                }
                found.push(message);
            }
        }
        token = match page["has_more"].as_bool() {
            Some(false) => None,
            Some(true) if pages < CONTEXT_MAX_PAGES => page["page_token"]
                .as_str()
                .filter(|next| !next.is_empty() && next.len() <= 4096 && !seen.contains(*next))
                .map(str::to_owned)
                .or_else(|| {
                    complete = false;
                    None
                }),
            _ => {
                complete = false;
                None
            }
        };
        if token.is_none() {
            break;
        }
    }
    let mut seeds: HashSet<String> = job.evidence.mentions.iter().map(|m| m.id.clone()).collect();
    seeds.insert(job.evidence.anchor.id.clone());
    if let Some(topic) = job.topic.as_ref().filter(|topic| provider_id(topic, "om_")) {
        seeds.insert(topic.clone());
    }
    let mut known = seeds.clone();
    if let Some(cursor) = &job.cursor {
        known.extend(
            cursor
                .known
                .iter()
                .filter(|id| !deleted.contains(*id))
                .cloned(),
        );
    }
    let mut replies: HashMap<_, _> = job
        .evidence
        .replies
        .iter()
        .filter(|e| !deleted.contains(&e.id) && e.created_at >= now - RETENTION)
        .map(|e| (e.id.clone(), e.clone()))
        .collect();
    known.extend(replies.keys().cloned());
    found.retain(|m| !seeds.contains(&m.evidence.id) && !deleted.contains(&m.evidence.id));
    let mut proven_edges = Vec::new();
    loop {
        let before = known.len();
        found.retain(|m| {
            let same_thread =
                thread.is_some_and(|id| m.thread_id.as_deref().is_none_or(|value| value == id));
            let mut replied = false;
            for parent in [m.root_id.as_ref(), m.parent_id.as_ref()]
                .into_iter()
                .flatten()
                .filter(|id| known.contains(*id))
            {
                if parent != &m.evidence.id {
                    replied = true;
                    proven_edges.push((m.evidence.id.clone(), parent.clone()));
                }
            }
            if same_thread || replied || known.contains(&m.evidence.id) {
                known.insert(m.evidence.id.clone());
                if replies
                    .get(&m.evidence.id)
                    .is_none_or(|old| old.updated_at <= m.evidence.updated_at)
                {
                    replies.insert(m.evidence.id.clone(), m.evidence.clone());
                }
                false
            } else {
                true
            }
        });
        if known.len() == before || found.is_empty() {
            break;
        }
    }
    let mut replies: Vec<_> = replies.into_values().collect();
    replies.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
    if replies.len() > 12 {
        replies.drain(..replies.len() - 12);
        complete = false;
    }
    let next = token.map(|token| {
        let mut known: Vec<_> = known.into_iter().filter(|id| !seeds.contains(id)).collect();
        known.sort_unstable();
        if known.len() > CONTEXT_MAX_IDS {
            known.truncate(CONTEXT_MAX_IDS);
            complete = false;
        }
        ContextCursor {
            from,
            through,
            token,
            pages,
            complete,
            known,
        }
    });
    Ok(ContextResult {
        replies,
        proven_edges,
        complete: complete && next.is_none(),
        next,
    })
}
