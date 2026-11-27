use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TokenCounts {
    pub total_tokens: Option<i64>,
    pub input_tokens: Option<i64>,
    pub cached_input_tokens: Option<i64>,
    pub cache_write_input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub reasoning_output_tokens: Option<i64>,
}

impl TokenCounts {
    pub fn fields(self) -> [Option<i64>; 6] {
        [
            self.total_tokens,
            self.input_tokens,
            self.cached_input_tokens,
            self.cache_write_input_tokens,
            self.output_tokens,
            self.reasoning_output_tokens,
        ]
    }

    pub fn from_fields(fields: [Option<i64>; 6]) -> Self {
        let [
            total_tokens,
            input_tokens,
            cached_input_tokens,
            cache_write_input_tokens,
            output_tokens,
            reasoning_output_tokens,
        ] = fields;
        Self {
            total_tokens,
            input_tokens,
            cached_input_tokens,
            cache_write_input_tokens,
            output_tokens,
            reasoning_output_tokens,
        }
    }

    pub fn merge_known(self, other: Self) -> Option<Self> {
        let mut fields = self.fields();
        for (left, right) in fields.iter_mut().zip(other.fields()) {
            if left.is_some() && right.is_some() && *left != right {
                return None;
            }
            *left = left.or(right);
        }
        Some(Self::from_fields(fields))
    }

    pub fn valid(self) -> bool {
        let fields = self.fields();
        fields.iter().any(Option::is_some) && fields.into_iter().flatten().all(|n| n >= 0)
    }
}

/// Model evidence from the native rollout, not from the assigned Agent.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case", tag = "source", content = "id")]
pub enum TokenModel {
    Response(String),
    TurnContext(String),
    /// A normalized request was checked without finding model evidence.
    /// Absent (None) means a legacy writer did not perform this check.
    Unresolved,
    Ambiguous,
    ResponseConflict,
}

impl TokenModel {
    pub fn valid_id(id: &str) -> bool {
        !id.is_empty()
            && id.len() <= 128
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/' | b':'))
    }

    pub fn valid(&self) -> bool {
        match self {
            Self::Response(id) | Self::TurnContext(id) => Self::valid_id(id),
            Self::Unresolved | Self::Ambiguous | Self::ResponseConflict => true,
        }
    }

    /// No evidence cannot revoke existing evidence. A contradiction at the
    /// same evidence level must not silently choose a new model.
    pub fn merge(old: Option<Self>, new: Option<Self>) -> Option<Self> {
        match (old, new) {
            (None, m) | (m, None) => m,
            (Some(Self::Unresolved), m) | (m, Some(Self::Unresolved)) => m,
            (Some(a), Some(b)) if a == b => Some(a),
            (Some(Self::ResponseConflict), _)
            | (_, Some(Self::ResponseConflict))
            | (Some(Self::Response(_)), Some(Self::Response(_))) => Some(Self::ResponseConflict),
            (Some(m @ Self::Response(_)), _) | (_, Some(m @ Self::Response(_))) => Some(m),
            _ => Some(Self::Ambiguous),
        }
    }
}

/// Request evidence stored alongside the original flat counts JSON. This
/// reads legacy records and preserves the count payload on older peers.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RequestMeter {
    #[serde(flatten)]
    pub counts: TokenCounts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<TokenModel>,
}

/// A native request observation with a corroborated upstream response ID.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RunTokenUsage {
    pub event_id: String,
    pub assignment_id: String,
    pub task_id: String,
    pub run_id: String,
    pub thread_id: String,
    pub turn_id: String,
    pub response_id: String,
    pub last: TokenCounts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<TokenModel>,
    pub observed_at: i64,
}

impl RunTokenUsage {
    pub fn valid(&self) -> bool {
        [
            &self.event_id,
            &self.assignment_id,
            &self.task_id,
            &self.run_id,
        ]
        .into_iter()
        .all(|id| crate::valid_id(id))
            && [&self.thread_id, &self.turn_id, &self.response_id]
                .into_iter()
                .all(|id| !id.is_empty() && id.len() <= 256)
            && self.last.valid()
            && self.model.as_ref().is_none_or(TokenModel::valid)
            && self.observed_at > 0
    }
}

