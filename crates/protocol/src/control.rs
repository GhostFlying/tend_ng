use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct ControlRequest {
    pub operation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ControlAction {
    Stop,
    Steer { text: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerControl {
    pub id: String,
    pub task_id: String,
    pub assignment_id: String,
    pub run_id: Option<String>,
    pub action: ControlAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlAck {
    pub id: String,
    pub assignment_id: String,
    pub state: ControlState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlState {
    Pending,
    Stopped,
    Delivered,
    Undelivered,
    Unknown,
    Queued,
}

impl ControlState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Stopped => "stopped",
            Self::Delivered => "delivered",
            Self::Undelivered => "undelivered",
            Self::Unknown => "unknown",
            Self::Queued => "queued",
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ControlView {
    pub operation_id: String,
    pub task_id: String,
    pub run_id: Option<String>,
    pub state: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IssueTransition {
    pub operation_id: String,
    pub state: String,
    pub reason: String,
}
