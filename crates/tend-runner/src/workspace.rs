use crate::{config::Repo, store::run::LocalRun};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    os::unix::fs::MetadataExt,
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
    root.join(
        run.review_workspace
            .clone()
            .unwrap_or_else(|| run.assignment.workspace_path()),
    )
}

fn workspace_key(run: &LocalRun) -> Option<&str> {
    run.assignment
        .workspace_key
        .as_deref()
        .or(run.assignment.parent_issue_id.as_deref())
}

pub fn prepare(root: &Path, run: &LocalRun) -> Result<PathBuf> {
    ensure!(
        run.assignment.review.is_none(),
        "bound reviews cannot prepare a new worktree"
    );
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
        let base = prepare_repository(
            repo,
            &target,
            issue,
            run.assignment
                .work
                .base_refs
                .get(&repo.id)
                .map(String::as_str),
        )?;
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

pub fn stamp(path: &Path) -> Result<protocol::WorkspaceStamp> {
    let metadata = fs::metadata(path)?;
    ensure!(metadata.is_dir(), "workspace is not a directory");
    Ok(protocol::WorkspaceStamp {
        path: path.to_owned(),
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

pub fn existing_author_directory(root: &Path, run: &LocalRun) -> Result<PathBuf> {
    let relative = run
        .review_workspace
        .as_deref()
        .context("cannot verify the original author workspace on this Runner")?;
    let (namespace, key) = relative
        .split_once('/')
        .context("invalid author workspace binding")?;
    ensure!(
        matches!(namespace, "workspaces" | "tasks") && protocol::valid_id(key),
        "invalid author workspace binding"
    );
    let expected = root.canonicalize()?.join(relative);
    let path = expected
        .canonicalize()
        .context("original author workspace is missing")?;
    ensure!(
        path == expected && path.is_dir(),
        "original author workspace path changed"
    );
    let proof = run
        .author_workspace_stamp
        .as_ref()
        .context("original author workspace identity is not available")?;
    ensure!(
        &stamp(&path)? == proof,
        "original author workspace identity changed"
    );
    Ok(path)
}

pub fn verify_review(root: &Path, run: &LocalRun, repos: &[Repo]) -> Result<PathBuf> {
    let path = existing_author_directory(root, run)?;
    let review = run
        .assignment
        .review
        .as_ref()
        .context("missing bound review request")?;
    ensure!(
        protocol::valid_shared_review_target(&review.candidate, &run.assignment.work),
        "review candidate does not identify a local artifact or exact Git object"
    );
    if !run.assignment.work.base_refs.is_empty() {
        let mounts: Vec<Mount> = serde_json::from_slice(
            &fs::read(path.join("mounts.json"))
                .context("read original author workspace mount manifest")?,
        )?;
        for (id, sha) in &run.assignment.work.base_refs {
            let repo = repos
                .iter()
                .find(|repo| &repo.id == id)
                .context("original Git registration is unavailable")?;
            let relative = PathBuf::from("repos").join(component(id));
            ensure!(
                mounts
                    .iter()
                    .any(|mount| &mount.repo_id == id && mount.path == relative),
                "original author worktree for {id} is not mounted"
            );
            let target = path.join(relative);
            ensure!(
                target.canonicalize()? == target && target.join(".git").is_file(),
                "original author worktree path for {id} changed"
            );
            verify_registration(repo, &target)?;
            ensure!(
                Path::new(&git(&target, &["rev-parse", "--show-toplevel"])?) == target,
                "original Git worktree root for {id} changed"
            );
            let object = format!("{sha}^{{commit}}");
            let commit = git_command(
                &target,
                &["rev-parse", "--verify", "--end-of-options", &object],
                true,
            )
            .with_context(|| format!("bound commit for {id} is not available locally"))?;
            ensure!(
                sha.eq_ignore_ascii_case(&commit),
                "review reference for {id} did not resolve to the exact commit"
            );
        }
    }
    Ok(path)
}

pub fn branches(run: &LocalRun, cwd: &Path) -> Vec<protocol::WorkBranch> {
    if run.assignment.parent_issue_id.is_none() || run.assignment.review.is_some() {
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

fn prepare_repository(
    repo: &Repo,
    target: &Path,
    issue: &str,
    reference: Option<&str>,
) -> Result<String> {
    if target.exists() {
        verify_registration(repo, target)?;
        return git(target, &["rev-parse", "HEAD"]);
    }
    git(&repo.path, &["fetch", "origin"])?;
    let base = if let Some(reference) = reference {
        let fetched = format!("refs/remotes/origin/{reference}^{{commit}}");
        let explicit = format!("{reference}^{{commit}}");
        git(
            &repo.path,
            &["rev-parse", "--verify", "--end-of-options", &fetched],
        )
        .or_else(|_| {
            git(
                &repo.path,
                &["rev-parse", "--verify", "--end-of-options", &explicit],
            )
        })
        .with_context(|| format!("resolve requested base for {}", repo.id))?
    } else {
        let branches = repo
            .default_branch
            .as_deref()
            .map(|branch| vec![branch])
            .unwrap_or_else(|| vec!["main", "master", "develop"]);
        branches
            .into_iter()
            .find_map(|branch| {
                git(
                    &repo.path,
                    &[
                        "rev-parse",
                        "--verify",
                        "--end-of-options",
                        &format!("refs/remotes/origin/{branch}^{{commit}}"),
                    ],
                )
                .ok()
            })
            .with_context(|| format!("resolve fetched default branch for {}", repo.id))?
    };
    let target_path = target.to_str().context("workspace path must be UTF-8")?;
    let name = format!("tend/{issue}/{}", component(&repo.id));
    if git(
        &repo.path,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{name}"),
        ],
    )
    .is_ok()
    {
        git(&repo.path, &["worktree", "add", target_path, &name])?;
        return git(target, &["rev-parse", "HEAD"]);
    }
    git(
        &repo.path,
        &["worktree", "add", "-b", &name, target_path, &base],
    )?;
    Ok(base)
}

fn verify_registration(repo: &Repo, target: &Path) -> Result<()> {
    let args = ["rev-parse", "--path-format=absolute", "--git-common-dir"];
    ensure!(
        git(&repo.path, &args)? == git(target, &args)? && target.join(".git").is_file(),
        "workspace has a different Git registration"
    );
    Ok(())
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
    git_command(cwd, arguments, false)
}

fn git_command(cwd: &Path, arguments: &[&str], review: bool) -> Result<String> {
    let mut git = Command::new("git");
    if review {
        git.env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("GIT_NO_LAZY_FETCH", "1");
    }
    let output = git
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

pub fn serve_reclaims(
    store: crate::store::Store,
    config: std::sync::Arc<crate::config::Config>,
    root: PathBuf,
) {
    std::thread::spawn(move || {
        loop {
            let result = (|| {
                if let Some(request) = store.next_reclaim()? {
                    let result = request
                        .keys
                        .iter()
                        .try_for_each(|key| reclaim_one(&root, key, &config.repos));
                    store.save_reclaim(protocol::WorkspaceReclaimReceipt {
                        id: request.id,
                        error: result.err().map(|error| format!("{error:#}")),
                    })?;
                }
                Ok::<_, anyhow::Error>(())
            })();
            if let Err(error) = result {
                eprintln!("Workspace reclaim: {error:#}");
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    });
}

fn reclaim_one(root: &Path, key: &str, repos: &[Repo]) -> Result<()> {
    ensure!(protocol::valid_id(key), "invalid workspace key");
    let workspaces = root.join("workspaces");
    let directory = workspaces.join(key);
    if !directory.try_exists()? {
        return Ok(());
    }
    let path = directory.canonicalize()?;
    ensure!(
        path == workspaces.canonicalize()?.join(key),
        "workspace path changed"
    );
    let manifest = path.join("mounts.json");
    if !manifest.try_exists()? && fs::read_dir(&path)?.next().is_none() {
        fs::remove_dir(path)?;
        return Ok(());
    }
    let mounts: Vec<Mount> =
        serde_json::from_slice(&fs::read(&manifest).context("read workspace ownership manifest")?)?;
    for entry in fs::read_dir(&path)? {
        let entry = entry?;
        ensure!(
            matches!(
                entry.file_name().to_str(),
                Some("mounts.json" | "repos" | "AGENTS.md")
            ),
            "workspace contains additional files"
        );
    }
    let mounts_root = path.join("repos");
    let mut targets = Vec::new();
    for mount in &mounts {
        let repo = repos
            .iter()
            .find(|repo| repo.id == mount.repo_id)
            .context("workspace repository registration is unavailable")?;
        let target = path.join(&mount.path);
        ensure!(
            mount.path == PathBuf::from("repos").join(component(&repo.id)),
            "workspace mount path changed"
        );
        if !target.try_exists()? {
            continue;
        }
        ensure!(
            target.canonicalize()? == target && target.join(".git").is_file(),
            "workspace mount identity changed"
        );
        let expected = git(
            &repo.path,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?;
        let actual = git(
            &target,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?;
        ensure!(
            expected == actual,
            "workspace mount belongs to a different repository"
        );
        ensure!(
            git(&target, &["status", "--porcelain"])?.is_empty(),
            "workspace has uncommitted changes in {}",
            repo.id
        );
        targets.push((repo, target));
    }
    if mounts_root.try_exists()? {
        for entry in fs::read_dir(&mounts_root)? {
            let entry = entry?.path();
            ensure!(
                targets.iter().any(|(_, target)| target == &entry),
                "workspace contains an unregistered mount"
            );
        }
    }
    for (repo, target) in targets {
        let target = target.to_str().context("workspace path must be UTF-8")?;
        git(&repo.path, &["worktree", "remove", target])
            .context("remove clean managed Git worktree")?;
    }
    if mounts_root.try_exists()? {
        fs::remove_dir(mounts_root)?;
    }
    let instructions = path.join("AGENTS.md");
    if instructions.try_exists()? {
        fs::remove_file(instructions)?;
    }
    fs::remove_file(manifest)?;
    fs::remove_dir(path)?;
    Ok(())
}
