use serde::{Deserialize, Serialize};

pub const DEFAULT_LISTEN: &str = "127.0.0.1:8832";
pub const TOKEN_ENV: &str = "TEND_TOKEN";

#[derive(Debug, Deserialize, Serialize)]
pub struct SendMessage {
    pub operation_id: String,
    pub text: String,
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

pub fn messages_path(chat_id: &str) -> String {
    format!("/api/v1/chats/{chat_id}/messages")
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
