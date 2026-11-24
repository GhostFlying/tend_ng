use crate::{config::Repo, store::run::LocalRun};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Deserialize, Serialize)]
struct Mount {
    repo_id: String,
    path: PathBuf,
    base: String,
}

pub fn directory(root: &Path, run: &LocalRun) -> PathBuf {
    match workspace_key(run) {
        Some(issue) => root.join("workspaces").join(issue),
        None => root.join(&run.id),
    }
}

fn workspace_key(run: &LocalRun) -> Option<&str> {
    run.assignment
        .workspace_key
        .as_deref()
        .or(run.assignment.parent_issue_id.as_deref())
}

pub fn prepare(root: &Path, run: &LocalRun) -> Result<PathBuf> {
    let path = directory(root, run);
    fs::create_dir_all(&path)?;
    let path = path.canonicalize()?;
    let Some(issue) = workspace_key(run) else {
        return Ok(path);
    };
    let manifest = path.join("mounts.json");
    let mut mounts: Vec<Mount> = if manifest.is_file() {
        serde_json::from_slice(&fs::read(&manifest)?)?
    } else {
        Vec::new()
    };
    fs::create_dir_all(path.join("repos"))?;
    for repo in &run.repos {
        let relative = PathBuf::from("repos").join(component(&repo.id));
        let target = path.join(&relative);
        let base = prepare_repository(repo, &target, issue)?;
        if !mounts.iter().any(|mount| mount.repo_id == repo.id) {
            mounts.push(Mount {
                repo_id: repo.id.clone(),
                path: relative,
                base,
            });
        }
    }
    let staged = path.join("mounts.tmp");
    fs::write(&staged, serde_json::to_vec_pretty(&mounts)?)?;
    fs::rename(staged, manifest)?;

    let mut instructions =
        String::from("# Tend workspace\n\nFollow each repository's own AGENTS.md.\n\n");
    for mount in mounts {
        instructions.push_str(&format!(
            "- Repository `{}` is at `{}` and started at `{}`.\n",
            mount.repo_id,
            mount.path.display(),
            mount.base,
        ));
    }
    fs::write(path.join("AGENTS.md"), instructions)?;
    Ok(path)
}

pub fn branches(run: &LocalRun, cwd: &Path) -> Vec<protocol::WorkBranch> {
    if run.assignment.parent_issue_id.is_none() {
        return Vec::new();
    }
    run.repos
        .iter()
        .filter_map(|repo| {
            let path = cwd.join("repos").join(component(&repo.id));
            let branch = git(&path, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok()?;
            let origin = git(&path, &["config", "--get", "remote.origin.url"]).ok()?;
            let repository = protocol::github_repository(&origin)?;
            let branch_remote_key = format!("branch.{branch}.pushRemote");
            let branch_remote = format!("branch.{branch}.remote");
            let preferred = git(&path, &["config", "--get", &branch_remote_key])
                .or_else(|_| git(&path, &["config", "--get", "remote.pushDefault"]))
                .or_else(|_| git(&path, &["config", "--get", &branch_remote]))
                .unwrap_or_else(|_| "origin".to_owned());
            let push = git(
                &path,
                &["config", "--get", &format!("remote.{preferred}.pushurl")],
            )
            .or_else(|_| {
                git(
                    &path,
                    &["config", "--get", &format!("remote.{preferred}.url")],
                )
            })
            .ok()?;
            let head_repository = protocol::github_repository(&push)?;
            Some(protocol::WorkBranch {
                repo_id: repo.id.clone(),
                repository,
                head_repository,
                branch,
            })
        })
        .collect()
}

fn prepare_repository(repo: &Repo, target: &Path, issue: &str) -> Result<String> {
    if target.exists() {
        let origin = git(
            &repo.path,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?;
        let existing = git(
            target,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?;
        ensure!(
            existing == origin && target.join(".git").is_file(),
            "workspace has a different Git registration"
        );
        return git(target, &["rev-parse", "HEAD"]);
    }
    git(&repo.path, &["fetch", "origin"])?;
    let branch = repo
        .default_branch
        .as_ref()
        .map(|branch| vec![branch.as_str()])
        .unwrap_or_else(|| vec!["main", "master", "develop"]);
    let base = branch
        .into_iter()
        .find_map(|branch| {
            git(
                &repo.path,
                &[
                    "rev-parse",
                    "--verify",
                    &format!("refs/remotes/origin/{branch}^{{commit}}"),
                ],
            )
            .ok()
        })
        .with_context(|| format!("resolve fetched default branch for {}", repo.id))?;
    let name = format!("tend/{issue}/{}", component(&repo.id));
    git(
        &repo.path,
        &[
            "worktree",
            "add",
            "-b",
            &name,
            target.to_str().context("workspace path must be UTF-8")?,
            &base,
        ],
    )?;
    Ok(base)
}

fn component(value: &str) -> String {
    let mut name = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_".contains(&byte) {
            name.push(byte as char);
        } else {
            name.push_str(&format!("%{byte:02x}"));
        }
    }
    name
}

fn git(cwd: &Path, arguments: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(cwd)
        .output()
        .context("run Git for the Tend workspace")?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            arguments[0],
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}
