//! Live request provenance for an already identity-fenced App Server stream.
//!
//! TokenCount also reports estimates, limit markers and restored context. Pair
//! the wire values with exact per-response evidence from this turn's append
//! segment. Never substitute counts from the file for missing wire fields.
use protocol::{
    ModelRepair, RequestMeter, TokenCounts, TokenModel, TokenSourceGap,
    TokenSourceGap::SourceUnreadable,
};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

/// Response identity exists even when the provider omits billable usage.
pub struct Response {
    pub id: String,
    pub meter: RequestMeter,
}

impl Response {
    pub(crate) fn from_event(params: &Value) -> Option<Self> {
        let id = params["responseId"]
            .as_str()
            .filter(|id| valid_native_id(id))?;
        let fields = [
            "totalTokens",
            "inputTokens",
            "cachedInputTokens",
            "cacheWriteInputTokens",
            "outputTokens",
            "reasoningOutputTokens",
        ];
        let read = |value: &Value| value.as_i64().filter(|count| *count >= 0);
        let original = &params["usageMetadata"]["metadata"];
        let provider_counts = original.is_object()
            && ["total_tokens", "input_tokens", "output_tokens"]
                .iter()
                .any(|key| original.get(key).is_some());
        let values = if provider_counts {
            let input = &original["input_tokens_details"];
            let output = &original["output_tokens_details"];
            [
                &original["total_tokens"],
                &original["input_tokens"],
                &input["cached_tokens"],
                &input["cache_write_tokens"],
                &original["output_tokens"],
                &output["reasoning_tokens"],
            ]
            .map(read)
        } else {
            fields.map(|field| read(&params["usage"][field]))
        };
        let counts = TokenCounts::from_fields(values);
        let counts = if provider_counts {
            counts
        } else {
            counts.without_unproven_optional_zeros()
        };
        let model = request_model(&params["responseModel"], &params["requestedModel"]);
        Some(Self {
            id: id.to_owned(),
            meter: RequestMeter {
                counts,
                model: Some(model),
                provider_counts,
            },
        })
    }
}

pub use protocol::valid_native_id;

pub(crate) struct MeteredTurn<'a, H> {
    inner: &'a mut H,
    collector: Option<Collector>,
    turn: Option<String>,
}

impl<'a, H: crate::TurnHandler> MeteredTurn<'a, H> {
    pub(crate) fn new(inner: &'a mut H, collector: Option<Collector>) -> Self {
        Self {
            inner,
            collector,
            turn: None,
        }
    }

    pub(crate) fn flush(&mut self, thread: &str) {
        if let (Some(turn), Some(collector)) = (&self.turn, &mut self.collector) {
            match collector.matched(thread, turn) {
                Ok(records) => {
                    for (id, meter) in records {
                        self.inner
                            .request_usage(thread, turn, Response { id, meter });
                    }
                }
                Err(error) => eprintln!("Read native request evidence: {error}"),
            }
        }
    }
}

impl<H: crate::TurnHandler> crate::TurnHandler for MeteredTurn<'_, H> {
    fn started(&mut self, turn: &str) -> anyhow::Result<()> {
        self.inner.started(turn)?;
        self.turn = Some(turn.to_owned());
        Ok(())
    }

    fn request_usage(&mut self, thread: &str, turn: &str, response: Response) {
        self.inner.request_usage(thread, turn, response);
    }

    fn child_request_usage(
        &mut self,
        root_thread: &str,
        root_turn: &str,
        thread: &str,
        turn: &str,
        response: Response,
    ) {
        self.inner
            .child_request_usage(root_thread, root_turn, thread, turn, response);
    }

    fn usage(&mut self, thread: &str, turn: &str, value: &Value) -> anyhow::Result<()> {
        if let Some(collector) = &mut self.collector {
            collector.notice(value);
            self.flush(thread);
        }
        self.inner.usage(thread, turn, value)
    }

    fn tool(&mut self, call: crate::ToolCall) -> anyhow::Result<crate::ToolOutput> {
        self.flush(&call.thread_id);
        self.inner.tool(call)
    }
}

pub type Rollouts = std::collections::HashMap<String, Option<PathBuf>>;

/// Index only Codex's two session directories in the frozen home. Do not
/// descend through symlinked entries or trust any pathname supplied by Server.
pub fn rollout_index(home: &Path) -> Result<Rollouts, TokenSourceGap> {
    let mut result = Rollouts::new();
    let mut dirs = vec![home.join("sessions"), home.join("archived_sessions")];
    while let Some(dir) = dirs.pop() {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => return Err(SourceUnreadable),
        };
        for entry in entries {
            let entry = entry.map_err(|_| SourceUnreadable)?;
            let kind = entry.file_type().map_err(|_| SourceUnreadable)?;
            if kind.is_dir() {
                dirs.push(entry.path());
            } else if kind.is_file()
                && let Some(name) = entry.file_name().to_str()
                // Codex normally uses UUIDs, but its thread IDs are opaque
                // here. Index by the suffix after its fixed ISO timestamp.
                && let Some(thread) = rollout_thread(name)
            {
                result
                    .entry(thread.to_owned())
                    .and_modify(|slot| *slot = None)
                    .or_insert_with(|| Some(entry.path()));
            }
        }
    }
    Ok(result)
}

