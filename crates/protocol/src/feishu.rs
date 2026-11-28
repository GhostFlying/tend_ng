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
    #[serde(default)]
    pub source: FeishuSource,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct FeishuSource {
    pub author_id: Option<String>,
    pub mentions: Vec<FeishuPerson>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub mention_bindings: Vec<FeishuMentionBinding>,
    pub reply_to: Option<String>,
    pub quote_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub quote_metadata_reported: bool,
    pub thread_id: Option<String>,
    pub root_id: Option<String>,
    pub position: Option<u64>,
    pub withdrawn: bool,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_key: Option<String>,
    pub non_text: bool,
    pub links_unread: bool,
    pub chat_link_only: bool,
    pub created_minute_precision: bool,
    pub edited_minute_precision: bool,
    pub edit_time_unknown: bool,
    pub created_subsecond: bool,
    pub edited_subsecond: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_millis: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_millis: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct FeishuPerson {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct FeishuMentionBinding {
    /// Unicode scalar offsets in the displayed excerpt, not provider bytes.
    pub start: usize,
    pub end: usize,
    pub id: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct FeishuObservation {
    pub through: i64,
    pub read_from: Option<i64>,
    pub read_to: Option<i64>,
    pub can_read_before: bool,
    pub can_read_after: bool,
    pub after_status: String,
    pub gaps: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct FeishuReference {
    pub id: String,
    pub relation: String,
    // available, unavailable (including permissions and cross-chat), or out_of_window
    pub status: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FeishuRelationship {
    RequestedOfYou,
    YouCommitted,
    SomeoneElse,
    ForInformation,
    Answered,
    Completed,
    Withdrawn,
    Unclear,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeishuCardCopy {
    pub request: String,
    pub current: String,
    pub relationship: FeishuRelationship,
    pub request_ids: Vec<String>,
    pub current_ids: Vec<String>,
    pub question: Option<FeishuQuestion>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeishuQuestion {
    pub text: String,
    pub basis: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeishuAssessment {
    pub headline: String,
    pub overview: String,
    pub open_questions: Vec<String>,
    pub evidence_ids: Vec<String>,
    pub disposition: FeishuDisposition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card: Option<FeishuCardCopy>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related_replies: Vec<String>,
    pub context_status: String,
    #[serde(default)]
    pub owner: Option<FeishuPerson>,
    #[serde(default)]
    pub observation: FeishuObservation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assessed_observation: Option<FeishuObservation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assessed_references: Option<Vec<FeishuReference>>,
    #[serde(default)]
    pub coverage_changed: bool,
    #[serde(default)]
    pub references: Vec<FeishuReference>,
    #[serde(default)]
    pub direct: Vec<FeishuEvidence>,
    #[serde(default)]
    pub before: Vec<FeishuEvidence>,
    #[serde(default)]
    pub changed: Vec<String>,
    #[serde(default)]
    pub cited: Vec<FeishuEvidence>,
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
