use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AttentionRequest {
    pub question: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub context: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_at: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttentionState {
    Pending,
    AwaitingUser,
    Answered,
    Delivered,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AttentionStatus {
    pub id: String,
    pub state: AttentionState,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Attention {
    #[serde(flatten)]
    pub status: AttentionStatus,
    pub issue_id: String,
    pub task_id: String,
    pub source_message_id: String,
    pub question_message_id: Option<String>,
    pub user_message_id: Option<String>,
    pub answer: Option<String>,
    pub request: AttentionRequest,
}

pub fn attentions_path(chat_id: &str) -> String {
    format!("/api/v1/chats/{chat_id}/attentions")
}
