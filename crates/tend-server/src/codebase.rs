use protocol::{PullFacts, PullProvider, PullRef, WorkBranch, codebase_repository};
use serde_json::Value;
use std::{
    collections::HashSet,
    ffi::OsStr,
    io::{self, Read},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(10);
const MAX_OUTPUT: u64 = 1024 * 1024;
const PAGE_SIZE: usize = 2;
const MAX_PAGES: usize = 10;

type Query<T> = Result<T, &'static str>;

pub fn observe(program: &OsStr, pull: &PullRef) -> Query<PullFacts> {
    let number = pull.number.to_string();
    let response = request(program, &["mr", "get", &number, "-R", &pull.repository])?;
    let mr = &response["merge_request"];
    if response["repository"]["path"] != pull.repository || mr["Number"] != pull.number {
        return Err("invalid_response");
    }
    let state = match mr["Status"].as_str() {
        Some("open") => "open",
        Some("merged") => "merged",
        Some("closed") => "closed",
        _ => return Err("invalid_response"),
    };
    let mut facts = PullFacts {
        title: mr["Title"].as_str().ok_or("invalid_response")?.to_owned(),
        state: state.to_owned(),
        draft: mr["Draft"].as_bool().ok_or("invalid_response")?,
        head_sha: nonempty(&response["version"]["SourceCommitId"])
            .ok_or("invalid_response")?
            .to_owned(),
        head_branch: nonempty(&mr["SourceBranchName"]).map(str::to_owned),
        head_repository: source_repository(mr, &response["repository"]).map(str::to_owned),
        merge_commit_sha: nonempty(&mr["MergeCommitId"])
            .filter(|_| state == "merged")
            .map(str::to_owned),
        ci: None,
    };

    let checks = request(
        program,
        &[
            "checks",
            "mr",
            &number,
            "-R",
            &pull.repository,
            "--commit",
            &facts.head_sha,
            "--no-step-logs",
        ],
    )?;
    facts.ci = match ci_summary(&checks, pull, &response, &facts.head_sha) {
        Some(CiSummary::Failure) => Some("failure"),
        Some(CiSummary::Pending) => Some("pending"),
        Some(CiSummary::NeedsSuccessEvidence(count)) => {
            // The summary treats missing conclusions like non-failures. Only
            // read per-check evidence when we might otherwise report success.
            let list = request(
                program,
                &[
                    "checks",
                    "list",
                    "-R",
                    &pull.repository,
                    "--commit",
                    &facts.head_sha,
                    "--mr",
                    &number,
                ],
            )?;
            confirms_ci_success(&list, pull, &response, &facts.head_sha, count).then_some("success")
        }
        None => None,
    }
    .map(str::to_owned);
    Ok(facts)
}

#[derive(Debug, PartialEq, Eq)]
enum CiSummary {
    Failure,
    Pending,
    NeedsSuccessEvidence(u64),
}

fn matches_repository(observed: &Value, detail: &Value, pull: &PullRef) -> bool {
    let id = &detail["repository"]["id"];
    // bytedcli can resolve -R to an ID while omitting the path in the reply.
    (nonempty(id).is_some() || id.is_u64())
        && observed["id"] == *id
        && observed
            .get("path")
            .is_none_or(|path| path.is_null() || *path == pull.repository)
}

fn ci_summary(checks: &Value, pull: &PullRef, detail: &Value, head_sha: &str) -> Option<CiSummary> {
    let mr = &checks["merge_request"];
    if !matches_repository(&checks["repository"], detail, pull)
        || mr["Number"] != pull.number
        || nonempty(&detail["merge_request"]["Id"]).is_some_and(|id| mr["Id"] != id)
    {
        return None;
    }
    // The query is pinned to head_sha. Also check that the MR snapshot in the
    // reply still reports that head, rather than showing green after a push.
    let reported_head = match mr["Versions"].as_array() {
        Some(versions) if !versions.is_empty() => versions
            .iter()
            .max_by_key(|version| version["Number"].as_u64())?
            .get("SourceCommitId")?,
        _ => &mr["SourceBranch"]["Commit"]["Id"],
    };
    if *reported_head != head_sha {
        return None;
    }
    let checked = checks["check_count"].as_u64()?;
    let total = checks["total_count"].as_u64()?;
    let failed = checks["failed_count"].as_u64()?;
    let running = checks["running_count"].as_u64()?;
    // The CLI can stop paging early. Counters cover only rows it read.
    if total == 0 || checked != total || failed.checked_add(running)? > checked {
        None
    } else if failed > 0 {
        Some(CiSummary::Failure)
    } else if running > 0 {
        Some(CiSummary::Pending)
    } else {
        Some(CiSummary::NeedsSuccessEvidence(checked))
    }
}

fn confirms_ci_success(
    list: &Value,
    pull: &PullRef,
    detail: &Value,
    head_sha: &str,
    expected: u64,
) -> bool {
    let Some(mr_id) = nonempty(&detail["merge_request"]["Id"]) else {
        return false;
    };
    let mr = &list["merge_request"];
    let scoped = &list["mr_checks"];
    if !matches_repository(&list["repository"], detail, pull)
        || list["ref"]["commit"] != head_sha
        || mr["number"] != pull.number
        || mr["id"] != mr_id
        || mr["commit"] != head_sha
        || expected == 0
        || scoped["check_count"] != expected
        || scoped["total_count"] != expected
        || scoped["failed_count"] != 0
    {
        return false;
    }
    // checks list also returns branch-scoped rows. Never use them for the MR.
    let Some(rows) = scoped["check_runs"].as_array() else {
        return false;
    };
    if rows.len() as u64 != expected {
        return false;
    }
    let mut seen = HashSet::new();
    let mut passed = false;
    for row in rows {
        let Some(id) = nonempty(&row["Id"]) else {
            return false;
        };
        if !seen.insert(id)
            || row["RepoId"] != detail["repository"]["id"]
            || row["MergeRequestId"] != mr_id
            || row["CommitId"] != head_sha
            || !is_status(&row["Status"], &["completed"])
        {
            return false;
        }
        if is_status(&row["Conclusion"], &["success", "succeeded", "passed"]) {
            passed = true;
        } else if !is_status(&row["Conclusion"], &["neutral", "skipped"]) {
            return false;
        }
    }
    passed
}

fn is_status(value: &Value, statuses: &[&str]) -> bool {
    value.as_str().is_some_and(|value| {
        statuses
            .iter()
            .any(|status| value.eq_ignore_ascii_case(status))
    })
}

pub fn find(program: &OsStr, branch: &WorkBranch) -> Query<Option<PullRef>> {
    if !codebase_repository(&branch.repository)
        || !codebase_repository(&branch.head_repository)
        || branch.branch.is_empty()
        || branch.branch.starts_with('-')
    {
        return Err("invalid_reference");
    }
    let mut found = None;
    let mut unreadable_source = false;
    let mut first_total = None;
    let mut seen = HashSet::new();
    for page in 1..=MAX_PAGES {
        let mut args = vec![
            "mr",
            "list",
            "-R",
            &branch.repository,
            "-H",
            &branch.branch,
            "--state",
            "all",
            "--page-size",
            "2",
        ];
        let page_arg = page.to_string();
        if page != 1 {
            args.extend(["--page", &page_arg]);
        }
        let response = request(program, &args)?;
        let repo = &response["repository"];
        if repo["path"] != branch.repository {
            return Err("invalid_response");
        }
        let candidates = response["merge_requests"]
            .as_array()
            .ok_or("invalid_response")?;
        let total = response["total_count"].as_u64().ok_or("invalid_response")?;
        if *first_total.get_or_insert(total) != total {
            return Err("lookup_incomplete");
        }
        if candidates.len() as u64
            != total
                .saturating_sub(((page - 1) * PAGE_SIZE) as u64)
                .min(PAGE_SIZE as u64)
        {
            return Err("invalid_response");
        }
        for mr in candidates {
            let Some(candidate_branch) = nonempty(&mr["SourceBranchName"]) else {
                unreadable_source = true;
                continue;
            };
            let number = mr["Number"]
                .as_i64()
                .filter(|number| *number > 0)
                .ok_or("invalid_response")?;
            if !seen.insert(number) {
                return Err("lookup_incomplete");
            }
            if candidate_branch != branch.branch {
                continue;
            }
            let Some(source) = source_repository(mr, repo) else {
                unreadable_source = true;
                continue;
            };
            if source != branch.head_repository {
                continue;
            }
            let pull = PullRef {
                provider: PullProvider::Codebase,
                repository: branch.repository.clone(),
                number,
            };
            if let Some(url) = nonempty(&mr["URL"])
                && PullRef::parse(url).as_ref() != Some(&pull)
            {
                return Err("invalid_response");
            }
            if found.replace(pull).is_some() {
                return Err("ambiguous");
            }
        }
        if total <= (page * PAGE_SIZE) as u64 {
            return if unreadable_source {
                Err("invalid_response")
            } else {
                Ok(found)
            };
        }
    }
    Err("lookup_incomplete")
}

fn source_repository<'a>(mr: &'a Value, target: &'a Value) -> Option<&'a str> {
    mr["SourceRepository"]["Path"]
        .as_str()
        .or_else(|| mr["SourceRepository"]["path"].as_str())
        .or_else(|| {
            // Many responses omit the expanded source repository for same-repo MRs.
            let id = &target["id"];
            let known = nonempty(id).is_some() || id.is_u64();
            (known && &mr["SourceRepoId"] == id).then(|| target["path"].as_str())?
        })
        .filter(|path| codebase_repository(path))
}

fn nonempty(value: &Value) -> Option<&str> {
    value.as_str().filter(|text| !text.is_empty())
}

fn request(program: &OsStr, args: &[&str]) -> Query<Value> {
    let deadline = Instant::now() + TIMEOUT;
    let mut child = Command::new(program)
        .args(["--json", "codebase"])
        .args(args)
        .env_remove(protocol::TOKEN_ENV)
        .env("BYTEDCLI_NO_AUTO_UPGRADE", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| "codebase_cli_unavailable")?;
    let stdout = read_pipe(child.stdout.take().expect("piped stdout"));
    let stderr = read_pipe(child.stderr.take().expect("piped stderr"));
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
            other => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(if other.is_err() {
                    "codebase_query_failed"
                } else {
                    "codebase_query_timeout"
                });
            }
        }
    };
    let receive = |receiver: Receiver<io::Result<Vec<u8>>>| {
        let output = receiver
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| "codebase_query_timeout")?
            .map_err(|_| "invalid_response")?;
        if output.len() as u64 > MAX_OUTPUT {
            return Err("invalid_response");
        }
        Ok(output)
    };
    let output = receive(stdout)?;
    let diagnostics = receive(stderr)?;
    let payload = serde_json::from_slice::<Value>(&output).ok();
    if payload.as_ref().is_some_and(access_error)
        || (!status.success()
            && serde_json::from_slice::<Value>(&diagnostics)
                .as_ref()
                .is_ok_and(access_error))
    {
        return Err("not_accessible");
    }
    if !status.success() {
        return Err("codebase_query_failed");
    }
    match payload {
        Some(mut value) if value["status"] == "success" && value["data"].is_object() => {
            Ok(value["data"].take())
        }
        Some(value) if value["status"] == "error" || value.get("event").is_some() => {
            Err("codebase_query_failed")
        }
        _ => Err("invalid_response"),
    }
}

