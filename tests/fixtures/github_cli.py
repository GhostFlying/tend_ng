#!/usr/bin/env python3
"""Fixed GitHub observations under the Server service identity."""

import json
import os
from pathlib import Path
import sys
import time

assert sys.argv[1:5] == ["api", "graphql", "--hostname", "github.com"], sys.argv
args = dict(value.split("=", 1) for value in sys.argv[6::2])
assert args["owner"] == "example" and args["repo"] == "guide"
assert "statusCheckRollup" in args["query"]
assert os.environ["GH_TOKEN"] == "fixture-read-token"
assert "TEND_TOKEN" not in os.environ

root = Path(os.environ["TEND_GITHUB_FIXTURE_DIR"])
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

assert number in {73, 74}, number
print(json.dumps({"data": {"repository": {"pullRequest": {
    "title": "补充安装和验证说明",
    "state": "OPEN",
    "isDraft": False,
    "headRefOid": "a" * 40,
    "commits": {"nodes": [{"commit": {
        "oid": ("b" if number == 74 else "a") * 40,
        "statusCheckRollup": {"state": "SUCCESS"},
    }}]},
}}}}))
