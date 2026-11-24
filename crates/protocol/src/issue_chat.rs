use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IssueMessageKind {
    User,
    Agent,
    Dispatch,
    Relay,
    Run,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct IssueMessage {
    pub id: String,
    pub issue_id: String,
    pub issue_title: String,
    pub kind: IssueMessageKind,
    pub author: String,
    pub text: String,
    pub reply_to: Option<String>,
    pub run_id: Option<String>,
    pub source_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forwarded_from: Option<String>,
    pub created_at: Option<i64>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct IssueMessagePage {
    pub title: String,
    #[serde(default)]
    pub chat_id: String,
    #[serde(default)]
    pub awaiting_start: bool,
    pub messages: Vec<IssueMessage>,
    pub next_before: Option<i64>,
}

pub fn issue_messages_path(id: &str) -> String {
    format!("/api/v1/issues/{id}/messages")
}

pub fn rollout_path(id: &str) -> String {
    format!("/api/v1/runs/{id}/rollout")
}

pub fn rollout_id(path: &str) -> Option<&str> {
    path.strip_prefix("/api/v1/runs/")?
        .strip_suffix("/rollout")
        .filter(|id| crate::valid_id(id))
}
