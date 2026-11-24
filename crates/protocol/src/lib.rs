use serde::{Deserialize, Serialize};

mod runner;
pub use runner::*;
mod attention;
pub use attention::*;
mod issue_chat;
pub use issue_chat::*;

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
    pub artifacts: Vec<Artifact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<AttentionStatus>,
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
}

#[derive(Debug, Deserialize, Serialize)]
pub struct IssueDispatch {
    pub issue_id: String,
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

pub fn artifact_upload_path(runner: &str, id: &str) -> String {
    format!("/api/v1/runners/{runner}/artifacts/{id}")
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
