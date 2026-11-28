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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_stamp: Option<WorkspaceStamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_thread: Option<NativeThreadBinding>,
}

/// Provenance of a stopped native turn. The receiving Runner checks the
/// execution stamp against its own launch; these paths never select a launch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeThreadBinding {
    pub run_id: String,
    pub thread_id: String,
    pub turn_id: String,
    pub execution: NativeExecutionStamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeExecutionStamp {
    pub host_id: String,
    pub workspace: WorkspaceStamp,
    pub codex_home: WorkspaceStamp,
    /// Versioned digest of the named Agent and effective execution context.
    pub compatibility: String,
}

/// Identity of the directory a Runner actually launched in. Used only for
/// same-host verification after a stopped Run, never to choose another path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceStamp {
    pub path: std::path::PathBuf,
    pub device: u64,
    pub inode: u64,
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
    // For an older configuration that used the Runner DB's adjacent .runs
    // directory. Explicitly asserted by the same-host upgrade operator.
    #[serde(default)]
    pub predecessor_workspace_root: Option<String>,
}
