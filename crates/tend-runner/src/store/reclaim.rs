use super::Store;
use anyhow::Result;
use protocol::{WorkspaceReclaim, WorkspaceReclaimReceipt};
use rusqlite::{Connection, OptionalExtension, params};

pub fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE local_workspace_reclaims (
            id TEXT PRIMARY KEY, command TEXT NOT NULL, receipt TEXT, acknowledged INTEGER NOT NULL DEFAULT 0
         );",
    )?;
    db.pragma_update(None, "user_version", 5)?;
    Ok(())
}

pub fn accept(db: &Connection, command: &WorkspaceReclaim) -> Result<()> {
    db.execute(
        "INSERT INTO local_workspace_reclaims(id,command) VALUES (?1,?2) ON CONFLICT DO NOTHING",
        params![command.id, serde_json::to_string(command)?],
    )?;
    Ok(())
}

impl Store {
    pub fn next_reclaim(&self) -> Result<Option<WorkspaceReclaim>> {
        let raw: Option<String> = self.0.query_row(
            "SELECT c.command FROM local_workspace_reclaims c WHERE c.receipt IS NULL AND NOT EXISTS (
                SELECT 1 FROM received_assignments a
                 WHERE (a.execution_state IN ('queued','active','waiting','waiting_external') OR a.retains_workspace = 1)
                   AND coalesce(json_extract(a.contract, '$.workspace_key'), json_extract(a.contract, '$.parent_issue_id')) IN (
                     SELECT value FROM json_each(c.command, '$.keys')
                   )
            ) ORDER BY c.rowid LIMIT 1", [], |row| row.get(0),
        ).optional()?;
        raw.map(|raw| Ok(serde_json::from_str(&raw)?)).transpose()
    }

    pub fn save_reclaim(&self, result: WorkspaceReclaimReceipt) -> Result<()> {
        self.0.execute(
            "UPDATE local_workspace_reclaims SET receipt = ?2 WHERE id = ?1",
            params![result.id, serde_json::to_string(&result)?],
        )?;
        Ok(())
    }

    pub fn reclaim_receipts(&self) -> Result<Vec<WorkspaceReclaimReceipt>> {
        let mut query = self.0.prepare(
            "SELECT receipt FROM local_workspace_reclaims WHERE acknowledged = 0
                                             AND receipt IS NOT NULL ORDER BY rowid LIMIT ?1",
        )?;
        query
            .query_map([protocol::RUNNER_SYNC_BATCH], |row| row.get::<_, String>(0))?
            .map(|raw| Ok(serde_json::from_str(&raw?)?))
            .collect()
    }
}
