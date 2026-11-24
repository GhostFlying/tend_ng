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
pub struct IssueCard {
    pub id: String,
    pub parent_issue_id: Option<String>,
    pub title: String,
    pub issue_state: String,
    pub task_state: String,
    pub pulls: Vec<PullLink>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IssueBoard {
    pub issues: Vec<IssueCard>,
    pub next_before: Option<i64>,
}

pub fn issue_pulls_path(issue: &str) -> String {
    format!("/api/v1/issues/{issue}/pulls")
}

pub fn issue_board_path(chat: &str) -> String {
    format!("/api/v1/chats/{chat}/issues")
}
