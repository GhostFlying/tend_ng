use crate::{AttentionRequest, DispatchIssue, IssueMessage};
use serde::{Deserialize, Serialize};

pub const RUNNER_PROTOCOL: u32 = 2;
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_server: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AgentPin {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_id: Option<String>,
}

impl AgentPin {
    pub fn valid(&self) -> bool {
        crate::valid_id(&self.id) && self.runner_id.as_deref().is_none_or(crate::valid_id)
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AgentQuery {
    pub category: String,
    #[serde(default)]
    pub repo_ids: Vec<String>,
    #[serde(default)]
    pub required_capabilities: Vec<String>,
}

impl AgentQuery {
    pub fn valid(&self) -> bool {
        (self.category == "issue-router" || WORK_CATEGORIES.contains(&self.category.as_str()))
            && self
                .repo_ids
                .iter()
                .chain(&self.required_capabilities)
                .all(|id| !id.trim().is_empty())
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AgentCandidate {
    pub runner_id: String,
    pub available: bool,
    pub agent: AgentProfile,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inspect_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub standby: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub shared_reviews: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub handoff_slots: bool,
}

impl RunnerProfile {
    pub fn valid(&self) -> bool {
        crate::valid_id(&self.runner_id)
            && crate::valid_id(&self.host_id)
            && self.running_slots <= self.slot_limit
            && self.available_for_assignment <= self.slot_limit - self.running_slots
            && self.agents.iter().all(|agent| {
                crate::valid_id(&agent.id)
                    && agent.app_server.as_deref().is_none_or(crate::valid_id)
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
    pub workspace_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff: Option<crate::HandoffContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub followup: Option<CoordinationFollowup>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<crate::TendReview>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub review_history: Vec<IssueInput>,
    pub work: DispatchIssue,
}

impl TaskAssignment {
    pub fn workspace_path(&self) -> String {
        match self
            .workspace_key
            .as_deref()
            .or(self.parent_issue_id.as_deref())
        {
            Some(key) => format!("workspaces/{key}"),
            None => format!("tasks/{}", self.task_id),
        }
    }
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
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub drained: bool,
    // Present only after the local drain fence is installed. Counts prepared,
    // running and recovering executions until they actually settle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drain_active_slots: Option<u32>,
    pub profile: RunnerProfile,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reclaim_receipts: Vec<crate::WorkspaceReclaimReceipt>,
    pub receipts: Vec<ClaimReceipt>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<RunObservation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub records: Vec<RunnerRecord>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RunnerSyncReply {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_slot_limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drain_acknowledged: Option<bool>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub proposal_approval: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_reclaims: Vec<crate::WorkspaceReclaim>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub confirmed_reclaims: Vec<String>,
    pub assignments: Vec<TaskAssignment>,
    pub confirmed_receipts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub confirmed_runs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub confirmed_operations: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<IssueDelivery>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub controls: Vec<crate::RunnerControl>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drain_to: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uploads: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum RunnerRecord {
    Run(RunObservation),
    Operation(WorkOperation),
    Control(crate::ControlAck),
    Handoff(crate::HandoffProof),
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
    ObserveSubagent {
        observation: crate::SubagentObservation,
    },
    RecordReview {
        review: crate::ReviewConclusion,
    },
    Delegate {
        issue_id: String,
        task_id: String,
        category: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        isolated_workspace: bool,
        work: DispatchIssue,
    },
    RequestReview {
        issue_id: String,
        task_id: String,
        candidate: String,
        scope: String,
        work: DispatchIssue,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        rereview: bool,
    },
    DeliverReview {
        result: crate::TendReviewConclusion,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
    },
    StopTask {
        task_id: String,
    },
    SetIssueState {
        state: String,
        reason: String,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        proposal: Option<crate::ProposalRequest>,
    },
    EscalateAttention {
        attention_id: String,
    },
    AnswerChildAttention {
        attention_id: String,
        answer: String,
    },
    ForwardIssueMessage {
        child_issue_id: String,
        message_id: String,
    },
    LinkPull {
        url: String,
    },
    WatchExternal {
        condition: crate::ExternalCondition,
        mode: crate::WatchMode,
    },
    ClearExternalWaits,
    ConfirmExternal {
        fact_id: String,
        fact: crate::ExternalFact,
    },
    FindPull {
        task_id: String,
    },
    PostIssueMessage {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to: Option<String>,
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
    ExternalFact {
        watch_id: String,
        fact_id: String,
        mode: crate::WatchMode,
        fact: crate::ExternalFact,
        error: Option<String>,
    },
    IssueClosed,
    IssueMessage {
        message: IssueMessage,
    },
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_id: Option<String>,
    },
    ReviewFinished {
        review: crate::TendReview,
        source_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<crate::TendReviewConclusion>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<String>,
    },
    ReviewRequested {
        review: crate::TendReview,
        base_refs: std::collections::BTreeMap<String, String>,
    },
    TaskCancelled {
        control_id: String,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub branches: Vec<crate::WorkBranch>,
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
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        queued_input: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        waiting_for_external: bool,
    },
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    Completed,
    Failed,
    Cancelled,
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
    matching_agents(
        profile,
        category,
        &work.repo_ids,
        &work.required_capabilities,
    )
    .find(|agent| named.is_none_or(|id| agent.id == id))
}

pub fn matching_agents<'profile, 'filters>(
    profile: &'profile RunnerProfile,
    category: &'filters str,
    repos: &[String],
    capabilities: &'filters [String],
) -> impl Iterator<Item = &'profile AgentProfile> + use<'profile, 'filters> {
    let satisfies = move |agent: &AgentProfile| {
        capabilities
            .iter()
            .all(|cap| agent.capabilities.contains(cap))
    };
    let coordinator = category == "issue-router";
    let eligible = repos.iter().all(|repo| profile.repos.contains(repo))
        && (!coordinator
            || capabilities.is_empty()
            || profile.agents.iter().any(|agent| {
                WORK_CATEGORIES.contains(&agent.category.as_str()) && satisfies(agent)
            }));
    profile.agents.iter().filter(move |agent| {
        eligible && agent.category == category && (coordinator || satisfies(agent))
    })
}
