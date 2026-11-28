use crate::config::{AppServer, Inventory};
use anyhow::{Context, Result, ensure};
use protocol::AgentProfile;
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

/// Only the current inventory is advertised. Older launchers remain readable
/// on this Host for contracts the Server reserved before a configuration edit.
pub struct LaunchVersions {
    database: PathBuf,
    default: AppServer,
}

impl LaunchVersions {
    pub fn open(root: &Path, default: AppServer) -> Result<Self> {
        std::fs::create_dir_all(root)?;
        let versions = Self {
            database: root.join("agent-launches.sqlite"),
            default,
        };
        versions.connect()?.execute_batch(
            "CREATE TABLE IF NOT EXISTS versions (
                 profile TEXT PRIMARY KEY, agent_id TEXT NOT NULL, revision TEXT NOT NULL,
                 launcher TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS named_revision ON versions(agent_id, revision);",
        )?;
        Ok(versions)
    }

    fn connect(&self) -> Result<Connection> {
        let db = Connection::open(&self.database)?;
        db.busy_timeout(Duration::from_secs(5))?;
        Ok(db)
    }

    fn effective(&self, configured: Option<&AppServer>) -> AppServer {
        let mut app = configured.unwrap_or(&self.default).clone();
        if app.codex_home.is_none() {
            app.codex_home.clone_from(&self.default.codex_home);
        }
        // Keep the program selected by this generation's PATH. Local fixture
        // or unavailable binaries may only become available at execution time.
        let program = Path::new(&app.program);
        if program.components().count() == 1
            && let Some(path) = std::env::var_os("PATH")
            && let Some(found) = std::env::split_paths(&path)
                .map(|dir| dir.join(program))
                .find(|file| {
                    file.metadata()
                        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                })
            && let Ok(absolute) = std::path::absolute(found)
            && let Some(absolute) = absolute.to_str()
        {
            app.program = absolute.to_owned();
        }
        app
    }

