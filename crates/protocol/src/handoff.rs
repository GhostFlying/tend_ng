use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffPhase {
    Queued,
    Waiting,
    WaitingExternal,
    Finished,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffContext {
    pub phase: HandoffPhase,
    pub operations: Vec<crate::WorkOperation>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffProof {
    pub id: String,
    pub assignment_id: String,
    pub task_id: String,
    pub successor: String,
    pub context: HandoffContext,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ReplaceRunner {
    pub successor: String,
}