fn rollout_thread(name: &str) -> Option<&str> {
    let stem = name.strip_prefix("rollout-")?.strip_suffix(".jsonl")?;
    let (date, thread) = stem.split_at_checked(20)?;
    (date.ends_with('-') && !thread.is_empty()).then_some(thread)
}
/// Build a home-local index of native children only for an explicit job.
pub struct ChildRollouts {
    sessions: BTreeMap<String, Vec<(String, PathBuf)>>,
    unreadable: bool,
}

impl ChildRollouts {
    pub fn from_rollouts(rollouts: &Rollouts) -> Self {
        let mut index = Self {
            sessions: BTreeMap::new(),
            unreadable: false,
        };
        for (thread, path) in rollouts {
            let Some(path) = path else {
                index.unreadable = true;
                continue;
            };
            if let Some(session) = child_session(path, thread) {
                index
                    .sessions
                    .entry(session)
                    .or_default()
                    .push((thread.clone(), path.clone()));
            }
        }
        index
    }

    pub fn snapshot(
        &self,
        session: &str,
        root_turn: &str,
        active: bool,
    ) -> (Vec<protocol::NativeTokenRecord>, Option<TokenSourceGap>) {
        let mut found = Vec::new();
        let mut gap = self.unreadable.then_some(TokenSourceGap::SourceAmbiguous);
        if let Some(children) = self.sessions.get(session) {
            for (thread, path) in children {
                let result = File::open(path)
                    .map_err(|_| TokenSourceGap::SourceUnreadable)
                    .and_then(|file| {
                        snapshot(file, thread, Scope::Child { session, root_turn }, active)
                    });
                match result {
                    Ok(records) => found.extend(records),
                    Err(error) => {
                        gap.get_or_insert(error);
                    }
                }
            }
        }
        (found, gap)
    }
}

fn child_session(path: &Path, thread: &str) -> Option<String> {
    let mut line = Vec::new();
    BufReader::new(File::open(path).ok()?)
        .take(MAX_LINE)
        .read_until(b'\n', &mut line)
        .ok()?;
    let value: Value = serde_json::from_slice(&line).ok()?;
    let payload = &value["payload"];
    let session = payload["session_id"]
        .as_str()
        .filter(|session| valid_native_id(session))?;
    if value["type"] != "session_meta"
        || payload["id"] != thread
        || session == thread
        || !payload["source"]["subagent"].is_object()
    {
        return None;
    }
    Some(session.to_owned())
}

const RECORD: &str = "token_usage_record";
const MAX_LINE: u64 = 1024 * 1024;

pub struct Collector {
    path: PathBuf,
    file: Option<File>,
    offset: u64,
    scanned_end: u64,
    notices: BTreeMap<i64, Vec<Notice>>,
    unmatched: usize,
    evidence: BTreeMap<i64, Vec<NativeRecord>>,
    dirty: BTreeSet<i64>,
    model: TurnModel,
}

struct Notice {
    last: TokenCounts,
    matched: bool,
}

#[derive(Deserialize)]
struct Envelope {
    payload: NativeRecord,
}

#[derive(Deserialize)]
struct NativeRecord {
    thread_id: String,
    turn_id: String,
    response_id: String,
    usage: TokenCounts,
    thread_token_usage: Cursor,
    #[serde(default)]
    response_model: Value,
    #[serde(default)]
    requested_model: Value,
    #[serde(skip)]
    model: Option<TokenModel>,
}

#[derive(Deserialize)]
struct Cursor {
    total_tokens: i64,
}

impl NativeRecord {
    fn valid(&self) -> bool {
        let cursor = self.thread_token_usage.total_tokens;
        !self.response_id.is_empty()
            && self.response_id.len() <= 256
            && cursor >= 0
            && self.usage.valid()
            && self.usage.total_tokens.is_none_or(|last| last <= cursor)
    }
}

fn model_id(v: &Value) -> Option<String> {
    v.as_str()
        .filter(|id| TokenModel::valid_id(id))
        .map(str::to_owned)
}

fn request_model(actual: &Value, selected: &Value) -> TokenModel {
    if let Some(id) = model_id(actual) {
        TokenModel::Response(id)
    } else if let Some(id) = model_id(selected) {
        TokenModel::RequestSelection(id)
    } else {
        TokenModel::Unresolved
    }
}

