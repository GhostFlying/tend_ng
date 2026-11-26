//! Live request provenance for an already identity-fenced App Server stream.
//!
//! TokenCount also reports estimates, limit markers and restored context. Pair
//! the wire values with exact per-response evidence from this turn's append
//! segment. Never substitute counts from the file for missing wire fields.
use protocol::TokenCounts;
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

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
}

struct Notice {
    last: TokenCounts,
    matched: bool,
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "type")]
    kind: String,
    payload: NativeRecord,
}

#[derive(Deserialize)]
struct NativeRecord {
    thread_id: String,
    turn_id: String,
    response_id: String,
    usage: TokenCounts,
    thread_token_usage: Cursor,
}

#[derive(Deserialize)]
struct Cursor {
    total_tokens: i64,
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

    pub fn matched(&mut self, thread: &str, turn: &str) -> io::Result<Vec<(String, TokenCounts)>> {
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
                    } else {
                        break;
                    }
                    continue;
                }
                self.offset += bytes;
                if !line
                    .windows(RECORD.len())
                    .any(|part| part == RECORD.as_bytes())
                {
                    continue;
                }
                if let Ok(Envelope { kind, payload }) = serde_json::from_slice::<Envelope>(&line)
                    && kind == RECORD
                    && payload.thread_id == thread
                    && payload.turn_id == turn
                    && !payload.response_id.is_empty()
                    && payload.response_id.len() <= 256
                    && payload.thread_token_usage.total_tokens >= 0
                    && payload.usage.valid()
                {
                    let cursor = payload.thread_token_usage.total_tokens;
                    self.evidence.entry(cursor).or_default().push(payload);
                    self.dirty.insert(cursor);
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
                    accepted.push((record.response_id.clone(), last));
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
            accepted[0].1.fields(),
            [Some(3), None, Some(0), None, None, None]
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
                .all(|(_, usage)| usage.total_tokens == Some(0))
        );
        Ok(())
    }
}
