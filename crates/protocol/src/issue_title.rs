use serde::{Deserialize, Serialize};

/// Bytes after trimming, before JSON escaping. A full page of these titles
/// leaves room in the Runner envelope even when every byte needs escaping.
pub const MAX_ISSUE_TITLE_BYTES: usize = 1024;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RenameIssue {
    pub operation_id: String,
    pub title: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IssueTitleActor {
    User,
    Owner { agent_id: String },
}

#[derive(Debug, Deserialize, Serialize)]
pub struct IssueTitleChange {
    pub operation_id: String,
    pub issue_id: String,
    pub old_title: String,
    pub new_title: String,
    pub actor: IssueTitleActor,
    pub changed_at: i64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct IssueTitleHistory {
    pub changes: Vec<IssueTitleChange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_before: Option<i64>,
}

pub fn normalize_issue_title(title: &str) -> Option<&str> {
    let title = title.trim();
    (!title.is_empty()
        && title.len() <= MAX_ISSUE_TITLE_BYTES
        && !title.chars().any(char::is_control))
    .then_some(title)
}

pub fn issue_title_path(id: &str) -> String {
    format!("/api/v1/issues/{id}/title")
}

pub fn issue_title_history_path(id: &str) -> String {
    format!("/api/v1/issues/{id}/title-history")
}
