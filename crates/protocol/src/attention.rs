use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AttentionRequest {
    pub question: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub context: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_at: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttentionState {
    Pending,
    AwaitingUser,
    Answered,
    Delivered,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AttentionStatus {
    pub id: String,
    pub state: AttentionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal: Option<ProposalStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_reply: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Attention {
    #[serde(flatten)]
    pub status: AttentionStatus,
    pub issue_id: String,
    pub task_id: String,
    pub source_message_id: String,
    pub question_message_id: Option<String>,
    pub user_message_id: Option<String>,
    pub answer: Option<String>,
    pub request: AttentionRequest,
}

pub fn attentions_path(chat_id: &str) -> String {
    format!("/api/v1/chats/{chat_id}/attentions")
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AnswerIssueAttention {
    pub operation_id: String,
    pub text: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct IssueAttentionReceipt {
    pub operation_id: String,
    pub attention_id: String,
    pub answer: String,
}

/// Present only for a replyable question or a recorded direct reply.
#[derive(Debug, Deserialize, Serialize)]
pub struct IssueAttentionReply {
    pub attention_id: String,
    pub answer: Option<String>,
}

pub fn answer_issue_attention_path(issue_id: &str, attention_id: &str) -> String {
    format!("/api/v1/issues/{issue_id}/attentions/{attention_id}/answer")
}

pub fn issue_attention_replies_path(issue_id: &str) -> String {
    format!("/api/v1/issues/{issue_id}/attention-replies")
}

/// Explicit author intent on a Runner's work operation, not part of the wake.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProposalRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaces: Option<String>,
    pub version: u32,
    pub report_id: String,
    pub artifact_id: String,
    pub title: String,
    pub highlights: Vec<String>,
    pub action: String,
}

impl ProposalRequest {
    pub fn valid(&self) -> bool {
        fn concise(value: &str, max: usize) -> bool {
            !value.trim().is_empty()
                && value.chars().count() <= max
                && !value.contains(['\n', '\r'])
        }
        crate::valid_id(&self.report_id)
            && crate::valid_id(&self.artifact_id)
            && match self.replaces.as_deref() {
                None => self.version == 1,
                Some(id) => crate::valid_id(id) && self.version > 1,
            }
            && concise(&self.title, 200)
            && concise(&self.action, 120)
            && (1..=6).contains(&self.highlights.len())
            && self.highlights.iter().all(|text| concise(text, 300))
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ApprovalDecision {
    pub id: String,
    pub version: u32,
    pub decided_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub continuation_blocked: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProposalStatus {
    pub issue_id: String,
    pub version: u32,
    pub question: String,
    pub action: String,
    pub can_approve: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<ApprovalDecision>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ProposalVersion {
    pub version: u32,
    pub report_id: String,
    pub title: String,
    pub highlights: Vec<String>,
    pub updated_at: i64,
    pub artifact: crate::Artifact,
    pub other_artifacts: Vec<crate::Artifact>,
    pub message_id: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct IssueProposal {
    pub attention_id: String,
    pub question_message_id: Option<String>,
    pub status: ProposalStatus,
    pub versions: Vec<ProposalVersion>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApproveProposal {
    pub version: u32,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub from_issue: bool,
}

pub fn approve_proposal_path(chat_id: &str, attention_id: &str) -> String {
    format!("{}/{attention_id}/approve", attentions_path(chat_id))
}