pub const TOKEN_OVERVIEW_PATH: &str = "/api/v1/token-usage";
pub const TOKEN_BACKFILL_PATH: &str = "/api/v1/token-usage/backfills";
pub const TOKEN_REBUILD_PATH: &str = "/api/v1/token-usage/rebuild";

#[derive(Debug, Eq, PartialEq)]
pub struct TokenOverviewCursor {
    pub source_seq: i64,
    pub issue_id: String,
}

impl TokenOverviewCursor {
    pub fn encode(&self) -> String {
        format!("v1.{}.{}", self.source_seq, self.issue_id)
    }

    pub fn decode(cursor: &str) -> Option<Self> {
        let (seq, issue_id) = cursor.strip_prefix("v1.")?.split_once('.')?;
        let source_seq = seq.parse::<i64>().ok()?;
        (seq == source_seq.to_string() && crate::valid_id(issue_id)).then(|| Self {
            source_seq,
            issue_id: issue_id.to_owned(),
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum TokenUsageScope {
    All,
    Run(String),
    Task(String),
    Issue(String),
    Runner(String),
    Host(String),
}

impl TokenUsageScope {
    pub fn valid(&self) -> bool {
        match self {
            Self::All => true,
            Self::Run(id) | Self::Task(id) | Self::Runner(id) | Self::Host(id) => {
                crate::valid_id(id)
            }
            Self::Issue(id) => crate::valid_id(id) || crate::parse_issue_number(id).is_some(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenConflictPolicy {
    #[default]
    Quarantine,
    PreferNative,
    PreferServer,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StartTokenBackfill {
    pub job_id: String,
    pub scope: TokenUsageScope,
    #[serde(default)]
    pub policy: TokenConflictPolicy,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TokenBackfillWork {
    pub item: i64,
    pub job_id: String,
    pub runner_id: String,
    pub run_id: String,
    pub thread_id: String,
    pub turn_id: String,
    pub offset: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenSourceGap {
    BindingMissing,
    SourceMissing,
    SourceAmbiguous,
    SourceUnreadable,
    SourceCorrupt,
    OversizedNativeRecord,
    SourceChanged,
    NoNativeRecords,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NativeTokenRecord {
    pub response_id: String,
    pub counts: Option<TokenCounts>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<TokenModel>,
    pub invalid: bool,
    /// A second distinct variant proves that the native key is ambiguous.
    pub variant: Option<TokenCounts>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TokenBackfillPage {
    pub item: i64,
    pub offset: i64,
    #[serde(flatten)]
    pub content: TokenBackfillContent,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum TokenBackfillContent {
    Gap {
        reason: TokenSourceGap,
    },
    Records {
        snapshot: String,
        records: Vec<NativeTokenRecord>,
        done: bool,
        prefix: bool,
    },
}

/// Opt-in on Runner-wide sync only. Presence advertises support even with no page.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct TokenBackfillSync {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<TokenBackfillPage>,
}

impl TokenBackfillPage {
    pub fn valid(&self) -> bool {
        if self.item <= 0 || self.offset < 0 {
            return false;
        }
        match &self.content {
            TokenBackfillContent::Gap { .. } => true,
            TokenBackfillContent::Records {
                snapshot,
                records,
                done,
                ..
            } => {
                snapshot.len() == 64
                    && snapshot.bytes().all(|byte| byte.is_ascii_hexdigit())
                    && !records.is_empty()
                    && records.len() <= crate::RUNNER_SYNC_BATCH
                    && (*done || records.len() == crate::RUNNER_SYNC_BATCH)
                    && records.iter().all(|record| {
                        !record.response_id.is_empty()
                            && record.response_id.len() <= 256
                            && record.model.as_ref().is_none_or(TokenModel::valid)
                    })
                    && records
                        .windows(2)
                        .all(|rows| rows[0].response_id < rows[1].response_id)
            }
        }
    }
}