fn read_pipe(pipe: impl Read + Send + 'static) -> Receiver<io::Result<Vec<u8>>> {
    let (send, receive) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = pipe
            .take(MAX_OUTPUT + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes);
        let _ = send.send(result);
    });
    receive
}

fn access_error(response: &Value) -> bool {
    let error = &response["error"];
    [error.get("status"), error.get("status_code")]
        .into_iter()
        .flatten()
        .any(|code| matches!(code.as_i64(), Some(401 | 403 | 404)))
        || [
            error.get("code"),
            error.get("type"),
            response["data"].get("code"),
        ]
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .any(|code| {
            let code = code.to_ascii_lowercase().replace(['_', '-', '.'], "");
            [
                "notfound",
                "notaccessible",
                "unauth",
                "authrequired",
                "authentication",
                "invalidtoken",
                "forbidden",
                "permission",
                "accessdenied",
                "login",
            ]
            .iter()
            .any(|part| code.contains(part))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn codebase_ci_distinguishes_unknown_pending_and_failure() {
        let pull = PullRef::parse("https://code.byted.org/example/guide/merge_requests/7").unwrap();
        let detail = json!({"repository": {"id": "201"}, "merge_request": {"Id": "801"}});
        for (checked, total, failed, running, expected) in [
            (0, 0, 0, 0, None),
            (2, 2, 0, 0, Some(CiSummary::NeedsSuccessEvidence(2))),
            (2, 2, 0, 1, Some(CiSummary::Pending)),
            (2, 2, 1, 1, Some(CiSummary::Failure)),
            (2, 2, 1, 2, None),
            (2, 2, -1, 0, None),
            (2, 3, 0, 0, None),
        ] {
            let checks = json!({
                "repository": {"id": "201", "path": null},
                "merge_request": {"Id": "801", "Number": 7, "Versions": [
                    {"Number": 2, "SourceCommitId": "head"},
                    {"Number": 1, "SourceCommitId": "older"}
                ]},
                "check_count": checked, "total_count": total,
                "failed_count": failed, "running_count": running,
            });
            assert_eq!(
                ci_summary(&checks, &pull, &detail, "head"),
                expected,
                "{checks}"
            );
        }
    }

    #[test]
    fn codebase_ci_requires_matching_repository_request_and_head() {
        let pull = PullRef::parse("https://code.byted.org/example/guide/merge_requests/7").unwrap();
        let detail = json!({"repository": {"id": "201"}, "merge_request": {"Id": "801"}});
        let checks = json!({
            "repository": {"id": "201", "path": null},
            "merge_request": {"Id": "801", "Number": 7, "SourceBranch": {"Commit": {"Id": "head"}}},
            "check_count": 2, "total_count": 2, "failed_count": 0, "running_count": 0,
        });
        assert_eq!(
            ci_summary(&checks, &pull, &detail, "head"),
            Some(CiSummary::NeedsSuccessEvidence(2))
        );
        assert_eq!(ci_summary(&checks, &pull, &detail, "stale"), None);
        assert_eq!(ci_summary(&checks, &pull, &Value::Null, "head"), None);
        for (path, replacement) in [
            ("/repository/id", json!("202")),
            ("/repository/id", Value::Null),
            ("/repository/path", json!("other/guide")),
            ("/merge_request/Number", json!(8)),
            ("/merge_request/Id", json!("802")),
            ("/merge_request/SourceBranch", Value::Null),
            ("/check_count", Value::Null),
            ("/total_count", Value::Null),
        ] {
            let mut bad = checks.clone();
            *bad.pointer_mut(path).unwrap() = replacement;
            assert_eq!(ci_summary(&bad, &pull, &detail, "head"), None, "{path}");
        }
    }

    #[test]
    fn codebase_ci_success_requires_complete_mr_scoped_terminal_conclusions() {
        let pull = PullRef::parse("https://code.byted.org/example/guide/merge_requests/7").unwrap();
        let detail = json!({"repository": {"id": "201"}, "merge_request": {"Id": "801"}});
        let row = json!({
            "Id": "check1", "RepoId": "201", "MergeRequestId": "801", "CommitId": "head",
            "Status": "completed", "Conclusion": "succeeded",
        });
        let mut neutral = row.clone();
        neutral["Id"] = json!("check2");
        neutral["Conclusion"] = json!("neutral");
        let list = json!({
            "repository": {"id": "201", "path": "example/guide"},
            "ref": {"commit": "head"},
            "merge_request": {"id": "801", "number": 7, "commit": "head"},
            "check_runs": [row],
            "mr_checks": {"check_count": 2, "total_count": 2, "failed_count": 0, "check_runs": [row, neutral]},
        });
        assert!(confirms_ci_success(&list, &pull, &detail, "head", 2));
        assert!(!confirms_ci_success(&list, &pull, &detail, "head", 1));
        for (path, replacement) in [
            ("/repository/id", json!("202")),
            ("/ref/commit", json!("older")),
            ("/merge_request/commit", json!("older")),
            ("/merge_request/id", json!("800")),
            ("/merge_request/number", json!(8)),
            ("/mr_checks", Value::Null),
            ("/mr_checks/failed_count", json!(1)),
            ("/mr_checks/total_count", json!(3)),
            ("/mr_checks/check_runs", json!([row])),
            ("/mr_checks/check_runs/1/Id", json!("check1")),
            ("/mr_checks/check_runs/1/RepoId", json!("202")),
            ("/mr_checks/check_runs/1/MergeRequestId", json!("800")),
            ("/mr_checks/check_runs/1/CommitId", json!("older")),
            ("/mr_checks/check_runs/1/Status", json!("in_progress")),
            ("/mr_checks/check_runs/1/Status", Value::Null),
            ("/mr_checks/check_runs/1/Conclusion", Value::Null),
            ("/mr_checks/check_runs/0/Conclusion", json!("skipped")),
        ] {
            let mut bad = list.clone();
            *bad.pointer_mut(path).unwrap() = replacement;
            assert!(
                !confirms_ci_success(&bad, &pull, &detail, "head", 2),
                "{path}"
            );
        }
    }

    #[test]
    fn access_errors_use_only_structured_codes() {
        for error in [
            json!({"error": {"code": "NotFound.Repository"}}),
            json!({"error": {"status": 403}}),
            json!({"event": "action_required", "data": {"code": "LOGIN_REQUIRED"}}),
        ] {
            assert!(access_error(&error), "{error}");
        }
        assert!(!access_error(
            &json!({"error": {"message": "forbidden", "code": "NetworkError"}})
        ));
    }
}