/// The selected model is sufficient for attribution. Unkeyed reroute
/// notifications may refer to another attempt, so cannot correct it.
#[derive(Default)]
struct TurnModel {
    current: bool,
    model: Option<TokenModel>,
    legacy_rerouted: bool,
}

impl TurnModel {
    fn observe(&mut self, kind: &str, payload: &Value, turn: &str, legacy: bool) {
        if kind == "turn_context" {
            self.current = payload["turn_id"].as_str() == Some(turn);
            let selected = model_id(&payload["model"]).map(TokenModel::TurnContext);
            if !legacy {
                self.model = selected.filter(|_| self.current).map(|current| {
                    // Preserve proof of equivalent spellings in successive
                    // contexts. A genuine change still selects the latest.
                    TokenModel::merge(self.model.clone(), Some(current.clone()))
                        .filter(|model| matches!(model.basis(), TokenModel::TurnContext(_)))
                        .unwrap_or(current)
                });
            } else if self.current {
                self.model = TokenModel::merge_legacy(
                    self.model.take(),
                    Some(selected.unwrap_or(TokenModel::Ambiguous)),
                );
            }
        } else if kind == "event_msg"
            && payload["type"] == "model_reroute"
            && payload["turn_id"]
                .as_str()
                .map_or(self.current, |id| id == turn)
        {
            self.legacy_rerouted = true;
        }
    }

    fn for_response(&self, actual: &Value, selected: &Value, legacy: bool) -> TokenModel {
        if legacy {
            // Preserve existing root-only backfill snapshot digests, even
            // when the previous scanner could not attribute model evidence.
            if let Some(id) = model_id(actual) {
                return TokenModel::Response(id);
            }
            if !actual.is_null() {
                return TokenModel::LegacyResponseConflict;
            }
            if self.legacy_rerouted {
                return TokenModel::Ambiguous;
            }
        }
        let selected = if legacy { &Value::Null } else { selected };
        match request_model(actual, selected) {
            TokenModel::Unresolved if self.current => {
                self.model.clone().unwrap_or(TokenModel::Unresolved)
            }
            model => model,
        }
    }

    fn for_history(&self, actual: &Value, selected: &Value, legacy: bool) -> TokenModel {
        let model = self.for_response(actual, selected, legacy);
        if legacy {
            return model;
        }
        let parsed_actual = model_id(actual).is_some();
        model.rechecked(ModelRepair::native(
            !parsed_actual && self.legacy_rerouted,
            !actual.is_null() && !parsed_actual,
        ))
    }
}

/// Read a fixed prefix. An incomplete trailing append is distinguishable from
/// a corrupt completed line, and neither is evidence that a request was absent.
pub fn native_snapshot(
    file: File,
    thread: &str,
    turn: &str,
    active: bool,
) -> Result<Vec<protocol::NativeTokenRecord>, protocol::TokenSourceGap> {
    snapshot(file, thread, Scope::Turn(turn), active)
}

/// Byte-compatible records for a pre-child Server's snapshot digest. This
/// is the old root-only parser policy, not the current accounting policy.
pub fn native_snapshot_legacy(
    file: File,
    thread: &str,
    turn: &str,
    active: bool,
) -> Result<Vec<protocol::NativeTokenRecord>, protocol::TokenSourceGap> {
    snapshot(file, thread, Scope::LegacyTurn(turn), active)
}

