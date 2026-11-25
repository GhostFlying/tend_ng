use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentObservation {
    pub id: String,
    pub sender_thread_id: String,
    pub receiver_thread_ids: Vec<String>,
    pub status: String,
    pub tool: String,
    #[serde(default)]
    pub agents_states: BTreeMap<String, SubagentState>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentState {
    pub status: String,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewConclusion {
    pub child_thread_id: String,
    pub candidate: String,
    pub scope: String,
    pub conclusion: String,
}

/// Immutable evidence target and the exact Task that asked for it.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct TendReview {
    pub request_id: String,
    pub author_issue_id: String,
    pub author_task_id: String,
    pub candidate: String,
    pub scope: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TendReviewConclusion {
    pub conclusion: String,
    pub evidence: String,
}

impl TendReviewConclusion {
    pub fn valid(&self) -> bool {
        !self.conclusion.trim().is_empty() && !self.evidence.trim().is_empty()
    }
}

/// Git mounts, when wanted, must use an exact commit, never an advancing ref.
pub fn valid_tend_review(candidate: &str, scope: &str, work: &crate::DispatchIssue) -> bool {
    !candidate.trim().is_empty()
        && !scope.trim().is_empty()
        && !work.title.trim().is_empty()
        && !work.objective.trim().is_empty()
        && work
            .agent
            .as_ref()
            .is_none_or(|agent| agent.valid() && agent.runner_id.is_none())
        && work.base_refs.len() == work.repo_ids.len()
        && work.repo_ids.iter().all(|repo| {
            work.base_refs.get(repo).is_some_and(|sha| {
                matches!(sha.len(), 40 | 64) && sha.bytes().all(|b| b.is_ascii_hexdigit())
            })
        })
}
