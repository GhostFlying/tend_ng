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

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PullProvider {
    #[default]
    GitHub,
    Codebase,
}

impl PullProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GitHub => "github",
            Self::Codebase => "codebase",
        }
    }

    pub fn repository(self, remote: &str) -> Option<String> {
        match self {
            Self::GitHub => github_repository(remote),
            Self::Codebase => {
                let remote = remote.trim();
                let path = [
                    "https://code.byted.org/",
                    "git@code.byted.org:",
                    "ssh://git@code.byted.org/",
                    "ssh://git@code.byted.org:22/",
                    "ssh://git@code.byted.org:29418/",
                ]
                .iter()
                .find_map(|prefix| remote.strip_prefix(prefix))?
                .trim_end_matches('/');
                let path = path.strip_suffix(".git").unwrap_or(path);
                codebase_repository(path).then(|| path.to_owned())
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct PullRef {
    pub provider: PullProvider,
    pub repository: String,
    pub number: i64,
}

impl PullRef {
    pub fn parse(url: &str) -> Option<Self> {
        if let Some(pull) = GitHubPull::parse(url) {
            return Some(Self::from(&pull));
        }
        let path = url
            .trim()
            .strip_prefix("https://code.byted.org/")?
            .split(['?', '#'])
            .next()?
            .trim_end_matches('/');
        let (repository, number) = path.split_once("/merge_requests/")?;
        if !codebase_repository(repository)
            || number.is_empty()
            || !number.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        Some(Self {
            provider: PullProvider::Codebase,
            repository: repository.to_owned(),
            number: number.parse().ok().filter(|number| *number > 0)?,
        })
    }

    pub fn url(&self) -> String {
        let base = match self.provider {
            PullProvider::GitHub => "github.com",
            PullProvider::Codebase => "code.byted.org",
        };
        let collection = match self.provider {
            PullProvider::GitHub => "pull",
            PullProvider::Codebase => "merge_requests",
        };
        format!(
            "https://{base}/{}/{collection}/{}",
            self.repository, self.number
        )
    }

    pub fn github(&self) -> Option<GitHubPull> {
        (self.provider == PullProvider::GitHub).then(|| GitHubPull {
            repository: self.repository.clone(),
            number: self.number,
        })
    }
}

impl From<&GitHubPull> for PullRef {
    fn from(pull: &GitHubPull) -> Self {
        Self {
            provider: PullProvider::GitHub,
            repository: pull.repository.clone(),
            number: pull.number,
        }
    }
}

pub fn codebase_repository(value: &str) -> bool {
    value.split_once('/').is_some_and(|(namespace, project)| {
        !namespace.starts_with('-') && repository_part(namespace) && repository_part(project)
    })
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_commit_sha: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PullLink {
    pub url: String,
    pub provider: PullProvider,
    pub repository: String,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_history: Option<crate::IssueTitleHistory>,
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
    #[serde(default)]
    pub provider: PullProvider,
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

#[cfg(test)]
mod provider_tests {
    use super::{GitHubPull, PullProvider, PullRef};

    #[test]
    fn provider_urls_are_canonical_and_distinct() {
        let mr =
            PullRef::parse(" https://code.byted.org/Example/Guide/merge_requests/007/?a=b#note ")
                .unwrap();
        assert_eq!(mr.provider, PullProvider::Codebase);
        assert_eq!(mr.number, 7);
        assert_eq!(
            mr.url(),
            "https://code.byted.org/Example/Guide/merge_requests/7"
        );
        assert!(GitHubPull::parse(&mr.url()).is_none());
        let pr = PullRef::parse("https://github.com/Example/Guide/pull/7/").unwrap();
        assert_eq!(pr.provider, PullProvider::GitHub);
        assert_eq!(pr.url(), "https://github.com/example/guide/pull/7");
        for invalid in [
            "http://code.byted.org/x/y/merge_requests/2",
            "https://code.byted.org.evil.test/x/y/merge_requests/2",
            "https://code.byted.org/x/y/merge_requests/2/diffs",
            "https://code.byted.org/x/../merge_requests/2",
            "https://code.byted.org/-x/y/merge_requests/2",
            "https://code.byted.org/x/y/merge_requests/+2",
            "https://code.byted.org/x/y/merge_requests/0",
            "https://code.byted.org/x/%2F/merge_requests/3",
            "https://code.byted.org/x/y/merge_requests/9223372036854775808",
        ] {
            assert!(PullRef::parse(invalid).is_none(), "{invalid}");
        }
    }

    #[test]
    fn codebase_git_remotes_preserve_repository_case() {
        for remote in [
            "https://code.byted.org/Example/Guide.git/",
            "git@code.byted.org:Example/Guide.git",
            "ssh://git@code.byted.org/Example/Guide.git",
            "ssh://git@code.byted.org:29418/Example/Guide.git",
        ] {
            assert_eq!(
                PullProvider::Codebase.repository(remote).as_deref(),
                Some("Example/Guide")
            );
            assert!(PullProvider::GitHub.repository(remote).is_none());
        }
    }
}