enum Scope<'a> {
    Turn(&'a str),
    LegacyTurn(&'a str),
    Child {
        session: &'a str,
        root_turn: &'a str,
    },
}

fn snapshot(
    file: File,
    thread: &str,
    scope: Scope<'_>,
    active: bool,
) -> Result<Vec<protocol::NativeTokenRecord>, TokenSourceGap> {
    use protocol::{NativeTokenRecord, NativeTurnKey, TokenSourceGap::*};

    let meta = file.metadata().map_err(|_| SourceUnreadable)?;
    if !meta.is_file() {
        return Err(SourceUnreadable);
    }
    let mut input = BufReader::new(file.take(meta.len()));
    let mut records = BTreeMap::<String, NativeTokenRecord>::new();
    let mut line = Vec::new();
    let mut model = TurnModel::default();
    let mut model_turn = match scope {
        Scope::Turn(turn) | Scope::LegacyTurn(turn) => turn.to_owned(),
        Scope::Child { .. } => String::new(),
    };
    loop {
        line.clear();
        let bytes = (&mut input)
            .take(MAX_LINE)
            .read_until(b'\n', &mut line)
            .map_err(|_| SourceUnreadable)?;
        if bytes == 0 {
            break;
        }
        if line.last() != Some(&b'\n') {
            if (bytes as u64) < MAX_LINE {
                if active {
                    break;
                }
                return Err(SourceCorrupt);
            }
            // Only deserialize the envelope tag. serde's IgnoredAny skips
            // large strings and nested values in unrelated payloads without
            // accumulating them in the spool or in a Value.
            let mut tail = LineTail {
                input: &mut input,
                ended: false,
            };
            let source = io::Cursor::new(&line).chain(&mut tail);
            let tag = serde_json::from_reader::<_, Tag>(source);
            if tag.as_ref().is_err_and(|err| err.is_io()) {
                return Err(SourceUnreadable);
            }
            if !tail.ended {
                if active && tag.as_ref().err().is_none_or(|err| err.is_eof()) {
                    break;
                }
                return Err(SourceCorrupt);
            }
            let tag = tag.map_err(|_| SourceCorrupt)?;
            if tag.kind == RECORD {
                return Err(OversizedNativeRecord);
            }
            if tag.kind == "turn_context" {
                model.current = false;
            }
            continue;
        }
        let value: Value = serde_json::from_slice(&line).map_err(|_| SourceCorrupt)?;
        let Some(kind) = value["type"].as_str() else {
            return Err(SourceCorrupt);
        };
        let payload = &value["payload"];
        if kind != RECORD {
            if matches!(scope, Scope::Child { .. }) && kind == "turn_context" {
                let next = payload["turn_id"].as_str().unwrap_or_default();
                if next != model_turn {
                    model = TurnModel::default();
                    model_turn = next.to_owned();
                }
            }
            model.observe(
                kind,
                payload,
                &model_turn,
                matches!(scope, Scope::LegacyTurn(_)),
            );
            continue;
        }
        let (Some(record_thread), Some(record_turn)) =
            (payload["thread_id"].as_str(), payload["turn_id"].as_str())
        else {
            return Err(SourceCorrupt);
        };
        // A fork can contain copied ancestor evidence. Only the exact
        // original thread and turn can contribute to this binding.
        let legacy = matches!(scope, Scope::LegacyTurn(_));
        if record_thread != thread {
            if legacy {
                return Err(SourceCorrupt);
            }
            continue;
        }
        let child = match scope {
            Scope::Turn(turn) | Scope::LegacyTurn(turn) if record_turn == turn => None,
            Scope::Child { session, root_turn }
                if payload["session_id"] == session
                    && payload["root_turn_id"] == root_turn
                    && valid_native_id(record_turn)
                    && record_turn != root_turn =>
            {
                Some(NativeTurnKey {
                    thread_id: thread.to_owned(),
                    turn_id: record_turn.to_owned(),
                })
            }
            _ => continue,
        };
        let response = payload["response_id"]
            .as_str()
            .filter(|id| {
                if legacy {
                    !id.is_empty() && id.len() <= 256
                } else {
                    valid_native_id(id)
                }
            })
            .ok_or(SourceCorrupt)?;
        let counts = serde_json::from_value::<TokenCounts>(payload["usage"].clone())
            .ok()
            .map(|counts| {
                if legacy {
                    counts
                } else {
                    counts.without_unproven_optional_zeros()
                }
            });
        let valid = serde_json::from_value::<NativeRecord>(payload.clone())
            .is_ok_and(|record| record.valid());
        let cursor = protocol::native_request_cursor(
            child
                .as_ref()
                .map(|key| (key.thread_id.as_str(), key.turn_id.as_str())),
            response,
        );
        let record = records.entry(cursor).or_insert_with(|| NativeTokenRecord {
            response_id: response.to_owned(),
            child,
            counts,
            invalid: false,
            variant: None,
            model: None,
        });
        record.invalid |= !valid;
        let merge_model = if legacy {
            TokenModel::merge_legacy
        } else {
            TokenModel::merge
        };
        record.model = merge_model(
            record.model.take(),
            Some(if model_turn == record_turn {
                model.for_history(
                    &payload["response_model"],
                    &payload["requested_model"],
                    legacy,
                )
            } else {
                TurnModel::default().for_history(
                    &payload["response_model"],
                    &payload["requested_model"],
                    legacy,
                )
            }),
        );
        if let Some(counts) = counts
            && record.counts != Some(counts)
        {
            if record.counts.is_some() {
                record.variant.get_or_insert(counts);
            } else {
                record.counts = Some(counts);
            }
        }
    }
    if records.is_empty() && matches!(scope, Scope::Turn(_) | Scope::LegacyTurn(_)) {
        return Err(NoNativeRecords);
    }
    // A file truncated after metadata has an unscanned tail even if its
    // replacement ends at a line boundary.
    if !active && input.get_ref().limit() != 0 {
        return Err(SourceChanged);
    }
    Ok(records.into_values().collect())
}

#[derive(Deserialize)]
struct Tag {
    #[serde(rename = "type")]
    kind: String,
}

struct LineTail<'a, B> {
    input: &'a mut B,
    ended: bool,
}

