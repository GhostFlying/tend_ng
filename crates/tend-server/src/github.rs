use crate::store::Store;
use protocol::{BranchResult, ExternalFact, ExternalKind, GitHubPull, PullFacts, WorkBranch};
use serde_json::Value;
use std::{
    ffi::OsString,
    io::Read,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const QUERY: &str = "query($owner:String!, $repo:String!, $number:Int!) {
  repository(owner:$owner, name:$repo) {
    pullRequest(number:$number) {
      title state isDraft headRefOid headRefName headRepository { nameWithOwner }
      commits(last:1) { nodes { commit { oid statusCheckRollup {
        state contexts(first:100) { nodes {
          ... on CheckRun { databaseId name status conclusion }
          ... on StatusContext { context state }
        } }
      } } } }
    }
  }
}";

const CHECKS_AT_COMMIT: &str = "query($owner:String!, $repo:String!, $sha:GitObjectID!) {
  repository(owner:$owner, name:$repo) { object(oid:$sha) { ... on Commit {
    oid statusCheckRollup { state contexts(first:100) { nodes {
      ... on CheckRun { databaseId name status conclusion }
      ... on StatusContext { context state }
    } } }
  } } }
}";

pub fn spawn(store: Store, program: OsString) {
    thread::spawn(move || {
        loop {
            let result = (|| {
                if let Some(pull) = store.due_pull()? {
                    let watched = store.watched_ci_heads(&pull)?;
                    store.observe_pull(&pull, observe(&program, &pull, &watched))?;
                    return Ok(true);
                }
                Ok::<_, anyhow::Error>(false)
            })();
            match result {
                Ok(true) => continue,
                Ok(false) => {}
                Err(error) => eprintln!("GitHub observation: {error:#}"),
            }
            thread::sleep(Duration::from_secs(1));
        }
    });
}

pub fn discussion_pull(program: &OsString, pull: &GitHubPull) -> Result<Value, &'static str> {
    let output = request(
        program,
        &[
            "api",
            "--method",
            "GET",
            "--hostname",
            "github.com",
            &format!("repos/{}/pulls/{}", pull.repository, pull.number),
        ],
    )?;
    if output["number"] != pull.number || !output["title"].is_string() {
        return Err("invalid_response");
    }
    Ok(serde_json::json!({
        "title": output["title"], "state": output["state"], "merged": output["merged"],
        "draft": output["draft"], "head_sha": output["head"]["sha"],
        "body": output["body"].as_str().map(|body| body.chars().take(12000).collect::<String>()),
    }))
}

fn observe(
    program: &OsString,
    pull: &GitHubPull,
    watched: &[String],
) -> Result<(PullFacts, Vec<ExternalFact>), &'static str> {
    let (owner, repo) = pull.repository.split_once('/').ok_or("invalid_reference")?;
    let output = request(
        program,
        &[
            "api",
            "graphql",
            "--hostname",
            "github.com",
            "-f",
            &format!("query={QUERY}"),
            "-f",
            &format!("owner={owner}"),
            "-f",
            &format!("repo={repo}"),
            "-F",
            &format!("number={}", pull.number),
        ],
    )?;
    let pr = &output["data"]["repository"]["pullRequest"];
    if pr.is_null() {
        return Err("not_accessible");
    }
    let title = pr["title"].as_str().ok_or("invalid_response")?.to_owned();
    let state = match pr["state"].as_str() {
        Some("OPEN") => "open",
        Some("CLOSED") => "closed",
        Some("MERGED") => "merged",
        _ => return Err("invalid_response"),
    }
    .to_owned();
    let draft = pr["isDraft"].as_bool().ok_or("invalid_response")?;
    let head_sha = pr["headRefOid"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or("invalid_response")?
        .to_owned();
    let commit = &pr["commits"]["nodes"][0]["commit"];
    let ci = if commit["oid"] == head_sha {
        ci_summary(&commit["statusCheckRollup"])
    } else {
        None
    };
    let mut check_facts = if commit["oid"] == head_sha {
        checks(
            pull,
            &head_sha,
            &commit["statusCheckRollup"]["contexts"]["nodes"],
        )
    } else {
        Vec::new()
    };
    let head_repository = pr["headRepository"]["nameWithOwner"]
        .as_str()
        .map(str::to_ascii_lowercase);
    let check_repository = head_repository.as_deref().unwrap_or(&pull.repository);
    for sha in watched.iter().filter(|sha| **sha != head_sha) {
        if let Ok(facts) = candidate_checks(program, check_repository, pull, sha) {
            check_facts.extend(facts);
        }
    }
    Ok((
        PullFacts {
            title,
            state,
            draft,
            head_sha,
            ci: ci.map(str::to_owned),
            head_branch: pr["headRefName"].as_str().map(str::to_owned),
            head_repository,
        },
        check_facts,
    ))
}

