use anyhow::{Context, Result, ensure};
use protocol::{AgentProfile, ClaimDecision, RunnerProfile, TaskAssignment};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs::File,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Deserialize)]
pub struct Config {
    pub runner_id: String,
    pub host_id: String,
    pub slot_limit: u32,
    pub repos: Vec<Repo>,
    pub agents: Vec<AgentProfile>,
}

#[derive(Deserialize, Serialize)]
pub struct Repo {
    pub id: String,
    pub path: PathBuf,
    pub default_branch: Option<String>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let path = path
            .canonicalize()
            .context("resolve Runner configuration path")?;
        let mut config: Self = serde_json::from_reader(File::open(&path)?)?;
        let root = path
            .parent()
            .context("Runner configuration has no directory")?;
        let mut repos = HashSet::new();
        for repo in &mut config.repos {
            ensure!(
                !repo.id.trim().is_empty() && repos.insert(repo.id.clone()),
                "invalid or duplicate repo ID"
            );
            repo.path = root.join(&repo.path);
        }
        let profile = config.profile(0);
        ensure!(profile.valid(), "invalid Runner configuration");
        let mut agents = HashSet::new();
        ensure!(
            config.agents.iter().all(|agent| agents.insert(&agent.id)),
            "duplicate named Agent ID"
        );
        Ok(config)
    }

    pub fn profile(&self, unfinished: u32) -> RunnerProfile {
        RunnerProfile {
            runner_id: self.runner_id.clone(),
            host_id: self.host_id.clone(),
            repos: self
                .repos
                .iter()
                .filter(|repo| repo.usable())
                .map(|repo| repo.id.clone())
                .collect(),
            agents: self.agents.clone(),
            slot_limit: self.slot_limit,
            running_slots: 0,
            available_for_assignment: self.slot_limit.saturating_sub(unfinished),
        }
    }

    pub fn decide(&self, assignment: &TaskAssignment, profile: &RunnerProfile) -> ClaimDecision {
        let agent = &assignment.placement.agent;
        let valid = agent.category == "issue-router"
            && profile.agents.contains(agent)
            && protocol::matching_issue_agent(profile, &assignment.work).is_some();
        if valid {
            ClaimDecision::Accepted
        } else {
            ClaimDecision::Rejected {
                reason: "the assigned Agent revision, repositories, or required capabilities are unavailable".to_owned(),
            }
        }
    }

    pub fn repositories_for(&self, assignment: &TaskAssignment) -> Result<String> {
        let repos: Vec<_> = self
            .repos
            .iter()
            .filter(|repo| assignment.work.repo_ids.contains(&repo.id))
            .collect();
        Ok(serde_json::to_string(&repos)?)
    }
}

impl Repo {
    fn usable(&self) -> bool {
        self.path.is_dir()
            && Command::new("git")
                .args(["rev-parse", "--git-dir"])
                .current_dir(&self.path)
                .output()
                .is_ok_and(|result| result.status.success())
    }
}
