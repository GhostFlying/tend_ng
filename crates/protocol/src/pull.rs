use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub struct GitHubPull {
    pub repository: String,
    pub number: i64,
}

impl GitHubPull {
    pub fn parse(url: &str) -> Option<Self> {
        let mut parts = url
            .trim()
            .strip_prefix("https://github.com/")?
            .split(['?', '#'])
            .next()?
            .trim_end_matches('/')
            .split('/');
        let (owner, repo) = (parts.next()?, parts.next()?);
        if parts.next()? != "pull" || !repository_part(owner) || !repository_part(repo) {
            return None;
        }
        let number = parts.next()?.parse().ok().filter(|number| *number > 0)?;
        if parts.next().is_some() {
            return None;
        }
        Some(Self {
            repository: format!("{owner}/{repo}").to_ascii_lowercase(),
            number,
        })
    }

    pub fn url(&self) -> String {
        format!(
            "https://github.com/{}/pull/{}",
            self.repository, self.number
        )
    }
}

fn repository_part(value: &str) -> bool {
    !matches!(value, "" | "." | "..")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LinkPull {
    pub operation_id: String,
    pub url: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PullLinkReceipt {
    pub issue_id: String,
    pub url: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PullFacts {
    pub title: String,
    pub state: String,
    pub draft: bool,
    pub head_sha: String,
    pub ci: Option<String>,
    #[serde(default)]
    pub head_branch: Option<String>,
    #[serde(default)]
    pub head_repository: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PullLink {
    pub url: String,
    pub number: i64,
    pub facts: Option<PullFacts>,
    pub observed_at: Option<i64>,
    pub observation_error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum BoardStage {
    Backlog,
    #[default]
    Todo,
    InProgress,
    Waiting,
    Done,
    Cancelled,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoardWait {
    External,
    Reply,
    Children,
    Continuation,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BoardAttention {
    pub chat_id: String,
    pub question_message_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IssueCard {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_number: Option<i64>,
    pub parent_issue_id: Option<String>,
    pub title: String,
    pub issue_state: String,
    pub task_state: String,
    #[serde(default)]
    pub stage: BoardStage,
    #[serde(default)]
    pub running: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repo_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_for: Option<BoardWait>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<BoardAttention>,
    pub pulls: Vec<PullLink>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IssueBoard {
    pub issues: Vec<IssueCard>,
    pub next_before: Option<String>,
}

/// The versioned board keyset cursor. Zero represents an unknown entry time.
#[derive(Debug, PartialEq, Eq)]
pub struct IssueBoardCursor {
    pub entry_ms: i64,
    pub issue_id: String,
}

impl IssueBoardCursor {
    pub fn encode(&self) -> String {
        format!("v1.{}.{}", self.entry_ms, self.issue_id)
    }

    pub fn decode(cursor: &str) -> Option<Self> {
        let (time, issue_id) = cursor.strip_prefix("v1.")?.split_once('.')?;
        let entry_ms = time.parse::<i64>().ok()?;
        (entry_ms >= 0 && time == entry_ms.to_string() && crate::valid_id(issue_id)).then(|| Self {
            entry_ms,
            issue_id: issue_id.to_owned(),
        })
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IssueOverview {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_number: Option<i64>,
    pub title: String,
    pub stage: BoardStage,
    pub objective: String,
    pub repo_ids: Vec<String>,
    pub chat_id: String,
    pub source_message_id: String,
    pub children: Vec<IssueOverviewChild>,
    pub pulls: Vec<PullLink>,
    pub artifacts: Vec<IssueOverviewArtifact>,
    #[serde(default)]
    pub reports: Vec<IssueDeliveryReport>,
    #[serde(default)]
    pub decisions: Vec<IssueDecision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_history_before: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub proposals: Vec<crate::IssueProposal>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub proposal_requires_chat: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IssueOverviewChild {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_number: Option<i64>,
    pub title: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IssueOverviewArtifact {
    #[serde(flatten)]
    pub artifact: crate::Artifact,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<DeliverySource>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DeliverySource {
    pub issue: IssueOverviewChild,
    pub task_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IssueDeliveryReport {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_id: Option<String>,
    pub message_id: String,
    pub text: String,
    pub artifacts: Vec<IssueOverviewArtifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<IssueDeliveryReportVersion>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IssueDeliveryReportVersion {
    pub report_id: String,
    pub message_id: String,
    pub text: String,
    pub artifacts: Vec<IssueOverviewArtifact>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IssueDecision {
    pub message_id: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply: Option<crate::IssueAttentionReply>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IssueAttentionReplyPage {
    pub decisions: Vec<IssueDecision>,
    pub next_before: Option<i64>,
}

pub fn issue_pulls_path(issue: &str) -> String {
    format!("/api/v1/issues/{issue}/pulls")
}

pub const ISSUE_BOARD_PATH: &str = "/api/v1/issues";
pub const TOP_LEVEL_BOARD_PATH: &str = "/api/v1/issue-board";

pub fn issue_board_path(chat: &str) -> String {
    format!("/api/v1/chats/{chat}/issues")
}

pub fn github_repository(remote: &str) -> Option<String> {
    let path = remote
        .trim()
        .strip_prefix("https://github.com/")
        .or_else(|| remote.trim().strip_prefix("git@github.com:"))
        .or_else(|| remote.trim().strip_prefix("ssh://git@github.com/"))?;
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, repo) = path.split_once('/')?;
    (repository_part(owner) && repository_part(repo)).then(|| path.to_ascii_lowercase())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkBranch {
    pub repo_id: String,
    pub repository: String,
    pub head_repository: String,
    pub branch: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FindPull {
    pub operation_id: String,
    pub task_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BranchResult {
    pub branch: WorkBranch,
    pub state: String,
    pub url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PullDiscovery {
    pub operation_id: String,
    pub task_id: String,
    pub automatic: bool,
    pub state: String,
    pub results: Vec<BranchResult>,
}

pub fn pull_discoveries_path(issue: &str) -> String {
    format!("/api/v1/issues/{issue}/pull-discoveries")
}

#[cfg(test)]
mod board_cursor_tests {
    use super::IssueBoardCursor;

    #[test]
    fn board_cursors_are_canonical_url_safe_keysets() {
        for ms in [0, 1_795_608_826_101] {
            let cursor = IssueBoardCursor {
                entry_ms: ms,
                issue_id: "7c183af5-6c6b-4f04-99d7-e6268eac8251".into(),
            };
            assert_eq!(IssueBoardCursor::decode(&cursor.encode()), Some(cursor));
        }
        for invalid in [
            "42",
            "v1.01.id",
            "v1.+1.id",
            "v1.-1.id",
            "v1.0.",
            "v1.0.id.part",
            "v1.0.id&scope=all",
            "v2.0.id",
            "v1.9223372036854775808.id",
        ] {
            assert_eq!(IssueBoardCursor::decode(invalid), None, "{invalid}");
        }
    }
}
