use crate::DispatchIssue;
use serde::{Deserialize, Serialize};

pub const RUNNER_PROTOCOL: u32 = 1;
pub const RUNNER_SYNC_BATCH: usize = 32;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct AgentProfile {
    pub id: String,
    pub category: String,
    pub revision: String,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct RunnerProfile {
    pub runner_id: String,
    pub host_id: String,
    pub repos: Vec<String>,
    pub agents: Vec<AgentProfile>,
    pub slot_limit: u32,
    pub running_slots: u32,
    pub available_for_assignment: u32,
}

impl RunnerProfile {
    pub fn valid(&self) -> bool {
        crate::valid_id(&self.runner_id)
            && crate::valid_id(&self.host_id)
            && self.running_slots <= self.slot_limit
            && self.available_for_assignment <= self.slot_limit - self.running_slots
            && self.agents.iter().all(|agent| {
                crate::valid_id(&agent.id)
                    && !agent.category.is_empty()
                    && !agent.revision.is_empty()
            })
    }
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Placement {
    pub assignment_id: String,
    pub runner_id: String,
    pub host_id: String,
    pub agent: AgentProfile,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TaskAssignment {
    pub placement: Placement,
    pub task_id: String,
    pub issue_id: String,
    pub source_message_id: String,
    pub chat_id: String,
    pub work: DispatchIssue,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum ClaimDecision {
    Accepted,
    Rejected { reason: String },
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ClaimReceipt {
    pub assignment_id: String,
    pub decision: ClaimDecision,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RunnerSync {
    pub protocol: u32,
    pub profile: RunnerProfile,
    pub receipts: Vec<ClaimReceipt>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<RunObservation>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RunnerSyncReply {
    pub assignments: Vec<TaskAssignment>,
    pub confirmed_receipts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub confirmed_runs: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct RunObservation {
    pub event_id: String,
    pub assignment_id: String,
    pub task_id: String,
    pub run_id: String,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub phase: RunPhase,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RunPhase {
    Started,
    Stopped {
        outcome: RunOutcome,
        error: Option<String>,
    },
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    Completed,
    Failed,
}

pub fn runner_sync_path(runner_id: &str) -> String {
    format!("/api/v1/runners/{runner_id}/sync")
}

pub fn matching_issue_agent<'a>(
    profile: &'a RunnerProfile,
    work: &DispatchIssue,
) -> Option<&'a AgentProfile> {
    if !work
        .repo_ids
        .iter()
        .all(|repo| profile.repos.contains(repo))
    {
        return None;
    }
    if !work.required_capabilities.is_empty()
        && !profile.agents.iter().any(|agent| {
            matches!(
                agent.category.as_str(),
                "implementation" | "implementation-economy" | "investigation" | "review"
            ) && work
                .required_capabilities
                .iter()
                .all(|cap| agent.capabilities.contains(cap))
        })
    {
        return None;
    }
    profile
        .agents
        .iter()
        .find(|agent| agent.category == "issue-router")
}
