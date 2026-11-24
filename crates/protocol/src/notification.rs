use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
pub struct IssueActivity {
    pub seq: i64,
    pub issue_id: String,
    pub issue_title: String,
    pub task_id: Option<String>,
    pub previous: String,
    pub current: String,
    pub changed_at: i64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct NotificationPage {
    pub activities: Vec<IssueActivity>,
    pub next_before: Option<i64>,
}

pub fn notifications_path(chat: &str) -> String {
    format!("/api/v1/chats/{chat}/notifications")
}
