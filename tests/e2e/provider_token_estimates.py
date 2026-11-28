"""Check provider pricing on real loopback Server, Runner, CLI and optionally Web."""
import argparse
from decimal import Decimal
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import time

from chat import server, wait_for
from issue_chat import TESTS, WEB, free_port, prepare
from issue_token_usage import runner_process

sys.path.insert(0, str(TESTS / "fixtures"))
from provider_pricing import CASES, DEEPSEEK, GROUPS, ISSUE_TITLE  # noqa: E402


def deepseek(usage):
    return next(row for row in usage["models"] if row["model"] == DEEPSEEK)


def check(usage, expected, request_count, input_sum, hits, outputs):
    row = deepseek(usage)
    assert row["estimate"] == {"usd": expected, "partial": False}, row
    assert row["reported_requests"] == request_count
    assert row["cache_write_input_tokens"] == {"tokens": None, "reported_requests": 0}
    counts = [input_sum - hits, hits, outputs]
    prices = [Decimal(".14"), Decimal(".0042"), Decimal(".42")]
    assert sum(n * price / 1_000_000 for n, price in zip(counts, prices)) == Decimal(expected)
    [tier] = row["price"]["tiers"]
    assert tier["tier"] == "standard"
    for i, count in zip((0, 1, 3), counts):
        field = tier["components"][i]
        assert field["tokens"] == str(count)
        assert field["priced_requests"] == request_count
    return row


