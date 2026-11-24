use crate::{AttentionRequest, DispatchIssue};
use serde::{Deserialize, Serialize};

pub const RUNNER_PROTOCOL: u32 = 1;
pub const RUNNER_SYNC_BATCH: usize = 32;
pub const WORK_CATEGORIES: &[&str] = &[
    "implementation",
    "implementation-economy",
    "investigation",
    "review",
];

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_issue_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub followup: Option<CoordinationFollowup>,
    pub work: DispatchIssue,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CoordinationFollowup {
    pub previous_task_id: String,
    pub issue_state: String,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub records: Vec<RunnerRecord>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RunnerSyncReply {
    pub assignments: Vec<TaskAssignment>,
    pub confirmed_receipts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub confirmed_runs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub confirmed_operations: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<IssueDelivery>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uploads: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum RunnerRecord {
    Run(RunObservation),
    Operation(WorkOperation),
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct WorkOperation {
    pub operation_id: String,
    pub assignment_id: String,
    pub run_id: String,
    pub action: WorkAction,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkAction {
    Delegate {
        issue_id: String,
        task_id: String,
        category: String,
        work: DispatchIssue,
    },
    CloseIssue {
        summary: String,
    },
    SubmitArtifact {
        name: String,
        bytes: u64,
    },
    ReportIssue {
        summary: String,
        artifact_ids: Vec<String>,
    },
    AskAttention {
        request: AttentionRequest,
    },
    EscalateAttention {
        attention_id: String,
    },
    AnswerChildAttention {
        attention_id: String,
        answer: String,
    },
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct IssueDelivery {
    pub assignment_id: String,
    #[serde(flatten)]
    pub input: IssueInput,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct IssueInput {
    pub input_id: String,
    pub child_issue_id: String,
    pub child_task_id: String,
    #[serde(flatten)]
    pub cause: IssueInputCause,
    pub summary: String,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IssueInputCause {
    IssueClosed,
    AttentionRequested {
        attention_id: String,
        request: AttentionRequest,
    },
    AttentionAnswered {
        attention_id: String,
        answer: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        user_message_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        user_text: Option<String>,
    },
    ArtifactAvailable {
        artifact: Artifact,
    },
    NoCandidate {
        missing_repo_ids: Vec<String>,
        category: String,
        required_capabilities: Vec<String>,
    },
    TaskStopped {
        run_id: String,
        outcome: RunOutcome,
    },
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Artifact {
    pub id: String,
    pub name: String,
    pub bytes: u64,
    pub available: bool,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct RunObservation {
    pub event_id: String,
    pub assignment_id: String,
    pub task_id: String,
    pub run_id: String,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_ids: Vec<String>,
    pub phase: RunPhase,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RunPhase {
    Started,
    Stopped {
        outcome: RunOutcome,
        error: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        waiting_for_children: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        waiting_for_attention: bool,
    },
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    Completed,
    Failed,
}

pub fn runner_sync_path(runner_id: &str) -> String {
    format!("/api/v1/runners/{runner_id}/sync")
}

pub fn reconcile_path(runner_id: &str, task_id: &str) -> String {
    format!("/api/v1/runners/{runner_id}/reconcile/{task_id}")
}

pub fn matching_task_agent<'a>(
    profile: &'a RunnerProfile,
    category: &str,
    work: &DispatchIssue,
    named: Option<&str>,
) -> Option<&'a AgentProfile> {
    if !work
        .repo_ids
        .iter()
        .all(|repo| profile.repos.contains(repo))
    {
        return None;
    }
    let satisfies = |agent: &AgentProfile| {
        work.required_capabilities
            .iter()
            .all(|cap| agent.capabilities.contains(cap))
    };
    if category != "issue-router" {
        return profile.agents.iter().find(|agent| {
            agent.category == category && named.is_none_or(|id| agent.id == id) && satisfies(agent)
        });
    }
    if !work.required_capabilities.is_empty()
        && !profile
            .agents
            .iter()
            .any(|agent| WORK_CATEGORIES.contains(&agent.category.as_str()) && satisfies(agent))
    {
        return None;
    }
    profile
        .agents
        .iter()
        .find(|agent| agent.category == "issue-router" && named.is_none_or(|id| agent.id == id))
}
