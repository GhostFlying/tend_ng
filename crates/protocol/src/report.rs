use serde::{Deserialize, Serialize};

pub const REPORT_REVISIONS_PATH: &str = "/api/v1/report-revisions";

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BackfillReportRevision {
    pub operation_id: String,
    pub old_report_id: String,
    pub new_report_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_message_id: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ReportRevisionReceipt {
    pub operation_id: String,
    pub issue_id: String,
    pub old_report_id: String,
    pub new_report_id: String,
    pub old_message_id: String,
    pub new_message_id: String,
    pub created: bool,
}
