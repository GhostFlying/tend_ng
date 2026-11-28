use crate::launch_versions::LaunchVersions;
use anyhow::{Context, Result, ensure};
use protocol::{AgentCategory, AgentProfile, ClaimDecision, RunnerProfile, TaskAssignment};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, OnceLock, RwLock},
};

#[derive(Deserialize)]
pub struct Config {
    pub runner_id: String,
    pub host_id: String,
    pub slot_limit: u32,
    pub repos: Vec<Repo>,
    #[serde(flatten)]
    startup_inventory: Inventory,
    #[serde(skip)]
    inventory: OnceLock<RwLock<Arc<Inventory>>>,
    #[serde(skip)]
    versions: Option<LaunchVersions>,
    pub inspect: Option<Inspection>,
    pub workspace_root: Option<PathBuf>,
    #[serde(default)]
    pub standby: bool,
}

#[derive(Default, Deserialize)]
pub struct Inventory {
    pub(super) agents: Vec<AgentProfile>,
    #[serde(default)]
    pub app_servers: HashMap<String, AppServer>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
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
    pub fn load(
        path: &Path,
        db_path: &Path,
        default: AppServer,
        predecessor: Option<&Path>,
    ) -> Result<Self> {
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
        if let Some(inspect) = &config.inspect {
            ensure!(
                inspect
                    .url
                    .strip_prefix("http://")
                    .is_some_and(|host| !host.is_empty()),
                "Runner inspection URL must be an HTTP base address"
            );
        }
        let workspace = config
            .workspace_root
            .clone()
            .unwrap_or_else(|| crate::default_workspace_root(db_path));
        ensure!(protocol::valid_id(&config.host_id), "invalid Host ID");
        let versions =
            LaunchVersions::open(&crate::host_data_root(&workspace, &config.host_id), default)?;
        if let Some(path) = predecessor {
            versions.import_predecessor(path, &config.host_id)?;
        }
        config.startup_inventory.normalize(root)?;
        versions.publish(&mut config.startup_inventory)?;
        config.versions = Some(versions);
        config
            .inventory
            .get_or_init(|| RwLock::new(Arc::new(std::mem::take(&mut config.startup_inventory))));
        let profile = config.profile(0);
        ensure!(profile.valid(), "invalid Runner configuration");
        Ok(config)
    }

    pub fn versions(&self) -> &LaunchVersions {
        self.versions.as_ref().expect("loaded Runner configuration")
    }

    pub fn inventory(&self) -> Arc<Inventory> {
        self.inventory
            .get()
            .expect("loaded configuration")
            .read()
            .unwrap()
            .clone()
    }

    pub fn refresh_inventory(&self, bytes: &[u8], root: &Path) -> Result<()> {
        let mut next: Inventory = serde_json::from_slice(bytes).context("parse Agent inventory")?;
        next.normalize(root)?;
        self.versions().publish(&mut next)?;
        *self.inventory.get().unwrap().write().unwrap() = Arc::new(next);
        Ok(())
    }

    pub fn profile(&self, unfinished: u32) -> RunnerProfile {
        self.profile_with_inventory(unfinished, &self.inventory())
    }

    pub fn profile_with_inventory(&self, unfinished: u32, inventory: &Inventory) -> RunnerProfile {
        RunnerProfile {
            runner_id: self.runner_id.clone(),
            host_id: self.host_id.clone(),
            repos: self
                .repos
                .iter()
                .filter(|repo| repo.usable())
                .map(|repo| repo.id.clone())
                .collect(),
            agents: inventory.agents.clone(),
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
            codebase_review_comments: true,
            direct_issue_owners: true,
            dynamic_categories: true,
        }
    }

    pub fn decide(
        assignment: &TaskAssignment,
        profile: &RunnerProfile,
        catalogue: &[AgentCategory],
    ) -> ClaimDecision {
        let agent = &assignment.placement.agent;
        let top_level = assignment.parent_issue_id.is_none();
        let rejected = |reason| ClaimDecision::Rejected { reason };
        let category =
            match protocol::admitted_agent_category(catalogue, &agent.category, Some(top_level)) {
                Ok(category) => category,
                Err(reason) => return rejected(reason),
            };
        if assignment.role() != Some(category.role)
            || !profile.supports_category(category, top_level)
        {
            return rejected("the assigned Agent role is not supported".to_owned());
        }
        if !profile.agents.contains(agent)
            || !assignment
                .work
                .repo_ids
                .iter()
                .all(|id| profile.repos.contains(id))
        {
            return rejected(
                "the assigned Agent revision or repositories are unavailable".to_owned(),
            );
        }
        // A catalogue-aware Server checked deployment capabilities when it admitted this
        // work. Later changes to other Agents cannot invalidate the issued contract.
        if !profile.dynamic_categories
            && protocol::matching_task_agent(
                profile,
                category,
                catalogue,
                &assignment.work,
                Some(&agent.id),
            )
            .is_none()
        {
            return rejected("the required capabilities are unavailable".to_owned());
        }
        ClaimDecision::Accepted
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

impl Inventory {
    fn normalize(&mut self, root: &Path) -> Result<()> {
        for (id, app) in &mut self.app_servers {
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
        let mut agents = HashSet::new();
        ensure!(
            self.agents.iter().all(|agent| agents.insert(&agent.id)),
            "duplicate named Agent ID"
        );
        for agent in &self.agents {
            if let Some(id) = &agent.app_server {
                ensure!(
                    self.app_servers.contains_key(id),
                    "Agent {} names an unconfigured App Server {id}",
                    agent.id
                );
            }
        }
        ensure!(
            self.agents.iter().all(|agent| protocol::valid_id(&agent.id)
                && protocol::valid_category_id(&agent.category)
                && !agent.revision.is_empty()),
            "invalid named Agent ID, category or revision"
        );
        Ok(())
    }
}

pub struct InventoryRefresh {
    path: PathBuf,
    root: PathBuf,
    read: Option<Vec<u8>>,
    last_error: Option<String>,
}

impl InventoryRefresh {
    pub fn new(path: PathBuf) -> Result<Self> {
        let resolved = path
            .canonicalize()
            .context("resolve Runner configuration path")?;
        let root = resolved
            .parent()
            .context("Runner configuration has no directory")?
            .to_owned();
        Ok(Self {
            path,
            root,
            read: None,
            last_error: None,
        })
    }

    pub fn poll(&mut self, config: &Config) {
        let error = match std::fs::read(&self.path) {
            Ok(bytes) if self.read.as_ref() == Some(&bytes) => return,
            Ok(bytes) => {
                let result = config.refresh_inventory(&bytes, &self.root);
                self.read = Some(bytes);
                result.err().map(|error| format!("{error:#}"))
            }
            Err(error) => {
                self.read = None;
                Some(error.to_string())
            }
        };
        if error != self.last_error {
            if let Some(error) = &error {
                eprintln!("Agent inventory refresh (retaining the last valid inventory): {error}");
            }
            self.last_error = error;
        }
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
