use serde::{Deserialize, Serialize};
use std::{
    env, io,
    num::NonZeroU64,
    time::{Duration, Instant},
};

/// Deployment configuration is initially read at startup; subsequent reads
/// have their own cadence regardless of scheduling or heartbeat frequency.
pub struct AgentConfigReload {
    checked_at: Instant,
    interval: Duration,
}

impl AgentConfigReload {
    pub fn from_env() -> io::Result<Self> {
        const KEY: &str = "TEND_AGENT_CONFIG_RELOAD_SECONDS";
        let seconds = match env::var_os(KEY) {
            None => 60,
            Some(value) => value
                .to_str()
                .and_then(|s| s.parse::<NonZeroU64>().ok())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("{KEY} must be a positive integer"),
                    )
                })?
                .get(),
        };
        Ok(Self {
            checked_at: Instant::now(),
            interval: Duration::from_secs(seconds),
        })
    }

    pub fn due(&mut self, now: Instant) -> bool {
        if now.duration_since(self.checked_at) < self.interval {
            return false;
        }
        self.checked_at = now;
        true
    }
}

pub const AGENT_CATEGORIES_PATH: &str = "/api/v1/agent-categories";

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    Coordinator,
    Work,
    Review,
    Router,
    Feishu,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentCategory {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub role: AgentRole,
    #[serde(default)]
    pub top_level: bool,
    #[serde(default)]
    pub delegated: bool,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
}

fn enabled_by_default() -> bool {
    true
}

impl AgentCategory {
    pub fn valid_declaration(&self) -> bool {
        valid_category_id(&self.id)
            && !self.display_name.trim().is_empty()
            && self.display_name.len() <= 128
            && !self.description.trim().is_empty()
            && self.description.len() <= 1024
            && (self.top_level || self.delegated)
            && match self.role {
                AgentRole::Coordinator => !self.delegated,
                AgentRole::Work => true,
                AgentRole::Review => !self.top_level,
                AgentRole::Router | AgentRole::Feishu => false,
            }
    }

    pub fn same_identity(&self, other: &Self) -> bool {
        self.id == other.id
            && self.role == other.role
            && self.top_level == other.top_level
            && self.delegated == other.delegated
    }

    pub fn dispatchable(&self, top_level: bool) -> bool {
        self.enabled
            && if top_level {
                self.top_level
            } else {
                self.delegated
            }
    }
}

pub fn valid_category_id(id: &str) -> bool {
    crate::valid_id(id)
}

pub fn legacy_agent_role(id: &str) -> Option<AgentRole> {
    Some(match id {
        "issue-router" => AgentRole::Coordinator,
        "implementation" | "implementation-economy" | "investigation" => AgentRole::Work,
        "review" => AgentRole::Review,
        _ => return None,
    })
}

pub fn default_agent_categories() -> Vec<AgentCategory> {
    use AgentRole::*;
    [
        (
            "issue-router",
            "Issue coordination",
            "Coordinate multiple independent deliveries as the top-level Owner.",
            Coordinator,
            true,
            false,
        ),
        (
            "implementation",
            "Implementation",
            "Own a regular implementation or other directly deliverable change.",
            Work,
            true,
            true,
        ),
        (
            "implementation-economy",
            "Economy implementation",
            "Small, clear implementations with straightforward acceptance.",
            Work,
            true,
            true,
        ),
        (
            "investigation",
            "Investigation",
            "Investigate and report evidence without unrequested changes.",
            Work,
            true,
            true,
        ),
        (
            "review",
            "Independent review",
            "Review a specific candidate or delegated scope.",
            Review,
            false,
            true,
        ),
        (
            "router",
            "Main Chat Router",
            "Server-local Main Chat routing. Not a Runner assignment.",
            Router,
            false,
            false,
        ),
        (
            "feishu-router",
            "Feishu assessment",
            "Server-local optional Feishu assessment. Not a Runner assignment.",
            Feishu,
            false,
            false,
        ),
    ]
    .into_iter()
    .map(
        |(id, display_name, description, role, top_level, delegated)| AgentCategory {
            id: id.into(),
            display_name: display_name.into(),
            description: description.into(),
            role,
            top_level,
            delegated,
            enabled: true,
        },
    )
    .collect()
}

pub fn check_agent_category<'a>(
    categories: &'a [AgentCategory],
    id: &str,
    top_level: Option<bool>,
) -> Result<&'a AgentCategory, String> {
    let category = admitted_agent_category(categories, id, top_level)?;
    if !category.enabled {
        return Err(format!("Agent category {id} is disabled"));
    }
    Ok(category)
}

/// Role and scope are immutable. Existing Tasks retain them across catalogue edits.
pub fn admitted_agent_category<'a>(
    categories: &'a [AgentCategory],
    id: &str,
    top_level: Option<bool>,
) -> Result<&'a AgentCategory, String> {
    let category = categories
        .iter()
        .find(|category| category.id == id)
        .ok_or_else(|| format!("Unknown Agent category {id}"))?;
    if !category.top_level && !category.delegated {
        return Err(format!(
            "Agent category {id} runs on the Server and cannot be assigned to a Runner"
        ));
    }
    if let Some(top_level) = top_level
        && !(if top_level {
            category.top_level
        } else {
            category.delegated
        })
    {
        let scope = if top_level {
            "top-level dispatch"
        } else {
            "delegation"
        };
        return Err(format!("Agent category {id} does not allow {scope}"));
    }
    Ok(category)
}

#[cfg(test)]
mod reload_tests {
    use super::*;

    #[test]
    fn scheduling_ticks_and_heartbeats_do_not_trigger_early_file_reads() {
        let start = Instant::now();
        let interval = Duration::from_secs(60);
        let mut reload = AgentConfigReload {
            checked_at: start,
            interval,
        };
        assert!(!reload.due(start));
        assert!(!reload.due(start + Duration::from_millis(59_999)));
        assert!(reload.due(start + interval));
        // A delayed cycle performs one refresh and starts a fresh interval.
        let late = start + interval * 4;
        assert!(reload.due(late));
        assert!(!reload.due(late));
        assert!(!reload.due(late + Duration::from_secs(59)));
    }
}