    pub fn publish(&self, inventory: &mut Inventory) -> Result<()> {
        let mut db = self.connect()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for profile in &mut inventory.agents {
            let launcher = self.effective(
                profile
                    .app_server
                    .as_ref()
                    .and_then(|id| inventory.app_servers.get(id)),
            );
            let launcher = serde_json::to_string(&launcher)?;
            let mut key = serde_json::to_string(&profile)?;
            if exact_version(&tx, &key)?.as_deref() != Some(&launcher) {
                if tx.query_row(
                    "SELECT EXISTS (SELECT 1 FROM versions WHERE agent_id=?1 AND revision=?2)",
                    params![profile.id, profile.revision],
                    |r| r.get::<_, bool>(0),
                )? {
                    // Preserve human-supplied revision names when possible. An
                    // edit under the same label needs its own wire identity.
                    let digest = Sha256::digest(serde_json::to_vec(&(&key, &launcher))?);
                    profile.revision = format!("{}@{digest:x}", profile.revision);
                    key = serde_json::to_string(&profile)?;
                }
                save(&tx, profile, &key, &launcher)?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn launcher(&self, profile: &AgentProfile) -> Result<Option<AppServer>> {
        exact_version(&self.connect()?, &serde_json::to_string(profile)?)?
            .map(|json| Ok(serde_json::from_str(&json)?))
            .transpose()
    }

    // Before the first upgrade with launcher history, the predecessor alone
    // knows what it actually accepted. Read its ledger before Server replace
    // allows either generation to relinquish work.
    pub fn import_predecessor(&self, path: &Path, host: &str) -> Result<()> {
        let prior = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let prior_host: String =
            prior.query_row("SELECT host_id FROM runner_instance", [], |r| r.get(0))?;
        ensure!(
            host == prior_host,
            "predecessor launcher ledger belongs to another Host"
        );
        let mut q = prior.prepare(
            "SELECT DISTINCT json_extract(a.contract, '$.placement.agent'),
                 coalesce((SELECT r.app_server FROM local_runs r WHERE r.assignment_id = a.id
                     AND r.app_server IS NOT NULL ORDER BY r.rowid DESC LIMIT 1), a.app_server)
             FROM received_assignments a WHERE execution_state NOT IN ('rejected','cancelled','handed_off')"
        )?;
        let mut db = self.connect()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let rows = q.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?;
        for row in rows {
            let (raw, app) = row?;
            let profile: AgentProfile = serde_json::from_str(&raw)?;
            let app: Option<AppServer> = app.as_deref().map(serde_json::from_str).transpose()?;
            ensure!(
                profile.app_server.is_none() || app.is_some(),
                "missing original launcher for {}",
                profile.id
            );
            let launch = serde_json::to_string(&self.effective(app.as_ref()))?;
            save(&tx, &profile, &serde_json::to_string(&profile)?, &launch)
                .with_context(|| format!("import original launcher for {}", profile.id))?;
        }
        tx.commit()?;
        Ok(())
    }
}

fn exact_version(db: &Connection, profile: &str) -> Result<Option<String>> {
    Ok(db
        .query_row(
            "SELECT launcher FROM versions WHERE profile=?1",
            [profile],
            |r| r.get(0),
        )
        .optional()?)
}

fn save(db: &Connection, profile: &AgentProfile, key: &str, launcher: &str) -> Result<()> {
    let inserted = db.execute(
        "INSERT INTO versions(profile, agent_id, revision, launcher) VALUES(?1, ?2, ?3, ?4)
          ON CONFLICT(profile) DO NOTHING",
        params![key, profile.id, profile.revision, launcher],
    )?;
    ensure!(
        inserted != 0 || exact_version(db, key)?.as_deref() == Some(launcher),
        "conflicting launcher for Agent {} revision {}",
        profile.id,
        profile.revision
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn replacements_can_claim_original_launchers_after_reconfiguration() -> Result<()> {
        let root = std::env::temp_dir().join(protocol::new_id());
        let default = AppServer {
            program: "fixture-default-cli".into(),
            args: vec!["app-server".into()],
            codex_home: Some(root.join("default-home")),
        };
        let old = LaunchVersions::open(&root, default.clone())?;
        let inventory = |program: &str, model: &str| -> Result<Inventory> {
            Ok(serde_json::from_value(json!({
                "agents": [
                    {"id":"leaf", "category":"implementation","revision":"one","app_server":"worker","model":model},
                    {"id":"owner", "category":"issue-router","revision":"one"}
                ],
                "app_servers":{"worker":{"program":program,"args":["serve"]}}
            }))?)
        };
        let mut initial = inventory("original-cli", "model-a")?;
        old.publish(&mut initial)?;
        let original = initial.agents[0].clone();
        let previous_default = initial.agents[1].clone();
        drop(old);

        let replacement = LaunchVersions::open(
            &root,
            AppServer {
                program: "new-default".into(),
                codex_home: Some(root.join("new-home")),
                ..default.clone()
            },
        )?;
        let mut cli_edit = inventory("new-cli", "model-a")?;
        replacement.publish(&mut cli_edit)?;
        assert_ne!(cli_edit.agents[0].revision, original.revision);
        assert_ne!(cli_edit.agents[1].revision, previous_default.revision);
        let mut model_edit = inventory("new-cli", "model-b")?;
        replacement.publish(&mut model_edit)?;
        assert_ne!(model_edit.agents[0].revision, cli_edit.agents[0].revision);
        let mut reload = inventory("new-cli", "model-b")?;
        replacement.publish(&mut reload)?;
        assert_eq!(reload.agents, model_edit.agents);
        let original_launch = replacement.launcher(&original)?.unwrap();
        assert_eq!(original_launch.program, "original-cli");
        assert_eq!(original_launch.args, ["serve"]);
        assert_eq!(original_launch.codex_home, default.codex_home);
        assert_eq!(replacement.launcher(&previous_default)?, Some(default));
        assert_eq!(
            replacement
                .launcher(&model_edit.agents[0])?
                .unwrap()
                .program,
            "new-cli"
        );
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