impl<B: BufRead> Read for LineTail<'_, B> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.ended || out.is_empty() {
            return Ok(0);
        }
        let bytes = self.input.fill_buf()?;
        let limit = bytes.len().min(out.len());
        let end = bytes[..limit].iter().position(|byte| *byte == b'\n');
        self.ended = end.is_some();
        let used = end.map_or(limit, |at| at + 1);
        out[..used].copy_from_slice(&bytes[..used]);
        self.input.consume(used);
        Ok(used)
    }
}

impl Collector {
    pub fn new(path: &Path, thread: &str, reused: bool) -> io::Result<Option<Self>> {
        if !path.is_absolute()
            || !path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.starts_with("rollout-") && name.ends_with(&format!("-{thread}.jsonl"))
                })
        {
            return Ok(None);
        }
        // File presence is optional before the first persisted work on a fresh
        // thread. On resume a missing baseline cannot prove a live append.
        let (file, offset) = match File::open(path) {
            Ok(file) => {
                let metadata = file.metadata()?;
                if !metadata.is_file() {
                    return Ok(None);
                }
                (Some(file), metadata.len())
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound && !reused => (None, 0),
            Err(e) => return Err(e),
        };
        Ok(Some(Self {
            path: path.to_owned(),
            file,
            offset,
            scanned_end: offset,
            notices: BTreeMap::new(),
            unmatched: 0,
            evidence: BTreeMap::new(),
            dirty: BTreeSet::new(),
            model: TurnModel::default(),
        }))
    }

    pub fn notice(&mut self, value: &Value) {
        if let Some((cursor, last)) = counts(value) {
            let versions = self.notices.entry(cursor).or_default();
            if !versions.iter().any(|notice| notice.last == last) {
                versions.push(Notice {
                    last,
                    matched: false,
                });
                self.unmatched += 1;
                self.dirty.insert(cursor);
            }
        }
    }

    pub fn has_unmatched(&self) -> bool {
        self.unmatched > 0
    }

    pub fn matched(&mut self, thread: &str, turn: &str) -> io::Result<Vec<(String, RequestMeter)>> {
        if self.notices.is_empty() {
            return Ok(Vec::new());
        }
        if self.file.is_none() {
            match File::open(&self.path) {
                Ok(file) if file.metadata()?.is_file() => self.file = Some(file),
                Ok(_) => return Ok(Vec::new()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
                Err(e) => return Err(e),
            }
        }
        let file = self.file.as_mut().unwrap();
        let end = file.metadata()?.len();
        if end < self.offset {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "active native rollout was truncated",
            ));
        }
        if end != self.scanned_end {
            file.seek(SeekFrom::Start(self.offset))?;
            // Revisit a partial JSONL line only after the file grows.
            let mut input = BufReader::new(file.take(end - self.offset));
            let mut line = Vec::new();
            while self.offset < end {
                line.clear();
                let bytes = (&mut input).take(MAX_LINE).read_until(b'\n', &mut line)? as u64;
                if bytes == 0 {
                    break;
                }
                if line.last() != Some(&b'\n') {
                    // Discard an oversized complete line without buffering it.
                    let rest = skip_line(&mut input)?;
                    if let Some(rest) = rest {
                        self.offset += bytes + rest;
                        if line[..line.len().min(256)]
                            .windows(12)
                            .any(|part| part == b"turn_context")
                        {
                            self.model.current = false;
                        }
                    } else {
                        break;
                    }
                    continue;
                }
                self.offset += bytes;
                let prefix = &line;
                if ![RECORD, "turn_context"]
                    .into_iter()
                    .any(|tag| prefix.windows(tag.len()).any(|part| part == tag.as_bytes()))
                {
                    continue;
                }
                let Ok(tag) = serde_json::from_slice::<Tag>(&line) else {
                    continue;
                };
                if tag.kind == RECORD {
                    if let Ok(mut payload) =
                        serde_json::from_slice::<Envelope>(&line).map(|e| e.payload)
                        && payload.thread_id == thread
                        && payload.turn_id == turn
                        && payload.valid()
                    {
                        payload.model = Some(self.model.for_response(
                            &payload.response_model,
                            &payload.requested_model,
                            false,
                        ));
                        let cursor = payload.thread_token_usage.total_tokens;
                        self.evidence.entry(cursor).or_default().push(payload);
                        self.dirty.insert(cursor);
                    }
                } else if tag.kind == "turn_context"
                    && let Ok(value) = serde_json::from_slice::<Value>(&line)
                {
                    self.model
                        .observe(&tag.kind, &value["payload"], turn, false);
                }
            }
            self.scanned_end = end;
        }
        let mut accepted = Vec::new();
        for cursor in std::mem::take(&mut self.dirty) {
            let (Some(records), Some(notices)) = (
                self.evidence.get_mut(&cursor),
                self.notices.get_mut(&cursor),
            ) else {
                continue;
            };
            records.retain(|record| {
                // A wire snapshot may omit optional fields, but every
                // reported value must agree with the native response.
                let mut matching = None;
                for notice in notices.iter_mut() {
                    let fields = notice.last.fields();
                    if fields
                        .into_iter()
                        .zip(record.usage.fields())
                        .all(|(wire, native)| wire.is_none() || wire == native)
                    {
                        if !notice.matched {
                            notice.matched = true;
                            self.unmatched -= 1;
                        }
                        let count = fields.into_iter().flatten().count();
                        if matching.is_none_or(|(previous, _)| count >= previous) {
                            matching = Some((count, notice.last));
                        }
                    }
                }
                if let Some((_, last)) = matching {
                    accepted.push((
                        record.response_id.clone(),
                        RequestMeter {
                            counts: last.without_unproven_optional_zeros(),
                            model: record.model.clone(),
                            provider_counts: false,
                        },
                    ));
                    false
                } else {
                    true
                }
            });
            if records.is_empty() {
                self.evidence.remove(&cursor);
            }
        }
        Ok(accepted)
    }
}

