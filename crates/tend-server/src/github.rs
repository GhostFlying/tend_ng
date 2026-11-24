use crate::store::Store;
use protocol::{GitHubPull, PullFacts};
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
      title state isDraft headRefOid
      commits(last:1) { nodes { commit { oid statusCheckRollup { state } } } }
    }
  }
}";

pub fn spawn(store: Store, program: OsString) {
    thread::spawn(move || {
        loop {
            let result = (|| {
                if let Some(pull) = store.due_pull()? {
                    store.observe_pull(&pull, observe(&program, &pull))?;
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

fn observe(program: &OsString, pull: &GitHubPull) -> Result<PullFacts, &'static str> {
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
        match commit["statusCheckRollup"]["state"].as_str() {
            Some("SUCCESS") => Some("success"),
            Some("FAILURE" | "ERROR") => Some("failure"),
            Some("PENDING" | "EXPECTED") => Some("pending"),
            _ => None,
        }
    } else {
        None
    };
    Ok(PullFacts {
        title,
        state,
        draft,
        head_sha,
        ci: ci.map(str::to_owned),
    })
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
        return Err("github_query_failed");
    }
    Ok(output)
}