def run(args, root):
    address = free_port()
    prepare(root, free_port())
    config = root / "runner.json"
    env = {**os.environ, "TEND_TOKEN": "isolated-provider-pricing-fixture",
           "TEND_URL": f"http://{address}", "CODEX_HOME": str(root / "codex-home")}
    command = [args.runner, "--config", config, "--db", root / "runner.sqlite",
               "--app-server", sys.executable, TESTS / "fixtures/token_usage_app_server.py", root]
    options = dict(listen=address, web_dir=WEB / "dist" if args.evidence else None)

    def cli(*words):
        return json.loads(subprocess.check_output([args.cli, *words], env=env, timeout=15))

    with server(args.server, root, env, **options), runner_process(command, env, root):
        wait_for(lambda: cli("runner", "list"), bool)
        ids = {}
        for prompt, title in CASES.items():
            source = cli("chat", "send", "main", prompt)["message_id"]
            [ids[title]] = wait_for(lambda: next(
                (row.get("issue_ids") for row in cli("chat", "read", "main")["messages"]
                 if row["id"] == source), None), bool)
            count, _ = GROUPS[title]
            expected = count + (2 if title == ISSUE_TITLE else 0)
            deadline = time.monotonic() + 40
            while True:
                n = cli("issue", "token-usage", ids[title])["direct"]["reported_requests"]
                if n == expected:
                    break
                assert time.monotonic() < deadline, (title, n, expected)
                time.sleep(.1)
        primary = ids[ISSUE_TITLE]
        live = cli("token-usage", "overview")
    db_file = root / "server.sqlite"

    def ledger(db):
        return db.execute("SELECT * FROM token_requests ORDER BY thread_id, turn_id, response_id").fetchall()

    with sqlite3.connect(db_file) as db:
        [run] = db.execute("SELECT run_id FROM token_runs WHERE issue_id=?", (primary,)).fetchone()
        # Reproduce an old writer's persisted default-zero shape on the GPT
        # request without details, including its raw coverage counters.
        assert db.execute(
            "UPDATE token_requests SET last=json_set(json_remove(last,'$.provider_counts'),"
            " '$.cached_input_tokens',0,'$.cache_write_input_tokens',0,'$.reasoning_output_tokens',0)"
            " WHERE run_id=? AND response_id='pricing-180'",
            (run,),
        ).rowcount == 1
        for table, column, base, condition, values in [
            ("token_run_tallies", "tally", "$", "run_id=?", (run,)),
            ("token_issue_rollups", "summary", "$.tally", "issue_id=?", (primary,)),
            ("token_global_rollup", "summary", "$.tally", "id=1", ()),
        ]:
            keys = [f"{base}.known[{i}]" for i in (2, 3, 5)]
            pairs = ", ".join(f"'{key}', json_extract({column}, '{key}')+1" for key in keys)
            db.execute(
                f"UPDATE {table} SET {column}=json_remove(json_set({column}, {pairs}), ?)"
                f" WHERE {condition}", (f"{base}.model_usage", *values),
            )
        original = ledger(db)
        assert len(original) == 422 and all(row[-1] is None for row in original)
        db.execute("PRAGMA user_version=51")
    with server(args.server, root, env, **options):
        issue = cli("issue", "token-usage", primary)
        overview = cli("token-usage", "overview")
        assert live["total"]["estimate"] == overview["total"]["estimate"]
        issue_model = check(issue["direct"], "0.1038973152", 180, 1_211_884, 806_656, 104_232)
        runner = next(row["usage"] for row in overview["sources"] if row["source"] == "runner")
        runner_model = check(runner, "4.501813372", 420, 38_855_868, 10_000_060, 1_000_000)
        assert deepseek(overview["total"]) == runner_model
        gpt = next(row for row in issue["direct"]["models"] if row["model"] == "gpt-6.2-sol")
        assert gpt["estimate"] == {"usd": "0.340654", "partial": True}, gpt
        standard, long = gpt["price"]["tiers"]
        assert [standard["tier"], long["tier"]] == ["standard", "long"]
        assert standard["baseline_input"]["tokens"] == "1000"
        assert standard["notes"] == {"read_discount_unobserved": 1, "write_adjustment_unobserved": 1}
        for index in (1, 2):
            assert standard["components"][index]["priced_requests"] == 0
            assert standard["components"][index]["tokens"] is None
        assert long["components"][0]["tokens"] == "62001"
        assert long["components"][2]["tokens"] == "10000"
        cli("token-usage", "rebuild", "--all")
        assert cli("issue", "token-usage", primary) == issue
        assert cli("token-usage", "overview") == overview
        with sqlite3.connect(db_file) as db:
            # Later independent migrations may raise the current schema version.
            assert db.execute("PRAGMA user_version").fetchone()[0] >= 52
            assert ledger(db) == original
            samples = db.execute(
                "SELECT last FROM token_requests q JOIN token_runs r USING(run_id) "
                "WHERE r.issue_id = ? ORDER BY q.response_id", [primary],
            ).fetchall()
        native = [json.loads(raw) for (raw,) in samples]
        deep_native = [r for r in native if r["model"]["id"] == DEEPSEEK]
        assert all(r["cache_write_input_tokens"] is None for r in deep_native)
        assert sum("cache_miss_input_tokens" in r for r in deep_native) == 90
        assert sum(
            r["model"]["id"] == "gpt-6.2-sol" and "provider_counts" not in r
            and r["cached_input_tokens"] == r["cache_write_input_tokens"] == 0
            for r in native
        ) == 1
        if args.evidence:
            args.evidence.mkdir(exist_ok=True, parents=True)
            (args.evidence / "provider-pricing-evidence.json").write_text(
                json.dumps({
                    "scope": "isolated fixture; Runner historical token split is not known",
                    "fixture_groups": {title: dict(requests=count, input_tokens=counts[0],
                                                   cached_input_tokens=counts[1], output_tokens=counts[2])
                                       for title, (count, counts) in GROUPS.items()},
                    "previous_partition_formula_on_the_same_fixture": {
                        "issue_read_plus_output_only_usd": "0.0471653952",
                        "omitted_issue_miss_usd": "0.05673192",
                    },
                    "issue_deepseek": issue_model, "runner_deepseek": runner_model,
                    "issue_gpt": gpt, "issue_raw_counts": native,
                    "post_migration_and_explicit_rebuild_agree": True,
                    "ledger_unchanged": True,
                    "mixed_field_replay_conflicts": 0,
                    "legacy_default_zero_counted_as_billed": False,
                }, indent=2) + "\n",
            )
            [row] = [r for r in overview["issues"] if r["issue_id"] == primary]
            subprocess.run(
                ["node", WEB / "tests/provider-token-estimates.mjs", str(row["number"])],
                env={**env, "TEND_SCREENSHOTS": str(args.evidence)}, check=True, timeout=90,
            )
    print("Provider pricing: 180 requests = $0.1038973152; Runner aggregate = $4.501813372.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for executable in ("server", "cli", "runner"):
        parser.add_argument(executable, type=lambda path: Path(path).resolve())
    parser.add_argument("--evidence", type=lambda path: Path(path).resolve(),
                        help="also run Chromium and store isolated API evidence and screenshots")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="tend-provider-pricing-") as temp:
        run(args, Path(temp))


if __name__ == "__main__":
    main()
