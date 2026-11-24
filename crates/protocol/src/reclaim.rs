use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkspaceReclaim {
    pub id: String,
    pub issue_id: String,
    pub keys: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkspaceReclaimReceipt {
    pub id: String,
    pub error: Option<String>,
}
