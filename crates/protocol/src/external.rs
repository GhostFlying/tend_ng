use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalCondition {
    pub url: String,
    pub event: ExternalKind,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalKind {
    Pull,
    Ci,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalFact {
    pub url: String,
    pub event: ExternalKind,
    pub state: String,
    pub head_sha: Option<String>,
    pub check: Option<String>,
    pub run_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodebaseCommentSource {
    ThreadComment,
    ReviewNote,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodebaseReviewComment {
    pub id: String,
    pub source: CodebaseCommentSource,
    pub repository: String,
    pub number: i64,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub created_at: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub positions: Vec<CodebaseCommentPosition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<CodebaseCommentDiff>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outdating_diff: Option<CodebaseCommentDiff>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodebaseCommentPosition {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub side: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_line: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_column: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_line: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_column: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodebaseCommentDiff {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_patch: Option<String>,
}

impl ExternalCondition {
    pub fn normalize(&mut self) -> bool {
        let Some(pull) = crate::GitHubPull::parse(&self.url) else {
            return false;
        };
        self.url = pull.url();
        if let Some(sha) = &mut self.head_sha {
            if !matches!(sha.len(), 40 | 64) || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return false;
            }
            sha.make_ascii_lowercase();
        }
        match self.event {
            ExternalKind::Pull => {
                matches!(self.state.as_str(), "open" | "merged" | "closed")
                    && self.check.is_none()
                    && self.run_id.is_none()
            }
            ExternalKind::Ci => {
                matches!(
                    self.state.as_str(),
                    "pending" | "success" | "failure" | "cancelled" | "skipped" | "terminal"
                ) && self
                    .check
                    .as_ref()
                    .is_none_or(|check| !check.trim().is_empty())
                    && self
                        .run_id
                        .as_ref()
                        .is_none_or(|id| self.check.is_some() && !id.trim().is_empty())
            }
        }
    }

    pub fn matches(&self, fact: &ExternalFact) -> bool {
        self.url == fact.url
            && self.event == fact.event
            && self
                .head_sha
                .as_ref()
                .is_none_or(|sha| fact.head_sha.as_ref() == Some(sha))
            && self.check == fact.check
            && self
                .run_id
                .as_ref()
                .is_none_or(|id| fact.run_id.as_ref() == Some(id))
            && (self.state == fact.state
                || self.state == "terminal"
                    && matches!(
                        fact.state.as_str(),
                        "success" | "failure" | "cancelled" | "skipped"
                    ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatchMode {
    Optional,
    Required,
    Wait,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RequireExternal {
    pub operation_id: String,
    pub condition: ExternalCondition,
}
