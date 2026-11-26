use anyhow::{Context, Result, ensure};
use protocol::{AgentProfile, ClaimDecision, RunnerProfile, TaskAssignment};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    net::SocketAddr,
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
    #[serde(default)]
    pub app_servers: HashMap<String, AppServer>,
    pub inspect: Option<Inspection>,
    pub workspace_root: Option<PathBuf>,
    #[serde(default)]
    pub standby: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AppServer {
    pub program: String,
    #[serde(default = "stdio_args")]
    pub args: Vec<String>,
    pub codex_home: Option<PathBuf>,
}

fn stdio_args() -> Vec<String> {
    ["app-server", "--listen", "stdio://"]
        .map(str::to_owned)
        .to_vec()
}

#[derive(Deserialize)]
pub struct Inspection {
    pub listen: SocketAddr,
    pub url: String,
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
        if let Some(workspace) = &mut config.workspace_root {
            let path = root.join(&*workspace);
            std::fs::create_dir_all(&path)?;
            *workspace = path.canonicalize()?;
        }
        let mut repos = HashSet::new();
        for repo in &mut config.repos {
            ensure!(
                !repo.id.trim().is_empty() && repos.insert(repo.id.clone()),
                "invalid or duplicate repo ID"
            );
            repo.path = root.join(&repo.path);
        }
        for (id, app) in &mut config.app_servers {
            ensure!(
                protocol::valid_id(id) && !app.program.trim().is_empty(),
                "invalid App Server configuration {id}"
            );
            let program = Path::new(&app.program);
            if program.is_relative() && program.components().count() > 1 {
                app.program = root
                    .join(program)
                    .to_str()
                    .context("invalid App Server path")?
                    .to_owned();
            }
            if let Some(home) = &mut app.codex_home {
                *home = root.join(&*home);
            }
        }
        if let Some(inspect) = &config.inspect {
            ensure!(
                inspect
                    .url
                    .strip_prefix("http://")
                    .is_some_and(|host| !host.is_empty()),
                "Runner inspection URL must be an HTTP base address"
            );
        }
        let profile = config.profile(0);
        ensure!(profile.valid(), "invalid Runner configuration");
        let mut agents = HashSet::new();
        ensure!(
            config.agents.iter().all(|agent| agents.insert(&agent.id)),
            "duplicate named Agent ID"
        );
        for agent in &config.agents {
            if let Some(id) = &agent.app_server {
                ensure!(
                    config.app_servers.contains_key(id),
                    "Agent {} names an unconfigured App Server {id}",
                    agent.id
                );
            }
        }
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
            inspect_url: self.inspect.as_ref().map(|inspect| inspect.url.clone()),
            workspace_root: self
                .workspace_root
                .as_ref()
                .and_then(|path| path.to_str())
                .map(str::to_owned),
            standby: self.standby,
            shared_reviews: true,
            handoff_slots: true,
        }
    }

    pub fn decide(&self, assignment: &TaskAssignment, profile: &RunnerProfile) -> ClaimDecision {
        let agent = &assignment.placement.agent;
        let valid = profile.agents.contains(agent)
            && (agent.category == "issue-router"
                || assignment
                    .work
                    .required_capabilities
                    .iter()
                    .all(|cap| agent.capabilities.contains(cap)))
            && protocol::matching_task_agent(
                profile,
                &agent.category,
                &assignment.work,
                Some(&agent.id),
            )
            .is_some();
        if valid {
            ClaimDecision::Accepted
        } else {
            ClaimDecision::Rejected {
                reason: "the assigned Agent revision, repositories, or required capabilities are unavailable".to_owned(),
            }
        }
    }

    pub fn native_agent(&self, assignment: &TaskAssignment) -> Result<&AgentProfile> {
        let assigned = &assignment.placement.agent;
        self.agents
            .iter()
            .find(|agent| {
                agent.id == assigned.id
                    && agent.category == assigned.category
                    && (agent.category == "issue-router"
                        || assignment
                            .work
                            .required_capabilities
                            .iter()
                            .all(|capability| agent.capabilities.contains(capability)))
            })
            .context("original named Agent or its required capabilities are unavailable")
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
