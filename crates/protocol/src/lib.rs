use serde::{Deserialize, Serialize};

mod runner;
pub use runner::*;
mod token_usage;
pub use token_usage::*;
mod discussion;
pub use discussion::*;
mod attention;
pub use attention::*;
mod control;
mod external;
mod handoff;
mod issue_chat;
mod notification;
mod pull;
mod reclaim;
mod report;
mod review;
pub use control::*;
pub use external::*;
pub use handoff::*;
pub use issue_chat::*;
pub use notification::*;
pub use pull::*;
pub use reclaim::*;
pub use report::*;
pub use review::*;

pub const DEFAULT_LISTEN: &str = "127.0.0.1:8832";
pub const TOKEN_ENV: &str = "TEND_TOKEN";

#[derive(Debug, Deserialize, Serialize)]
pub struct SendMessage {
    pub operation_id: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Receipt {
    pub message_id: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Author {
    User,
    Router,
    Discussion,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RouterState {
    Queued,
    AwaitingTurn,
    Completed,
    Failed,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Message {
    pub id: String,
    pub author: Author,
    pub text: String,
    pub created_at: i64,
    pub reply_to: Option<String>,
    pub router_state: Option<RouterState>,
    pub router_error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub issue_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub issue_numbers: Vec<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<Artifact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<AttentionStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discussion_id: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct MessagePage {
    pub messages: Vec<Message>,
    pub next_before: Option<i64>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ApiError {
    pub error: String,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DispatchIssue {
    pub title: String,
    pub objective: String,
    #[serde(default)]
    pub repo_ids: Vec<String>,
    #[serde(default)]
    pub required_capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub base_refs: std::collections::BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentPin>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct IssueDispatch {
    pub issue_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_number: Option<i64>,
    pub task_id: String,
    pub operation_id: String,
    pub source_message_id: String,
    pub chat_id: String,
    pub category: String,
    pub status: String,
    #[serde(default = "open_issue")]
    pub issue_state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_issue_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<Placement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<RunObservation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<Artifact>,
    #[serde(flatten)]
    pub work: DispatchIssue,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RecordedIssue {
    pub issue_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_number: Option<i64>,
    pub operation_id: String,
    pub source_message_id: String,
    pub chat_id: String,
    pub issue_state: String,
    #[serde(flatten)]
    pub work: DispatchIssue,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum IssueDetail {
    Dispatched(Box<IssueDispatch>),
    Recorded(RecordedIssue),
}

impl IssueDetail {
    pub fn chat_id(&self) -> &str {
        match self {
            Self::Dispatched(issue) => &issue.chat_id,
            Self::Recorded(issue) => &issue.chat_id,
        }
    }
}

fn open_issue() -> String {
    "open".to_owned()
}

pub fn messages_path(chat_id: &str) -> String {
    format!("/api/v1/chats/{chat_id}/messages")
}

pub fn issue_path(issue_id: &str) -> String {
    format!("/api/v1/issues/{issue_id}")
}

pub fn artifact_path(id: &str) -> String {
    format!("/api/v1/artifacts/{id}")
}

pub const ARTIFACT_BYTES_HEADER: &str = "X-Tend-Artifact-Bytes";

pub fn artifact_upload_path(runner: &str, id: &str) -> String {
    format!("/api/v1/runners/{runner}/artifacts/{id}")
}

pub fn parse_issue_number(reference: &str) -> Option<i64> {
    let digits = reference.strip_prefix('#').unwrap_or(reference);
    if digits.is_empty() || digits.starts_with('0') || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::parse_issue_number;

    #[test]
    fn parses_canonical_positive_issue_numbers() {
        for reference in ["1", "#1"] {
            assert_eq!(parse_issue_number(reference), Some(1));
        }
        assert_eq!(parse_issue_number("#9223372036854775807"), Some(i64::MAX));
        for reference in [
            "",
            "#",
            "0",
            "#0",
            "01",
            "#01",
            "-1",
            "#-1",
            "+1",
            " 1",
            "1 ",
            "##1",
            "#1a",
            "１",
            "9223372036854775808",
        ] {
            assert_eq!(parse_issue_number(reference), None, "{reference}");
        }
    }
}