fn skip_line(input: &mut impl BufRead) -> io::Result<Option<u64>> {
    let mut total = 0;
    loop {
        let bytes = input.fill_buf()?;
        if bytes.is_empty() {
            return Ok(None);
        }
        let end = bytes.iter().position(|&byte| byte == b'\n');
        let used = end.map_or(bytes.len(), |i| i + 1);
        input.consume(used);
        total += used as u64;
        if end.is_some() {
            return Ok(Some(total));
        }
    }
}

fn counts(value: &Value) -> Option<(i64, TokenCounts)> {
    let cumulative = value["total"]["totalTokens"].as_i64().filter(|&n| n >= 0)?;
    let last = &value["last"];
    let fields = [
        "totalTokens",
        "inputTokens",
        "cachedInputTokens",
        "cacheWriteInputTokens",
        "outputTokens",
        "reasoningOutputTokens",
    ]
    .map(|key| match &last[key] {
        Value::Null => Some(None),
        value => value.as_i64().filter(|&n| n >= 0).map(Some),
    });
    let [a, b, c, d, e, f] = fields;
    let last = TokenCounts::from_fields([a?, b?, c?, d?, e?, f?]);
    (last.valid() && last.total_tokens.is_none_or(|last| last <= cumulative))
        .then_some((cumulative, last))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{
        fs::{self, OpenOptions},
        io::Write,
    };

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> io::Result<Self> {
            let dir = std::env::temp_dir().join(format!("tend-live-usage-{}", protocol::new_id()));
            fs::create_dir(&dir)?;
            Ok(Self(dir))
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn record(thread: &str, turn: &str, id: &str, cursor: i64, last: i64) -> Value {
        json!({"type":"token_usage_record", "payload": {
            "thread_id": thread, "turn_id": turn, "response_id": id,
            "usage": {"total_tokens":last, "input_tokens":last, "cached_input_tokens":0},
            "thread_token_usage":{"total_tokens":cursor}
        }})
    }

    fn notice(last: i64, cursor: i64) -> Value {
        json!({"total":{"totalTokens":cursor}, "last": {"totalTokens":last,"cachedInputTokens":0}})
    }

    #[test]
    fn old_root_snapshot_retains_optional_defaults_for_its_digest() -> anyhow::Result<()> {
        let fixture = Fixture::new()?;
        let path = fixture.0.join("legacy.jsonl");
        fs::write(
            &path,
            format!("{}\n", record("thread", "turn", "response", 10, 10)),
        )?;
        let legacy = native_snapshot_legacy(File::open(&path)?, "thread", "turn", false).unwrap();
        assert_eq!(legacy[0].counts.unwrap().cached_input_tokens, Some(0));
        let encoded = serde_json::to_value((&legacy, false))?;
        assert!(encoded[0][0].get("child").is_none());
        assert_eq!(encoded[0][0]["counts"]["cached_input_tokens"], 0);
        let updated = native_snapshot(File::open(&path)?, "thread", "turn", false).unwrap();
        assert_eq!(updated[0].counts.unwrap().cached_input_tokens, None);
        assert!(matches!(
            native_snapshot_legacy(File::open(&path)?, "other", "turn", false),
            Err(TokenSourceGap::SourceCorrupt)
        ));
        Ok(())
    }

    #[test]
    fn exact_app_server_selection_is_backward_readable() -> anyhow::Result<()> {
        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case", tag = "source", content = "id")]
        enum OldModel {
            TurnContext(String),
            Ambiguous,
            ResponseConflict,
        }

        let selected = Response::from_event(&json!({
            "responseId": "specific-response",
            "requestedModel": "selected",
            "usage": {"totalTokens": 100}
        }))
        .unwrap();
        let exact = selected.meter.model.unwrap();
        let wire = serde_json::to_value(&exact)?;
        assert_eq!(
            wire,
            json!({"source":"turn_context","id":"selected","request_level":true})
        );
        assert!(matches!(
            serde_json::from_value(wire.clone())?,
            OldModel::TurnContext(id) if id == "selected"
        ));
        assert_eq!(serde_json::from_value::<TokenModel>(wire)?, exact);
        let rechecked = exact.rechecked(ModelRepair::native(true, false));
        let read_old = |model| -> anyhow::Result<OldModel> {
            Ok(serde_json::from_str(&serde_json::to_string(&model)?)?)
        };
        assert!(matches!(read_old(rechecked)?, OldModel::TurnContext(id) if id == "selected"));
        assert!(matches!(
            read_old(TokenModel::TurnConflict)?,
            OldModel::Ambiguous
        ));
        assert!(matches!(
            read_old(TokenModel::ResponseConflict)?,
            OldModel::ResponseConflict
        ));
        let masked = TokenModel::merge(
            Some(TokenModel::LegacyResponseConflict),
            Some(TokenModel::Response("selected".into())),
        )
        .unwrap();
        assert!(matches!(read_old(masked)?, OldModel::ResponseConflict));
        Ok(())
    }

    #[test]
    fn models_use_only_prior_context_for_the_exact_turn_or_the_response() -> anyhow::Result<()> {
        let fixture = Fixture::new()?;
        let path = fixture.0.join("rollout-fixture-thread.jsonl");
        let mut live = Collector::new(&path, "thread", false)?.unwrap();
        let mut response = record("thread", "turn", "confirmed", 39, 8);
        response["payload"]["requested_model"] = json!("gpt-6.2-sol");
        response["payload"]["response_model"] = json!("openai/gpt-6.2-astra");
        let mut selected = record("thread", "turn", "selected", 49, 10);
        selected["payload"]["requested_model"] = json!("gpt-6.1-sol");
        selected["payload"]["response_model"] = json!("invalid model");
        let lines = [
            json!({"type":"turn_context","payload":{"turn_id":"old","model":"wrong"}}),
            record("thread", "turn", "early", 7, 7),
            json!({"type":"turn_context","payload":{"turn_id":"turn","model":"gpt-6.2-sol"}}),
            record("thread", "turn", "middle", 20, 13),
            json!({"type":"event_msg","payload":{"type":"model_reroute","from_model":"gpt-6.2-sol","to_model":"other"}}),
            record("thread", "turn", "late", 31, 11),
            response,
            selected,
        ];
        std::fs::write(
            &path,
            lines
                .iter()
                .map(|line| format!("{line}\n"))
                .collect::<String>(),
        )?;
        for (last, cursor) in [(7, 7), (13, 20), (11, 31), (8, 39), (10, 49)] {
            live.notice(&notice(last, cursor));
        }
        let live = live
            .matched("thread", "turn")?
            .into_iter()
            .map(|(id, meter)| (id, meter.model))
            .collect::<BTreeMap<_, _>>();
        let historical = native_snapshot(File::open(&path)?, "thread", "turn", false)
            .unwrap()
            .into_iter()
            .map(|row| (row.response_id, row.model))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            live,
            historical
                .iter()
                .map(|(id, model)| {
                    (
                        id.clone(),
                        model.as_ref().map(|model| model.basis().clone()),
                    )
                })
                .collect()
        );
        assert_eq!(live["early"], Some(TokenModel::Unresolved));
        assert_eq!(
            live["middle"],
            Some(TokenModel::TurnContext("gpt-6.2-sol".into()))
        );
        assert_eq!(live["late"], live["middle"]);
        assert_eq!(
            live["confirmed"],
            Some(TokenModel::Response("openai/gpt-6.2-astra".into()))
        );
        assert_eq!(
            live["selected"],
            Some(TokenModel::RequestSelection("gpt-6.1-sol".into()))
        );
        let legacy = native_snapshot_legacy(File::open(&path)?, "thread", "turn", false)
            .unwrap()
            .into_iter()
            .map(|row| (row.response_id, row.model))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(legacy["late"], Some(TokenModel::Ambiguous));
        assert_eq!(legacy["selected"], Some(TokenModel::LegacyResponseConflict));
        for id in ["late", "selected"] {
            let repaired = TokenModel::merge(legacy[id].clone(), historical[id].clone());
            assert_eq!(repaired, historical[id]);
            assert_eq!(
                TokenModel::merge(repaired.clone(), legacy[id].clone()),
                repaired
            );
        }
        Ok(())
    }

    #[test]
    fn consumes_only_completed_appends_and_requires_both_sources_and_native_identity()
    -> anyhow::Result<()> {
        let dir = Fixture::new()?;
        let path = dir.0.join("rollout-benchmark-thread.jsonl");
        let mut file = File::create(&path)?;
        writeln!(file, "{}", record("thread", "turn", "preexisting", 6, 3))?;
        let mut live = Collector::new(&path, "thread", true)?.unwrap();
        live.notice(&notice(3, 6));
        assert!(live.has_unmatched());
        assert!(live.matched("thread", "turn")?.is_empty());
        writeln!(
            file,
            "{}",
            record("other", "turn", "different-thread", 6, 3)
        )?;
        writeln!(
            file,
            "{}",
            record("thread", "old-turn", "previous-turn", 6, 3)
        )?;
        write!(file, "{}", record("thread", "turn", "live", 6, 3))?;
        assert!(live.matched("thread", "turn")?.is_empty());
        writeln!(file)?;
        let accepted = live.matched("thread", "turn")?;
        assert_eq!(accepted.len(), 1);
        assert_eq!(accepted[0].0, "live");
        assert!(!live.has_unmatched());
        assert_eq!(
            accepted[0].1.counts.fields(),
            [Some(3), None, None, None, None, None]
        );
        assert!(live.matched("thread", "turn")?.is_empty());

        // An unmatched context estimate does not retract the first event. A
        // genuine native variant for the same response does reach the ledger.
        live.notice(&notice(7, 16));
        assert!(live.has_unmatched());
        assert!(live.matched("thread", "turn")?.is_empty());
        writeln!(file, "{}", record("thread", "turn", "live", 16, 7))?;
        assert_eq!(live.matched("thread", "turn")?[0].0, "live");
        assert!(!live.has_unmatched());
        Ok(())
    }

    #[test]
    fn historical_snapshot_fences_turn_and_isolates_invalid_or_conflicting_keys()
    -> anyhow::Result<()> {
        use protocol::TokenSourceGap::*;
        let dir = Fixture::new()?;
        let path = dir.0.join("native.jsonl");
        let mut file = File::create(&path)?;
        writeln!(file, "{}", record("thread", "other-turn", "old", 8, 2))?;
        for (id, cursor, last) in [
            ("a", 1, 1),
            ("a", 1, 1),
            ("b", 3, 2),
            ("b", 4, 3),
            ("c", 4, 9),
        ] {
            writeln!(file, "{}", record("thread", "turn", id, cursor, last))?;
        }
        write!(file, "{{partial")?;
        let records = native_snapshot(File::open(&path)?, "thread", "turn", true).unwrap();
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].response_id, "a");
        assert!(records[0].variant.is_none());
        assert_eq!(records[1].variant.unwrap().total_tokens, Some(3));
        assert!(records[2].invalid);
        assert_eq!(
            native_snapshot(File::open(&path)?, "thread", "turn", false),
            Err(SourceCorrupt)
        );
        writeln!(file)?;
        assert_eq!(
            native_snapshot(File::open(&path)?, "thread", "turn", true),
            Err(SourceCorrupt)
        );
        Ok(())
    }

    #[test]
    fn oversized_unrelated_payload_is_skipped_but_oversized_meter_has_explicit_gap()
    -> anyhow::Result<()> {
        use protocol::TokenSourceGap::OversizedNativeRecord;
        let dir = Fixture::new()?;
        let path = dir.0.join("long-lines.jsonl");
        let mut file = File::create(&path)?;
        let huge = "X".repeat(MAX_LINE as usize + 19);
        // Place the type after the huge payload, so no prefix heuristic is enough.
        writeln!(
            file,
            r#"{{"payload":{{"type":"token_usage_record","data":"{huge}"}},"type":"response_item"}}"#
        )?;
        writeln!(file, "{}", record("thread", "turn", "present", 6, 3))?;
        let records = native_snapshot(File::open(&path)?, "thread", "turn", false).unwrap();
        assert_eq!(records[0].response_id, "present");
        writeln!(
            file,
            r#"{{"payload":"{huge}","type":"token_usage_record"}}"#
        )?;
        assert_eq!(
            native_snapshot(File::open(&path)?, "thread", "turn", false),
            Err(OversizedNativeRecord)
        );
        Ok(())
    }

    #[test]
    fn native_zero_is_proven_even_when_the_cursor_does_not_change() -> anyhow::Result<()> {
        let dir = Fixture::new()?;
        let path = dir.0.join("rollout-benchmark-thread.jsonl");
        assert!(Collector::new(&path, "thread", true).is_err());
        let mut live = Collector::new(&path, "thread", false)?.unwrap();
        live.notice(&notice(0, 0));
        assert!(live.matched("thread", "turn")?.is_empty());
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(file, "{}", record("thread", "turn", "zero-one", 0, 0))?;
        writeln!(file, "{}", record("thread", "turn", "zero-two", 0, 0))?;
        let accepted = live.matched("thread", "turn")?;
        assert_eq!(accepted.len(), 2);
        assert!(
            accepted
                .iter()
                .all(|(_, usage)| usage.counts.total_tokens == Some(0))
        );
        Ok(())
    }
}