fn ci_summary(rollup: &Value) -> Option<&'static str> {
    match rollup["state"].as_str() {
        Some("SUCCESS") => Some("success"),
        Some("FAILURE" | "ERROR") => Some("failure"),
        Some("PENDING" | "EXPECTED") => Some("pending"),
        _ => None,
    }
}

fn candidate_checks(
    program: &OsString,
    repository: &str,
    pull: &GitHubPull,
    sha: &str,
) -> Result<Vec<ExternalFact>, &'static str> {
    let (owner, repo) = repository.split_once('/').ok_or("invalid_reference")?;
    let output = request(
        program,
        &[
            "api",
            "graphql",
            "--hostname",
            "github.com",
            "-f",
            &format!("query={CHECKS_AT_COMMIT}"),
            "-f",
            &format!("owner={owner}"),
            "-f",
            &format!("repo={repo}"),
            "-f",
            &format!("sha={sha}"),
        ],
    )?;
    let commit = &output["data"]["repository"]["object"];
    if commit["oid"] != sha {
        return Ok(Vec::new());
    }
    let rollup = &commit["statusCheckRollup"];
    let mut facts = checks(pull, sha, &rollup["contexts"]["nodes"]);
    if let Some(state) = ci_summary(rollup) {
        facts.push(ExternalFact {
            url: pull.url(),
            event: ExternalKind::Ci,
            state: state.to_owned(),
            head_sha: Some(sha.to_owned()),
            check: None,
            run_id: None,
        });
    }
    Ok(facts)
}

fn checks(pull: &GitHubPull, head: &str, nodes: &Value) -> Vec<ExternalFact> {
    nodes
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let check = item["name"].as_str().or_else(|| item["context"].as_str())?;
            let state = match (
                item["status"].as_str(),
                item["conclusion"].as_str(),
                item["state"].as_str(),
            ) {
                (Some("COMPLETED"), Some("SUCCESS"), _) | (_, _, Some("SUCCESS")) => "success",
                (_, Some("CANCELLED"), _) => "cancelled",
                (_, Some("SKIPPED" | "NEUTRAL"), _) => "skipped",
                (
                    _,
                    Some("FAILURE" | "TIMED_OUT" | "ACTION_REQUIRED" | "STARTUP_FAILURE" | "STALE"),
                    _,
                )
                | (_, _, Some("ERROR" | "FAILURE")) => "failure",
                (Some("QUEUED" | "IN_PROGRESS" | "PENDING" | "WAITING" | "REQUESTED"), _, _)
                | (_, _, Some("PENDING" | "EXPECTED")) => "pending",
                _ => return None,
            };
            let run_id = item["databaseId"].as_i64().map(|id| id.to_string());
            Some(ExternalFact {
                url: pull.url(),
                event: ExternalKind::Ci,
                state: state.into(),
                head_sha: Some(head.to_owned()),
                check: Some(check.to_owned()),
                run_id,
            })
        })
        .collect()
}

