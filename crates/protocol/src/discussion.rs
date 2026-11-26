use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const DISCUSSIONS_PATH: &str = "/api/v1/discussions";
pub const DISCUSSION_PROFILE: &str = "tend-discussion";

pub fn discussion_workspace_path(issue: &str, task: &str, assignment: &str) -> String {
    format!("/api/v1/inspect/discussion-workspaces/{issue}/{task}/{assignment}")
}

pub fn discussion_workspace_ids(path: &str) -> Option<(&str, &str, &str)> {
    let (issue, suffix) = path
        .strip_prefix("/api/v1/inspect/discussion-workspaces/")?
        .split_once('/')?;
    let (task, assignment) = suffix.split_once('/')?;
    [issue, task, assignment]
        .into_iter()
        .all(crate::valid_id)
        .then_some((issue, task, assignment))
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DiscussionWorkspace {
    pub assignment_id: String,
    pub task_id: String,
    pub issue_id: String,
    pub runner_id: String,
    pub host_id: String,
    pub mounts: Vec<DiscussionMount>,
    pub unavailable: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DiscussionMount {
    pub repo_id: String,
    pub stamp: crate::WorkspaceStamp,
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DiscussionOrigin {
    Chat { chat_id: String, message_id: String },
    Issue { issue_id: String },
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiscussionSource {
    Repo {
        repo_id: String,
    },
    Issue {
        issue_id: String,
    },
    Pull {
        url: String,
    },
    Worktree {
        issue_id: String,
        task_id: String,
        repo_id: String,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateDiscussion {
    pub operation_id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub origin: Option<DiscussionOrigin>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateDiscussion {
    pub title: Option<String>,
    pub archived: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DiscussionReference {
    pub id: String,
    #[serde(flatten)]
    pub source: DiscussionSource,
    pub label: String,
    pub href: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct DiscussionDraft {
    pub goal: String,
    pub decisions: String,
    pub constraints: String,
    pub authorization_end: String,
}

#[derive(Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct DiscussionSend {
    pub operation_id: String,
    pub draft_id: String,
    #[serde(flatten)]
    pub draft: DiscussionDraft,
    #[serde(default)]
    pub reference_ids: Vec<String>,
    #[serde(default)]
    pub target_issue_id: Option<String>,
}
