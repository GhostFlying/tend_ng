use serde::{Deserialize, Serialize};

pub const FEISHU_SUGGESTIONS_PATH: &str = "/api/v1/feishu-suggestions";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct FeishuEvidence {
    pub id: String,
    pub sender: String,
    pub text: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub href: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeishuAssessment {
    pub headline: String,
    pub overview: String,
    pub open_questions: Vec<String>,
    pub evidence_ids: Vec<String>,
    pub disposition: FeishuDisposition,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FeishuDisposition {
    Open,
    Resolved,
    Unclear,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FeishuSuggestionEvidence {
    pub id: String,
    pub revision: i64,
    pub chat_name: String,
    pub anchor: FeishuEvidence,
    pub mentions: Vec<FeishuEvidence>,
    pub replies: Vec<FeishuEvidence>,
    pub context_status: String,
    pub assessment: Option<FeishuAssessment>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeishuDiscussionRequest {
    pub revision: i64,
    pub chat_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeishuHandoffRequest {
    pub revision: i64,
    pub retry_generation: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeishuIgnoreRequest {
    pub revision: i64,
    pub ignored: bool,
}
