"""Exercise the live Tend conversation using a dedicated Codex home."""

import argparse
import contextlib
import json
import os
from pathlib import Path
import signal
import socket
import sqlite3
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description="Run a native Codex smoke with dedicated local state.")
    for component in ("server", "cli", "runner"):
        parser.add_argument(component, type=lambda value: Path(value).resolve())
    parser.add_argument("--codex-home", required=True, type=lambda value: Path(value).resolve())
    parser.add_argument("--image", action="store_true")
    parser.add_argument("--direct-owner", action="store_true",
                        help="Exercise natural-language single-owner dispatch and original Chat delivery")
    args = parser.parse_args()
    personal_home = (Path.home() / ".codex").resolve()
    inherited_home = os.environ.get("CODEX_HOME")
    if (args.codex_home.is_relative_to(personal_home) or
            inherited_home and args.codex_home.is_relative_to(Path(inherited_home).resolve())):
        parser.error("specify a separate Codex home for native Tend smoke sessions")
    if not args.codex_home.is_dir():
        parser.error("the separate Codex home must already be authenticated")

    with tempfile.TemporaryDirectory(prefix="tend-phase-one-live-") as folder:
        root = Path(folder)
        source = root / "source"
        subprocess.run(["git", "init", "-q", "-b", "main", source], check=True)
        (source / "README.md").write_text("# Garden demo\n\nRun python3 seed.py to print two names.\n")
        (source / "seed.py").write_text("print('radish and thyme')\n")
        subprocess.run(["git", "-C", source, "add", "."], check=True)
        subprocess.run(["git", "-C", source, "-c", "user.name=Demo", "-c", "user.email=demo@example.invalid",
                        "commit", "-qm", "demo"], check=True)
        subprocess.run(["git", "clone", "-q", source, root / "repository"], check=True)
        runner_config = {
            "runner_id": "live-demo", "host_id": "local-phase-one", "slot_limit": 2,
            "repos": [{"id": "example", "path": "repository"}],
            "agents": [{"id": "coordinator", "category": "issue-router", "revision": "one", "reasoning_effort": "low"},
                       {"id": "worker", "category": "implementation", "revision": "one", "reasoning_effort": "low", "capabilities": ["image_gen"]}],
        }
        if args.direct_owner:
            runner_config["agents"].append({"id": "quick-worker", "category": "implementation-economy",
                                            "revision": "one", "reasoning_effort": "low"})

        (root / "runner.json").write_text(json.dumps(runner_config))
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            addr = f"127.0.0.1:{sock.getsockname()[1]}"
        env = {**os.environ, "CODEX_HOME": str(args.codex_home),
               "TEND_URL": f"http://{addr}", "TEND_TOKEN": "local-phase-acceptance"}
        processes = []

        def cli(*words, raw=False):
            response = subprocess.check_output([args.cli, *words], env=env, timeout=6, stderr=subprocess.DEVNULL)
            return response if raw else json.loads(response)

        def wait(read, predicate, deadline=150):
            expiry = time.monotonic() + deadline
            while time.monotonic() < expiry:
                observed = read()
                if predicate(observed):
                    return observed
                time.sleep(1)
            raise TimeoutError("Local model acceptance condition did not arrive")

        try:
            for name, arguments in [
                ("server", ["--listen", addr, "--db", root / "server.sqlite", "--router-cwd", root, "--router-effort", "low"]),
                ("runner", ["--config", root / "runner.json", "--db", root / "runner.sqlite"]),
            ]:
                with (root / f"{name}.log").open("w") as stderr:
                    processes.append(subprocess.Popen([getattr(args, name), *arguments], env=env, cwd=root, stderr=stderr, start_new_session=True))
            time.sleep(2)
            wait(lambda: cli("runner", "list"), bool, 8)
            if args.direct_owner:
                source = cli(
                    "chat", "send", "main",
                    "请现在在已登记的 example 仓库整理一个极短的 quickstart.txt。"
                    "阅读 README.md 和 seed.py，核对并记录 seed.py 输出的两种植物。"
                    "用 Tend 附件把完整文件和一句结论发回本对话，交付后结案。"
                    "这是本机隔离仓库，不提交、不推送，也不访问外部服务。",
                )["message_id"]
                rows = wait(lambda: cli("chat", "read", "main")["messages"],
                            lambda messages: any(m.get("reply_to") == source and m.get("artifacts")
                                                 for m in messages), 240)
                [issue] = next(m for m in rows if m["id"] == source)["issue_ids"]
                owner = wait(lambda: cli("issue", "read", issue),
                             lambda item: item["issue_state"] == "closed"
                             and item["status"] == "completed")
                assert owner["category"] in {"implementation", "implementation-economy"}
                assert owner["placement"]["agent"]["id"] in {"worker", "quick-worker"}
                with sqlite3.connect(root / "server.sqlite") as db:
                    assert db.execute("SELECT id FROM issues WHERE parent_issue_id=?", (issue,)).fetchall() == []
                    assert db.execute("SELECT id FROM tasks WHERE issue_id=?", (issue,)).fetchall() == [
                        (owner["task_id"],)]
                report = next(m for m in rows if m.get("reply_to") == source and m.get("artifacts"))
                [file] = [a for a in report["artifacts"] if a["name"] == "quickstart.txt"]
                body = cli("artifact", "download", file["id"], raw=True).lower()
                assert b"radish" in body and b"thyme" in body
                print("PASS actual Codex directly owned one Issue/Task and delivered the original file to Chat",
                      flush=True)
                return
            req_a = cli("chat", "send", "main",
                        "请立即创建一个工作 Issue，只需要登记的 example 仓库。让 Issue Agent 委派给 implementation Work Agent，"
                        "在分配的本机 Git worktree 中阅读 README.md 并创建 quickstart.txt，内容含 radish and thyme。"
                        "请使用交付工具提交 quickstart.txt，Issue Agent 汇报这个交付文件，最终从本对话把可下载的文件与简短结论交付给我。"
                        "这是本机隔离的测试仓库。不需要 git commit、push 或访问外部服务。")
            req_b = cli("chat", "send", "main",
                        "请另外创建第二个独立的工作 Issue，也用登记的 example 仓库。让 Issue Agent 委派给 implementation Work Agent。"
                        "请让 Work Agent 在开始选择写法前用 Tend 的 Attention 工具询问我要简版还是详版，需把问题沿原 Issue 送回我。"
                        "收到实际答复后使用所选写法生成一句结论，Issue Agent 把结果汇报到本对话。不需要改仓库和访问外部服务。")
            if args.image:
                graphic = cli("chat", "send", "image-smoke",
                              "请新建一次只在本机隔离仓库 example 执行的 Issue，并要求 image_gen capability。"
                              "请委派给具备 image_gen 的 implementation Work Agent，由它调用 Codex 原生 image_gen 一次，"
                              "生成只有两片简单绿色叶子的方形图标。保存为 sample.png，调用原文件交付工具，"
                              "Issue Agent 将交付汇报给你，再从原对话把可下载的 PNG 给我。不要访问外部网站，不用 Git 提交。")
            else:
                graphic = None
            print("ISSUED real Codex workflow smoke in an isolated local Git repository", flush=True)
            def awaiting_decision():
                return next((decision for decision in cli("attention", "list", "main")
                             if decision["state"] == "awaiting_user" and
                             decision["source_message_id"] == req_b["message_id"]), None)

            target = wait(awaiting_decision, bool, 240)
            cli("chat", "send", "main", "选简版。", "--reply-to", target["question_message_id"])
            wait(lambda: cli("attention", "list", "main"),
                 lambda entries: any(q["id"] == target["id"] and q["state"] == "delivered" for q in entries), 160)
            print("PASS actual Codex Attention carried through main Chat to original owner", flush=True)
            chat = wait(lambda: cli("chat", "read", "main")["messages"],
                        lambda rows: any(x.get("artifacts") and x.get("reply_to") == req_a["message_id"] for x in rows), 150)
            report = next(x for x in chat if x.get("artifacts") and x.get("reply_to") == req_a["message_id"])
            for file in report["artifacts"]:
                if file["name"] == "quickstart.txt":
                    assert b"radish and thyme" in cli("artifact", "download", file["id"], raw=True).lower()
                    break
            else:
                raise AssertionError("original chat report misses quickstart.txt")
            print("PASS actual Codex artifact returned and downloaded in the original Chat", flush=True)
            if graphic is not None:
                rows = wait(lambda: cli("chat", "read", "image-smoke")["messages"],
                            lambda messages: any(x.get("artifacts") and x.get("reply_to") == graphic["message_id"]
                                                 for x in messages), 200)
                png_ids = [item["id"] for row in rows if row.get("reply_to") == graphic["message_id"]
                           for item in row.get("artifacts", []) if item["name"] == "sample.png"]
                assert png_ids and len(png_ids) == len(set(png_ids))
                for file_id in png_ids:
                    assert cli("artifact", "download", file_id, raw=True).startswith(b"\x89PNG\r\n\x1a\n")
                print("PASS actual native image_gen output returned through the assigned capability", flush=True)
        except BaseException:
            db_path = root / "server.sqlite"
            if db_path.exists():
                with sqlite3.connect(db_path) as db:
                    with contextlib.suppress(sqlite3.Error):
                        print("Observed task states:", db.execute("SELECT category, status, COUNT(*) FROM tasks GROUP BY 1,2").fetchall(), flush=True)
                        print("Observed router outcomes:", db.execute("SELECT outcome, COUNT(*) FROM router_inputs GROUP BY outcome").fetchall(), flush=True)
            raise
        finally:
            for process in processes:
                with contextlib.suppress(ProcessLookupError):
                    os.killpg(process.pid, signal.SIGTERM)
            for process in processes:
                try:
                    process.wait(timeout=6)
                except subprocess.TimeoutExpired:
                    with contextlib.suppress(ProcessLookupError):
                        os.killpg(process.pid, signal.SIGKILL)
                    process.wait()


if __name__ == "__main__":
    main()
