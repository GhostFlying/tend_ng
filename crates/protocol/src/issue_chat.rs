use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IssueMessageKind {
    User,
    Agent,
    Dispatch,
    Relay,
    Run,
    Takeover,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "origin", rename_all = "snake_case")]
pub enum DispatchProvenance {
    MainChat {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_name: Option<String>,
    },
    OtherChat,
    IssueDelegation {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_name: Option<String>,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct IssueMessage {
    pub id: String,
    pub issue_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_number: Option<i64>,
    pub issue_title: String,
    pub kind: IssueMessageKind,
    pub author: String,
    pub text: String,
    pub reply_to: Option<String>,
    pub run_id: Option<String>,
    pub source_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forwarded_from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch_provenance: Option<DispatchProvenance>,
    pub created_at: Option<i64>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct IssueMessagePage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_number: Option<i64>,
    pub title: String,
    #[serde(default)]
    pub chat_id: String,
    #[serde(default)]
    pub awaiting_start: bool,
    pub messages: Vec<IssueMessage>,
    pub next_before: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approvals: Vec<IssueApprovalEvent>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct IssueApprovalEvent {
    pub decision: crate::ApprovalDecision,
    pub report_title: String,
    pub issue_id: String,
    pub before_message_id: Option<String>,
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

#[derive(Debug, Serialize, Deserialize)]
pub struct NativeTurnRef {
    pub thread_id: String,
    pub turn_id: String,
}

pub fn retired_rollout_path(origin: &str, run: &str) -> String {
    format!("/api/v1/history/{origin}/runs/{run}/rollout")
}

pub fn retired_rollout_ids(path: &str) -> Option<(&str, &str)> {
    let (origin, run) = path
        .strip_prefix("/api/v1/history/")?
        .strip_suffix("/rollout")?
        .split_once("/runs/")?;
    (crate::valid_id(origin) && crate::valid_id(run)).then_some((origin, run))
}
