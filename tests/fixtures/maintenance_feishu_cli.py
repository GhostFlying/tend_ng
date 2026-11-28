"""Deterministic discovery windows for the maintenance slice. No network."""
from datetime import datetime
import json
import os
from pathlib import Path
import sys
import time

root = Path(__file__).parent
scenario = json.loads((root / "maintenance-feishu.json").read_text())
assert "TEND_TOKEN" not in os.environ
args = sys.argv[1:]
if args == ["whoami"]:
    print(json.dumps({"brand": "feishu", "identity": "user", "available": True,
                      "onBehalfOf": {"openId": "ou_maintenance_fixture"}}))
else:
    assert args[:3] == ["api", "POST", "/open-apis/im/v1/messages/search"], args
    time_range = json.loads(args[args.index("--data") + 1])["filter"]["time_range"]
    through = int(datetime.fromisoformat(time_range["end_time"]).timestamp())
    if scenario["mode"].startswith("held-"):
        gate = f"maintenance-{scenario['mode'].removeprefix('held-')}"
        (root / f"{gate}-started").touch()
        release = root / f"{gate}-release"
        until = time.monotonic() + 22
        while not release.exists() and time.monotonic() < until:
            time.sleep(0.05)
        if not release.exists() or scenario.get("release_result") != "success":
            sys.exit(5)
    if scenario["mode"] == "incomplete":
        print("incomplete fixture response")
        sys.exit(0)
    if scenario["mode"] == "stalled" or (
        scenario["mode"] == "partial" and through > scenario["first_through"]
    ):
        sys.exit(5)
    print(json.dumps({"ok": True, "identity": "user", "data": {"items": [], "has_more": False}}))
