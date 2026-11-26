#!/usr/bin/env python3
"""Only the allowlisted, read-only bytedcli calls against a synthetic repository."""

import json
import os
from pathlib import Path
import sys
import time

assert "TEND_TOKEN" not in os.environ
assert os.environ["BYTEDCLI_NO_AUTO_UPGRADE"] == "1"
root = Path(os.environ["TEND_CODEBASE_FIXTURE_DIR"])
repo = {"path": "example/guide", "id": "201"}
assert sys.argv[1:3] == ["--json", "codebase"]
args = sys.argv[3:]


def log(*fields):
    with (root / "codebase-reads").open("a") as output:
        output.write(json.dumps(fields) + "\n")


def denied(code):
    # The body is not safe for logging or display by the provider adapter.
    print(json.dumps({"status": "error", "error": {"code": code, "message": "private provider diagnostic"}}))
    sys.exit(1)


def mr(number, branch, source="201"):
    state = {73: "merged", 74: "closed", 76: "closed"}.get(number, "open")
    return {
        "Id": f"801{number}", "Number": number, "Title": f"Synthetic merge request {number}", "Status": state,
        "Draft": number == 204, "SourceBranchName": branch, "SourceRepoId": source,
        "TargetRepoId": repo["id"], "MergeCommitId": "b" * 40 if state == "merged" else None,
        "URL": f"https://code.byted.org/{repo['path']}/merge_requests/{number}",
    }


if args[:2] == ["mr", "list"]:
    assert len(args) in (10, 12) and args[2:5] == ["-R", repo["path"], "-H"]
    assert args[6:10] == ["--state", "all", "--page-size", "2"]
    branch = args[5]
    assert branch.startswith("tend/") and branch.endswith("/example")
    page = 1
    if len(args) == 12:
        assert args[10] == "--page"
        page = int(args[11])
    mode = (root / "codebase-discovery-mode").read_text()
    log("list", mode, branch, page)
    if mode == "denied":
        denied("NotFound.Repository")
    same = {**mr(204, branch), "URL": ""}
    foreign = {**mr(206, branch, source="902"), "SourceRepository": {"Path": "fork/guide"}}
    candidates = {
        "missing": [foreign],
        "unique": [same, foreign],
        "later": [foreign, {**foreign, "Number": 207}, same],
        "ambiguous": [same, mr(205, branch)],
        "source-unknown": [same, {**mr(205, branch), "SourceRepoId": None}],
    }[mode]
    data = {"repository": repo, "total_count": len(candidates),
            "merge_requests": candidates[(page - 1) * 2:page * 2]}
elif args[:2] == ["checks", "list"]:
    assert args[2:] == ["-R", repo["path"], "--commit", "a" * 40, "--mr", "73"]
    log("checks", "list", 73)
    row = {"Id": "run-1", "RepoId": repo["id"], "MergeRequestId": "80173",
           "CommitId": "a" * 40, "Status": "completed", "Conclusion": "succeeded"}
    rows = [row, {**row, "Id": "run-2"}]
    if (root / "missing-conclusion-73").exists():
        rows = [{**check, "Conclusion": None} for check in rows]
    data = {
        "repository": repo, "ref": {"commit": "a" * 40},
        "merge_request": {"id": "80173", "number": 73, "commit": "a" * 40},
        "check_runs": [{**row, "MergeRequestId": None}],
        "mr_checks": {"check_count": 2, "total_count": 2, "failed_count": 0, "check_runs": rows},
    }
else:
    assert args[:2] in (["mr", "get"], ["checks", "mr"]) and args[3:5] == ["-R", repo["path"]]
    number = int(args[2])
    assert number in (73, 74, 76, 99, 204, 206)
    log(*args[:2], number)
    if args[:2] == ["mr", "get"]:
        assert not args[5:]
        if number == 99 or (root / f"deny-mr-{number}").exists():
            denied("AuthRequired")
        branch = (root / "recorded-branch").read_text() if number in (204, 206) else "fixture/guide"
        merge_request = mr(number, branch)
        if number == 206:
            merge_request.update(SourceRepoId="902", SourceRepository={"Path": "fork/guide"})
        data = {"repository": repo, "merge_request": merge_request,
                "version": {"Number": 2, "SourceCommitId": "a" * 40}}
    else:
        assert args[5:] == ["--commit", "a" * 40, "--no-step-logs"]
        if (root / f"timeout-checks-{number}").exists():
            time.sleep(30)
            raise AssertionError("observation deadline did not terminate the process")
        checked, failed, running = {
            73: (2, 0, 0), 74: (0, 0, 0), 76: (2, 1, 0), 204: (2, 0, 1), 206: (2, 0, 1)
        }[number]
        total = checked + int((root / f"partial-checks-{number}").exists())
        data = {"repository": {**repo, "path": None},
                "merge_request": {"Id": f"801{number}", "Number": number,
                                  "Versions": [{"Number": 2, "SourceCommitId": "a" * 40}]},
                "step_logs_enabled": False, "check_count": checked, "total_count": total,
                "failed_count": failed, "running_count": running}

print(json.dumps({"status": "success", "data": data}))
