"""Compare #147 and the candidate on the same synthetic persisted ledger.

Generate the fixture with the Rust test request_pricing_coverage_fixture and
TEND_REQUEST_PRICING_FIXTURE_DB set to a new absolute path. No original
rollouts or production database are needed.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import tempfile

from chat import server
from issue_chat import WEB


def old_wire(value):
    if isinstance(value, dict):
        return {key: old_wire(item) for key, item in value.items() if key != "unpriced_requests"}
    if isinstance(value, list):
        return [old_wire(item) for item in value]
    return value


def run(args, root):
    db = root / "server.sqlite"
    shutil.copyfile(args.fixture, db)

    def ledger():
        with sqlite3.connect(db) as conn:
            return [conn.execute(f"SELECT * FROM {table} ORDER BY 1,2,3").fetchall()
                    for table in ("token_requests", "token_agent_requests")]

    original = ledger()
    with sqlite3.connect(db) as conn:
        # Ask the actual baseline binary to regenerate its own valid v52
        # pricing projection from the same saved ledgers before upgrading.
        conn.execute("PRAGMA user_version=51")

    env = {**os.environ, "TEND_TOKEN": "isolated-request-pricing-coverage",
           "CODEX_HOME": str(root / "codex-home")}

    def snapshot(env):
        def cli(*words):
            return json.loads(subprocess.check_output([args.cli, *words], env=env, timeout=10))

        overview = cli("token-usage", "overview")
        [issue] = [row for row in overview["issues"] if row["issue_id"] == "pricing-root"]
        number = issue["number"]
        return number, {
            "overview": overview,
            "children": cli("token-usage", "overview", "--parent", f"#{number}"),
            "issue": cli("issue", "token-usage", f"#{number}"),
        }

    with server(args.baseline_server, root, env) as address:
        _, baseline = snapshot({**env, "TEND_URL": f"http://{address}"})
    assert ledger() == original

    with server(args.server, root, env, web_dir=WEB / "dist" if args.evidence else None) as address:
        local = {**env, "TEND_URL": f"http://{address}"}
        number, candidate = snapshot(local)
        assert old_wire(candidate) == baseline, "pricing upgrade changed an existing projection field"
        with sqlite3.connect(db) as conn:
            assert conn.execute("PRAGMA user_version").fetchone()[0] >= 54
        total = candidate["overview"]["total"]
        assert total["estimate"] == {"usd": "1.0942952", "partial": True, "unpriced_requests": 4}
        direct = candidate["issue"]["direct"]
        assert direct["reported_requests"] == 52
        assert direct["estimate"] == {"usd": "1.0942952", "partial": True, "unpriced_requests": 0}
        feishu = next(s for s in candidate["overview"]["sources"] if s["source"] == "feishu")
        assert feishu["usage"]["estimate"] == {"usd": None, "partial": True, "unpriced_requests": 4}
        assert feishu["responses_without_usage"] == 0
        if args.evidence:
            args.evidence.mkdir(parents=True, exist_ok=True)
            for name, view in [("baseline", baseline), ("candidate", candidate)]:
                (args.evidence / f"pricing-{name}.json").write_text(json.dumps(view, ensure_ascii=False, indent=2) + "\n")
            subprocess.run(["node", WEB / "tests/request-pricing-coverage.mjs", str(number)],
                           env={**local, "TEND_SCREENSHOTS": str(args.evidence)}, check=True, timeout=120)
    assert ledger() == original
    print("PASS #147 v52 → candidate v54: all previous fields and both raw ledgers unchanged; 52 priced, 4 Feishu unpriced")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("fixture", "baseline_server", "server", "cli"):
        parser.add_argument(name, type=Path)
    parser.add_argument("--evidence", type=Path)
    with tempfile.TemporaryDirectory(prefix="tend-pricing-coverage-") as temp:
        run(parser.parse_args(), Path(temp))
