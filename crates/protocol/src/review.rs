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
