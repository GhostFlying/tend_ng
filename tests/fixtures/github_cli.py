#!/usr/bin/env python3
"""Fixed GitHub observations under the Server service identity."""

import json
import os
from pathlib import Path
import sys
import time

assert os.environ["GH_TOKEN"] == "fixture-read-token"
assert "TEND_TOKEN" not in os.environ
root = Path(os.environ["TEND_GITHUB_FIXTURE_DIR"])
if sys.argv[1:7] == ["api", "--hostname", "github.com", "--method", "GET", "repos/example/guide/pulls"]:
    args = dict(value.split("=", 1) for value in sys.argv[8::2])
    assert args["state"] == "all" and args["per_page"] == "2"
    owner, branch = args["head"].split(":", 1)
    assert owner == "example" and branch.startswith("tend/")
    mode = (root / "branch-mode").read_text()
    with (root / "branch-reads").open("a") as log:
        log.write(json.dumps([mode, branch]) + "\n")
    if mode == "denied":
        print(json.dumps({"message": "Not Found", "status": "404"}))
        sys.exit(1)
    candidate = {"number": 73, "head": {"ref": branch, "repo": {"full_name": "Example/Guide"}}}
    print(json.dumps([candidate, {**candidate, "number": 74}] if mode == "ambiguous" else [candidate]))
    sys.exit(0)

assert sys.argv[1:5] == ["api", "graphql", "--hostname", "github.com"], sys.argv
args = dict(value.split("=", 1) for value in sys.argv[6::2])
assert args["owner"] == "example" and args["repo"] == "guide"
assert "statusCheckRollup" in args["query"]

if "sha" in args:
    assert args["sha"] == "b" * 40
    print(json.dumps({"data": {"repository": {"object": {
        "oid": args["sha"], "statusCheckRollup": {"state": "SUCCESS",
            "contexts": {"nodes": [{"name": "build-linux", "databaseId": 13,
                "status": "COMPLETED", "conclusion": "SUCCESS"}]}}
    }}}}))
    sys.exit(0)

number = int(args["number"])
with (root / "github-reads").open("a") as log:
    log.write(f"{number}\n")

if number == 99:
    deadline = time.monotonic() + 7
    while not (root / "release-denied-pr").exists():
        assert time.monotonic() < deadline
        time.sleep(.03)

if number == 99 or (root / f"deny-{number}").exists():
    print(json.dumps({"errors": [{"type": "FORBIDDEN"}], "data": {"repository": None}}))
    sys.exit(1)

if number == 82:
    mode = (root / "wait-mode").read_text()
    head = ("c" if mode == "advanced" else "a") * 40
    print(json.dumps({"data": {"repository": {"pullRequest": {
        "title": "CI wait fixture", "state": "OPEN", "isDraft": False,
        "headRefOid": head, "headRefName": "fixture-branch",
        "headRepository": {"nameWithOwner": "Example/Guide"},
        "commits": {"nodes": [{"commit": {"oid": "b" * 40 if mode == "stale" else head,
            "statusCheckRollup": {"state": "PENDING" if mode in {"pending", "advanced"} else "SUCCESS",
                "contexts": {"nodes": [{"name": "build-linux", "databaseId": 7,
                    "status": "IN_PROGRESS" if mode in {"pending", "advanced"} else "COMPLETED",
                    "conclusion": None if mode in {"pending", "advanced"} else "SUCCESS"}]}}
        }}]},
    }}}}))
    sys.exit(0)

assert number in {73, 74}, number
print(json.dumps({"data": {"repository": {"pullRequest": {
    "title": "补充安装和验证说明",
    "state": "OPEN",
    "isDraft": False,
    "headRefOid": "a" * 40,
    "headRefName": json.loads((root / "branch-reads").read_text().splitlines()[-1])[1]
        if (root / "branch-reads").exists() else "fixture-branch",
    "headRepository": {"nameWithOwner": "Example/Guide"},
    "commits": {"nodes": [{"commit": {
        "oid": ("b" if number == 74 else "a") * 40,
        "statusCheckRollup": {"state": "SUCCESS"},
    }}]},
}}}}))
