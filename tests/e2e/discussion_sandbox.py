"""Exercise the real Codex App Server's Discussion sandbox without service auth."""
import argparse
import json
from pathlib import Path
import subprocess
import tempfile

PROFILE = "tend-discussion"
TOOLS = ("tend_read_discussion_reference", "tend_read_discussion_delivery", "tend_read_discussion_history")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--codex", required=True, type=lambda value: Path(value).resolve())
    parser.add_argument("--scratch", required=True, type=lambda value: Path(value).resolve(),
                        help="Use a local scratch parent outside /tmp for Codex's sandbox helpers")
    args = parser.parse_args()
    args.scratch.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=args.scratch, prefix="discussion-sandbox-") as directory:
        root = Path(directory).resolve()
        home, cwd, repo, forbidden = (root / part for part in ("service-home", "empty-cwd", "repo", "forbidden"))
        for path in (home, cwd, repo, forbidden):
            path.mkdir()
        (repo / "visible.txt").write_text("attached checkout contents")
        (forbidden / "private.txt").write_text("unattached-source-secret")
        (home / "synthetic-auth").write_text("service-file-secret")
        (repo / "escape").symlink_to(forbidden / "private.txt")
        subprocess.run(["git","init","-q","-b","main",repo],check=True)
        subprocess.run(["git","-C",repo,"add","visible.txt"],check=True)
        subprocess.run(["git","-C",repo,"-c","user.name=Fixture","-c","user.email=fixture@example.invalid",
                        "commit","-qm","test object"],check=True)
        allowed = (":minimal", str(repo), str(cwd), str(args.codex))
        table = "{" + ",".join(f'{json.dumps(path)}="read"' for path in allowed) + "}"
        config = [
            f'default_permissions="{PROFILE}"',f"permissions.{PROFILE}.filesystem={table}",
            f"permissions.{PROFILE}.network.enabled=false", 'shell_environment_policy={inherit="none"}',
            'approval_policy="never"', 'web_search="disabled"', "mcp_servers={}", "features.apps=false",
            "features.plugins=false", "features.remote_plugin=false", "features.multi_agent=false",
            "features.multi_agent_v2=false", "features.skill_mcp_dependency_install=false",
            "features.code_mode=false", "features.code_mode_host=false",
        ]
        launch = [args.codex, "app-server", "--listen", "stdio://", "--strict-config"]
        for setting in config:
            launch.extend(("-c",setting))
        with subprocess.Popen(launch, cwd=cwd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True,
                              env={"HOME":str(home),"CODEX_HOME":str(home),"OPENAI_API_KEY":"service-env-secret",
                                   "PATH":"/usr/local/bin:/usr/bin:/bin"}) as process:
            counter = 0
            def call(method, params, notify=False):
                nonlocal counter
                counter += 1
                frame = {"method":method,"params":params}
                if not notify:
                    frame["id"] = counter
                process.stdin.write(json.dumps(frame) + "\n")
                process.stdin.flush()
                if notify:
                    return None
                for line in process.stdout:
                    message = json.loads(line)
                    if message.get("id") == counter:
                        assert "result" in message, message
                        return message["result"]
                raise AssertionError(f"App Server exited before {method}")

            call("initialize", {"clientInfo":{"name":"tend-sandbox-test","version":"1"},
                                "capabilities":{"experimentalApi":True}})
            call("initialized", {}, notify=True)
            config = call("config/read", {"includeLayers":False})["config"]
            assert not config.get("mcp_servers"), config.get("mcp_servers")
            start = call("thread/start", {"cwd":str(cwd),"permissions":PROFILE,
                "approvalPolicy":"never","historyMode":"legacy","dynamicTools":[
                    {"name":name,"description":"Scoped Tend discussion fixture tool",
                     "inputSchema":{"type":"object","properties":{}}} for name in TOOLS
                ]})
            assert start["sandbox"] == {"type":"readOnly","networkAccess":False}, start
            assert start["activePermissionProfile"]["id"] == PROFILE
            assert start["approvalPolicy"] == "never"
            spec = {
                "repo": str(repo), "hidden": str(forbidden / "private.txt"),
                "auth": str(home / "synthetic-auth"), "cwd":str(cwd), "server_pid":process.pid,
            }
            command = r"""
import json, os, pathlib, socket, subprocess, sys
scope=json.loads(sys.argv[1])
repo=pathlib.Path(scope["repo"])
assert (repo/"visible.txt").read_text()=="attached checkout contents"
assert subprocess.check_output(["/usr/bin/git","-C",repo,"show","HEAD:visible.txt"],text=True)=="attached checkout contents"
assert "OPENAI_API_KEY" not in os.environ and "TEND_TOKEN" not in os.environ
try:
    app_environment=pathlib.Path(f"/proc/{scope['server_pid']}/environ").read_bytes()
except OSError:
    pass
else:
    assert b"service-env-secret" not in app_environment
for path in (scope["hidden"],scope["auth"],repo/"escape"):
    try:
        pathlib.Path(path).read_text()
    except OSError:
        pass
    else:
        raise AssertionError(f"Unattached path read: {path}")
for path in (repo/"write-probe", pathlib.Path(scope["cwd"])/"write-probe"):
    try:
        path.write_text("denied")
    except OSError:
        pass
    else:
        raise AssertionError(f"Writable path: {path}")
assert os.environ["CODEX_SANDBOX_NETWORK_DISABLED"]=="1"
try:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1",0))
except PermissionError:
    pass
else:
    raise AssertionError("TCP bind was not blocked")
print("native checkout and Git object readable; other roots, credentials, write, and TCP denied")
"""
            result = call("command/exec", {
                "command":["/usr/bin/python3","-c",command,json.dumps(spec)],
                "permissionProfile":PROFILE, "cwd":str(cwd),
            })
            assert result["exitCode"] == 0 and "TCP denied" in result["stdout"], result
            print(result["stdout"].strip())
            assert not (repo / "write-probe").exists()
            process.stdin.close()
            process.wait(timeout=8)
    print("PASS actual native Discussion thread permissions and file/Git/network boundaries (no model call)")


if __name__ == "__main__":
    main()