pub fn spawn_discovery(mut store: Store, program: OsString) {
    thread::spawn(move || {
        loop {
            let outcome = (|| {
                let Some(request) = store.pending_discovery()? else {
                    return Ok(false);
                };
                let results: Vec<_> = request
                    .branches
                    .iter()
                    .map(|branch| {
                        let (state, url) = match find(&program, branch) {
                            Ok(Some(pull)) => ("linked", Some(pull.url())),
                            Ok(None) => ("missing", None),
                            Err(reason) => (reason, None),
                        };
                        BranchResult {
                            branch: branch.clone(),
                            state: state.to_owned(),
                            url,
                        }
                    })
                    .collect();
                store.finish_discovery(&request, &results)?;
                Ok::<_, anyhow::Error>(true)
            })();
            match outcome {
                Ok(true) => continue,
                Ok(false) => {}
                Err(error) => eprintln!("GitHub branch discovery: {error:#}"),
            }
            thread::sleep(Duration::from_secs(1));
        }
    });
}

fn find(program: &OsString, branch: &WorkBranch) -> Result<Option<GitHubPull>, &'static str> {
    let (owner, _) = branch
        .head_repository
        .split_once('/')
        .ok_or("invalid_reference")?;
    let response = request(
        program,
        &[
            "api",
            "--hostname",
            "github.com",
            "--method",
            "GET",
            &format!("repos/{}/pulls", branch.repository),
            "-f",
            "state=all",
            "-f",
            &format!("head={owner}:{}", branch.branch),
            "-F",
            "per_page=2",
        ],
    )?;
    let candidates = response.as_array().ok_or("invalid_response")?;
    match candidates.as_slice() {
        [] => Ok(None),
        [pr] if pr["head"]["ref"] == branch.branch
            && pr["head"]["repo"]["full_name"]
                .as_str()
                .is_some_and(|repo| repo.eq_ignore_ascii_case(&branch.head_repository)) =>
        {
            let number = pr["number"]
                .as_i64()
                .filter(|number| *number > 0)
                .ok_or("invalid_response")?;
            Ok(Some(GitHubPull {
                repository: branch.repository.clone(),
                number,
            }))
        }
        [_] => Ok(None),
        _ => Err("ambiguous"),
    }
}

fn request(program: &OsString, args: &[&str]) -> Result<Value, &'static str> {
    let mut child = Command::new(program)
        .args(args)
        .env("GH_PROMPT_DISABLED", "1")
        .env_remove(protocol::TOKEN_ENV)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|_| "github_cli_unavailable")?;
    let mut stdout = child
        .stdout
        .take()
        .expect("GitHub CLI stdout pipe")
        .take(128 * 1024);
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("github_query_timeout");
            }
        }
    };
    let bytes = reader
        .join()
        .map_err(|_| "invalid_response")?
        .map_err(|_| "invalid_response")?;
    let output: Value = serde_json::from_slice(&bytes).map_err(|_| {
        if status.success() {
            "invalid_response"
        } else {
            "github_query_failed"
        }
    })?;
    if let Some(errors) = output["errors"]
        .as_array()
        .filter(|errors| !errors.is_empty())
    {
        if errors.iter().any(|error| {
            matches!(
                error["type"].as_str(),
                Some("FORBIDDEN" | "UNAUTHORIZED" | "NOT_FOUND")
            )
        }) {
            return Err("not_accessible");
        }
        return Err("github_query_failed");
    }
    if !status.success() {
        let code = output["status"]
            .as_str()
            .and_then(|value| value.parse::<i64>().ok())
            .or_else(|| output["status"].as_i64());
        if matches!(code, Some(401 | 403 | 404))
            || output["message"] == "Resource not accessible by integration"
            || output["message"] == "Resource not accessible by personal access token"
        {
            return Err("not_accessible");
        }
        return Err("github_query_failed");
    }
    Ok(output)
}
