"""Check the real v2 sync path and the upgrade window for simulated generations."""

import argparse
from contextlib import closing
from http.client import HTTPConnection
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile

from chat import server


def exercise(args, root, address, env):
    host_id = "fixture-host"
    second_host_id = "second-fixture-host"
    v1, v2, v3 = "runner-v1", "simulated-v2", "simulated-v3"

    def cli(*words):
        return json.loads(subprocess.check_output([args.cli, *words], env=env, timeout=10))

    def register(runner_id, host):
        config = root / f"{runner_id}.json"
        config.write_text(json.dumps({
            "runner_id": runner_id, "host_id": host, "slot_limit": 1,
            "repos": [], "agents": [], "workspace_root": "workspaces",
        }))
        subprocess.run(
            [args.runner, "--config", config, "--db", root / f"{runner_id}.sqlite", "--once"],
            env=env, check=True, timeout=10,
        )

    def runners():
        return {row["profile"]["runner_id"]: row for row in cli("runner", "list")}

    def check_upgrade(target, minimum, blockers):
        result = cli("server", "upgrade-check", str(target))
        assert result == {
            "target_protocol": target,
            "compatible_range": {"min": minimum, "max": target},
            "allowed": not blockers,
            "blockers": blockers,
        }, result

    register(v1, host_id)
    [registration] = runners().values()
    assert registration["profile"]["runner_id"] == v1, registration
    assert registration["profile"]["host_id"] == host_id, registration
    assert registration["protocol"] == 2, registration
    check_upgrade(1, 1, [{"host_id": host_id, "runner_id": v1, "protocol": 2}])
    check_upgrade(2, 1, [])
    print("PASS real v2 Runner registration and target 1 and 2 upgrade preflight")

    for protocol, expected in [(1, 200), (2, 200), (0, 426), (3, 426)]:
        payload = {"protocol": protocol, "profile": registration["profile"],
                   "receipts": [], "records": []}
        with closing(HTTPConnection(address, timeout=5)) as connection:
            connection.request("POST", f"/api/v1/runners/{v1}/sync", json.dumps(payload), {
                "Authorization": f"Bearer {env['TEND_TOKEN']}",
                "Content-Type": "application/json",
            })
            response = connection.getresponse()
            body = json.load(response)
            assert response.status == expected, (protocol, response.status, body)
            if expected == 426:
                assert body["error"] == "unsupported Runner protocol", body
    print("PASS current Server sync accepts protocols 1 and 2 and rejects 0 and 3")

    # Register valid records with the v2 binary. Only the temporary Server database
    # supplies the simulated v2 and v3 declarations used by upgrade preflight.
    register(v2, host_id)
    register(v3, second_host_id)
    with closing(sqlite3.connect(root / "server.sqlite")) as ledger:
        with ledger:
            ledger.executemany(
                "UPDATE runners SET protocol = ? WHERE id = ?", [(1, v1), (2, v2), (3, v3)],
            )

        blocked_v1 = {"host_id": host_id, "runner_id": v1, "protocol": 1}
        blocked_v2 = {"host_id": host_id, "runner_id": v2, "protocol": 2}
        blocked_v3 = {"host_id": second_host_id, "runner_id": v3, "protocol": 3}
        check_upgrade(2, 1, [blocked_v3])
        check_upgrade(3, 2, [blocked_v1])
        check_upgrade(4, 3, [blocked_v1, blocked_v2])
        print("PASS simulated matrix: v3 blocks target 2, v1 blocks target 3, v1/v2 block target 4")

        downgrade = subprocess.run([args.cli, "runner", "replace", v2, v1],
                                   env=env, text=True, capture_output=True, timeout=10)
        assert downgrade.returncode and "HTTP 409" in downgrade.stderr, downgrade.stderr
        assert runners()[v2]["successor"] is None
        print("PASS newer Runner cannot drain into an older protocol")
        cli("runner", "replace", v1, v2)
        replacing = runners()[v1]
        assert replacing["successor"] == v2 and replacing["drained_at"] is None, replacing
        check_upgrade(3, 2, [blocked_v1])
        with ledger:
            ledger.execute(
                "UPDATE runner_replacements SET drained_at = unixepoch() WHERE predecessor = ?",
                (v1,),
            )
            ledger.execute("UPDATE runners SET last_seen = 0 WHERE id = ?", (v1,))
        drained = runners()[v1]
        assert drained["drained_at"] is not None and drained["retired_at"] is None, drained
        assert not drained["online"], drained
        check_upgrade(3, 2, [blocked_v1])
        print("PASS a replaced v1 still blocks target 3 before and after an offline drain")

    cli("runner", "retire", v1)
    roster = runners()
    assert {runner: row["protocol"] for runner, row in roster.items()} == {
        v1: 1, v2: 2, v3: 3,
    }, roster
    assert roster[v1]["retired_at"] is not None, roster[v1]
    assert roster[v2]["retired_at"] is None and roster[v3]["retired_at"] is None, roster
    check_upgrade(3, 2, [])
    print("PASS retired v1 remains listed and simulated v2/v3 allow target 3")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for executable in ("server", "cli", "runner"):
        parser.add_argument(executable, type=lambda value: Path(value).resolve())
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="tend-protocol-window-") as temp:
        root = Path(temp)
        env = {**os.environ, "TEND_TOKEN": "protocol-window-fixture",
               "CODEX_HOME": str(root / "codex")}
        with server(args.server, root, env) as address:
            exercise(args, root, address, {**env, "TEND_URL": f"http://{address}"})


if __name__ == "__main__":
    main()
