use super::Store;
use anyhow::{Context, Result, ensure};
use protocol::{AgentCategory, default_agent_categories};
use rusqlite::{Connection, OptionalExtension};
use std::path::Path;

pub(crate) fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS agent_category_snapshot (
             singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
             catalogue TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS task_agent_versions (
             task_id TEXT NOT NULL REFERENCES tasks(id),
             host_id TEXT NOT NULL,
             agent TEXT NOT NULL,
             PRIMARY KEY(task_id, host_id)
         );",
    )?;
    Ok(())
}

pub(super) fn read(db: &Connection) -> Result<Vec<AgentCategory>> {
    let saved: Option<String> = db
        .query_row(
            "SELECT catalogue FROM agent_category_snapshot WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    saved.map_or_else(
        || Ok(default_agent_categories()),
        |raw| Ok(serde_json::from_str(&raw)?),
    )
}

#[derive(Debug)]
pub(crate) struct Unavailable(pub String);

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unavailable {}

fn update(
    previous: Vec<AgentCategory>,
    declared: Vec<AgentCategory>,
) -> Result<Vec<AgentCategory>> {
    let mut defaults = default_agent_categories();
    let mut unique = std::collections::HashSet::new();
    for entry in declared {
        ensure!(
            entry.valid_declaration(),
            "invalid Agent category declaration for {}",
            entry.id
        );
        ensure!(
            unique.insert(entry.id.clone()),
            "duplicate Agent category ID {}",
            entry.id
        );
        if let Some(old) = previous.iter().find(|old| old.id == entry.id) {
            ensure!(
                old.same_identity(&entry),
                "cannot change role or scope of Agent category {}; use a new ID",
                entry.id
            );
        }
        if let Some(builtin) = defaults.iter_mut().find(|old| old.id == entry.id) {
            ensure!(
                builtin.same_identity(&entry),
                "reserved Agent category {}",
                entry.id
            );
            *builtin = entry;
        } else {
            defaults.push(entry);
        }
    }
    for mut old in previous {
        if !defaults.iter().any(|entry| entry.id == old.id) {
            // Retain tombstones so that removing and readding an ID cannot
            // change the meaning of a pending or offline dispatch.
            old.enabled = false;
            defaults.push(old);
        }
    }
    defaults.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(defaults)
}

impl Store {
    pub fn agent_categories(&self) -> Result<Vec<AgentCategory>> {
        read(&self.db)
    }

    pub fn has_saved_category_snapshot(&self) -> Result<bool> {
        Ok(self.db.query_row(
            "SELECT EXISTS (SELECT 1 FROM agent_category_snapshot)",
            [],
            |row| row.get(0),
        )?)
    }

    pub fn refresh_agent_categories(&mut self, path: &Path, optional: bool) -> Result<()> {
        let file = match std::fs::File::open(path) {
            Err(error) if optional && error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            result => {
                result.with_context(|| format!("open category configuration {}", path.display()))?
            }
        };
        let declared: Vec<AgentCategory> =
            serde_json::from_reader(file).context("parse category configuration")?;
        let previous = self.agent_categories()?;
        let next = update(previous.clone(), declared)?;
        if next != previous {
            self.db.execute(
                "INSERT INTO agent_category_snapshot VALUES (1, ?1)
                 ON CONFLICT(singleton) DO UPDATE SET catalogue = excluded.catalogue",
                [serde_json::to_string(&next)?],
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::AgentRole;

    #[test]
    fn category_identity_survives_removal() -> Result<()> {
        let entry = AgentCategory {
            id: "lab-work".into(),
            display_name: "Lab work".into(),
            description: "Lab tasks".into(),
            role: AgentRole::Work,
            top_level: true,
            delegated: false,
            enabled: true,
        };
        let current = update(default_agent_categories(), vec![entry.clone()])?;
        let removed = update(current, vec![])?;
        assert!(
            !removed
                .iter()
                .find(|category| category.id == entry.id)
                .unwrap()
                .enabled
        );
        assert!(
            update(
                removed.clone(),
                vec![AgentCategory {
                    role: AgentRole::Coordinator,
                    ..entry.clone()
                }]
            )
            .is_err()
        );
        assert!(update(removed.clone(), vec![entry.clone(), entry.clone()]).is_err());
        assert!(update(removed, vec![entry]).is_ok());
        Ok(())
    }
}
