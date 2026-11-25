"""Exercise Web sessions against an isolated Server, optionally in Chromium over TLS."""

import argparse
from contextlib import closing, contextmanager
from http.client import HTTPConnection
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import base64
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import ssl
import subprocess
import tempfile
import threading
import time

from chat import server

TOKEN = "fixture-tend-web-session-only"
ROTATED = "fixture-tend-web-session-rotated"
SESSION = "/api/v1/web/session"
CHAT = "/api/v1/chats/main/messages"
WEB = Path(__file__).resolve().parents[2] / "web"


def call(address, method, path=SESSION, headers=None, payload=None):
    with closing(HTTPConnection(address, timeout=5)) as conn:
        conn.request(method, path, payload, headers or {})
        response = conn.getresponse()
        return response.status, dict(response.getheaders()), response.read()


def check(expected, reply):
    status, headers, _ = reply
    assert status == expected, (expected, status)
    if status == 401:
        assert "Set-Cookie" not in headers  # No stale response may erase a newer login.
    assert "Access-Control-Allow-Credentials" not in headers
    assert "Access-Control-Allow-Origin" not in headers
    return headers


def attributes(headers):
    pair, *flags = headers["Set-Cookie"].split("; ")
    return pair, set(flags)


def login(address, request, remember, token=TOKEN):
    return call(address, "POST", headers={
        **request, "Authorization": f"Bearer {token}", "Content-Type": "application/json",
    }, payload=json.dumps({"remember": remember}))


def web_headers(origin):
    return {"Host": origin.split("://")[1], "Origin": origin, "X-Tend-Web": "1"}


def check_loopback(address):
    origin = f"http://{address}"
    headers = web_headers(origin)
    check(401, call(address, "GET"))
    check(401, login(address, headers, False, token="invalid-fixture"))
    check(403, login(address, headers, True))
    for unsafe in [
        {**headers, "Origin": "null"},
        {**headers, "Origin": f"https://{address}"},
        {**headers, "X-Tend-Web": "0"},
        {key: value for key, value in headers.items() if key != "Origin"},
        {key: value for key, value in headers.items() if key != "X-Tend-Web"},
        {**web_headers("http://not-loopback.invalid"), "X-Forwarded-Proto": "https"},
    ]:
        assert "Set-Cookie" not in check(403, login(address, unsafe, False))

    cookie, flags = attributes(check(204, login(address, headers, False)))
    assert cookie.startswith("tend-loopback-session=")
    assert flags == {"Path=/", "HttpOnly", "SameSite=Lax"}
    auth = {"Cookie": cookie}
    check(204, call(address, "GET", headers=auth))
    check(204, call(address, "HEAD", headers=auth))
    check(200, call(address, "GET", CHAT, auth))
    check(403, call(address, "POST", CHAT, auth, "{}"))
    check(403, call(address, "POST", CHAT, {**auth, **headers, "Origin": "http://other.invalid"}, "{}"))
    check(400, call(address, "POST", CHAT, {**auth, **headers}, "{}"))
    for route in ("/api/v1/issues/42/start", "/api/v1/issues/42/state"):
        check(403, call(address, "POST", route, auth, "{}"))
        check(401, call(address, "GET", route, auth))
        check(401, call(address, "HEAD", route, auth))
    reply = "/api/v1/issues/fixture-issue/attentions/fixture-attention/answer"
    history = "/api/v1/issues/fixture-issue/attention-replies"
    check(403, call(address, "POST", reply, auth, "{}"))
    check(403, call(address, "POST", reply, {**auth, **headers, "Origin": "http://other.invalid"}, "{}"))
    no_mark = {key: value for key, value in headers.items() if key != "X-Tend-Web"}
    check(403, call(address, "POST", reply, {**auth, **no_mark}, "{}"))
    check(400, call(address, "POST", reply, {**auth, **headers}, "{}"))
    check(401, call(address, "GET", reply, auth))
    check(401, call(address, "POST", history, {**auth, **headers}, "{}"))
    check(404, call(address, "GET", history, auth))
    check(401, call(address, "OPTIONS", CHAT, {**auth, "Origin": "http://other.invalid"}))
    check(403, call(address, "DELETE", headers=auth))
    check(204, call(address, "GET", headers=auth))
    cleared, flags = attributes(check(204, call(address, "DELETE", headers={**headers, **auth})))
    assert cleared == "tend-loopback-session=" and "Max-Age=0" in flags
    check(401, call(address, "GET", headers=auth))
    print("PASS loopback session, HTTPS requirement, exact Origin and request-header checks")


def check_https(args, root, address, origin, env):
    headers = web_headers(origin)
    host = {"Host": headers["Host"]}
    times = []
    cookies = []
    for remember, ttl in [(False, 43200), (True, 604800), (True, 604800)]:
        start = int(time.time())
        cookie, flags = attributes(check(204, login(address, headers, remember)))
        end = int(time.time())
        assert cookie.startswith("__Host-tend-session=")
        expected = {"Path=/", "HttpOnly", "SameSite=Lax", "Secure"}
        if remember:
            expected.add(f"Max-Age={ttl}")
        assert flags == expected
        cookies.append(cookie)
        times.append((start + ttl, end + ttl))
    assert len(set(cookies)) == len(cookies)

    def read_stored(ledger, cookie):
        secret = cookie.split("=", 1)[1]
        raw = base64.urlsafe_b64decode(secret + "=")
        assert len(raw) == 32
        digest = hashlib.sha256(raw).digest()
        for db_file in root.glob("server.sqlite*"):
            data = db_file.read_bytes()
            assert raw not in data and secret.encode() not in data and TOKEN.encode() not in data
        return ledger.execute(
            "SELECT credential_generation, origin, expires_at FROM web_sessions WHERE id_digest = ?",
            [digest],
        ).fetchone()

    auth = {**host, "Cookie": cookies[1]}
    check(204, call(address, "GET", headers=auth))
    check(401, call(address, "GET", headers={**auth, "Host": "different.invalid"}))
    if ":" in host["Host"]:
        check(401, call(address, "GET", headers={**auth, "Host": host["Host"].split(":")[0]}))
    check(200, call(address, "GET", CHAT, auth))
    check(404, call(address, "GET", "/api/v1/artifacts/fixture-missing", auth))
    check(404, call(address, "GET", "/api/v1/runs/fixture-missing/rollout", auth))
    check(401, call(address, "GET", CHAT, {**auth, "Authorization": "Bearer invalid-fixture"}))
    check(401, call(address, "POST", CHAT, {**auth, "Authorization": "invalid-fixture"}, "{}"))
    check(400, call(address, "POST", CHAT, {**auth, "Authorization": f"Bearer {TOKEN}"}, "{}"))
    check(401, call(address, "GET", headers={**auth, "Authorization": f"Bearer {TOKEN}"}))

    for method, route in [
        ("POST", "/api/v1/runners/session-fixture/sync"),
        ("POST", "/api/v1/runners/session-fixture/reconcile/task"),
        ("PUT", "/api/v1/runners/session-fixture/artifacts/file"),
        ("GET", "/api/v1/upgrade-check/2"),
    ]:
        check(401, call(address, method, route, {**auth, **headers}))
    subprocess.run([args.cli, "chat", "read", "main"], env=env, stdout=subprocess.DEVNULL,
                   check=True, timeout=10)
    config = root / "runner.json"
    config.write_text(json.dumps({
        "runner_id": "session-fixture", "host_id": "fixture-host", "slot_limit": 1,
        "repos": [], "agents": [], "workspace_root": "workspaces",
    }))
    subprocess.run([args.runner, "--config", config, "--db", root / "runner.sqlite", "--once"],
                   env=env, check=True, timeout=10)
    check(401, call(address, "GET", "/api/v1/runners/session-fixture/inspect", auth))
    _, _, runners = call(address, "GET", "/api/v1/runners", auth)
    assert json.loads(runners)[0]["profile"]["runner_id"] == "session-fixture"

    with closing(sqlite3.connect(root / "server.sqlite")) as ledger:
        for cookie, (earliest, latest) in zip(cookies, times):
            generation, bound_origin, expires = read_stored(ledger, cookie)
            assert len(generation) == 32 and bound_origin == origin and earliest <= expires <= latest
        replacement, _ = attributes(check(204, login(address, {**headers, "Cookie": cookies[0]}, False)))
        check(401, call(address, "GET", headers={**host, "Cookie": cookies[0]}))
        check(204, call(address, "GET", headers={**host, "Cookie": replacement}))
        check(204, call(address, "DELETE", headers={**headers, "Cookie": replacement}))
        check(401, call(address, "GET", headers={**host, "Cookie": replacement}))
        check(204, call(address, "GET", headers=auth))
        with ledger:
            ledger.execute("UPDATE web_sessions SET expires_at = unixepoch() WHERE id_digest IN "
                           "(SELECT id_digest FROM web_sessions ORDER BY expires_at DESC, id_digest LIMIT 1)")
        expired = [cookie for cookie in cookies[1:] if read_stored(ledger, cookie)[2] <= time.time()]
        assert len(expired) == 1
        check(401, call(address, "GET", headers={**host, "Cookie": expired[0]}))
        # A read neither deletes the expired row nor rewrites the remaining expiry.
        assert read_stored(ledger, expired[0]) is not None
        remaining = next(cookie for cookie in cookies[1:] if cookie != expired[0])
        old_expiry = read_stored(ledger, remaining)[2]
        check(204, call(address, "GET", headers={**host, "Cookie": remaining}))
        assert read_stored(ledger, remaining)[2] == old_expiry
    print("PASS SHA-256-only storage, cookie attributes, absolute TTLs, single revocation, and Bearer CLI/Runner")
    return {**host, "Cookie": remaining}


@contextmanager
def tls_proxy(root, certificate, key):
    """TLS termination with the browser Host preserved, like a loopback transport."""
    class Proxy(BaseHTTPRequestHandler):
        upstream = None
        protocol_version = "HTTP/1.1"

        def log_message(self, *_):
            pass

        def forward(self):
            size = int(self.headers.get("Content-Length", 0))
            with closing(HTTPConnection(self.upstream, timeout=5)) as conn:
                conn.request(self.command, self.path, self.rfile.read(size) if size else None,
                             dict(self.headers.items()))
                reply = conn.getresponse()
                body = reply.read()
                try:
                    self.send_response(reply.status)
                    for name, value in reply.getheaders():
                        if name.lower() not in {"connection", "transfer-encoding", "content-length",
                                                "server", "date"}:
                            self.send_header(name, value)
                    self.send_header("Content-Length", str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)
                except (BrokenPipeError, ConnectionResetError, ssl.SSLEOFError):
                    # The browser may abort an obsolete session check or close a tab.
                    self.close_connection = True

        do_GET = do_POST = do_DELETE = do_HEAD = forward

    with ThreadingHTTPServer(("127.0.0.1", 0), Proxy) as proxy:
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(root / certificate, root / key)
        proxy.socket = context.wrap_socket(proxy.socket, server_side=True)
        thread = threading.Thread(target=proxy.serve_forever, daemon=True)
        thread.start()
        try:
            yield f"https://127.0.0.1:{proxy.server_port}", Proxy
        finally:
            proxy.shutdown()
            thread.join()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for executable in ("server", "cli", "runner"):
        parser.add_argument(executable, type=lambda value: Path(value).resolve())
    parser.add_argument("--web", type=Path, help="built Web dist directory, enables Chromium")
    parser.add_argument("--screenshots", type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="tend-web-session-") as temp:
        root = Path(temp)
        env = {**os.environ, "TEND_TOKEN": TOKEN, "CODEX_HOME": str(root / "codex")}
        with server(args.server, root, env, web_dir=args.web) as loopback:
            check_loopback(loopback)
            tls_root = root / "tls"
            tls_root.mkdir()
            if not args.web:
                verify(args, tls_root, env, "https://tend-session.invalid")
                return
            subprocess.run([
                "openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-sha256",
                "-days", "1", "-subj", "/CN=tend-fixture", "-keyout", root / "key.pem",
                "-out", root / "cert.pem",
            ], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15)
            with tls_proxy(root, "cert.pem", "key.pem") as (origin, proxy), \
                 tls_proxy(root, "cert.pem", "key.pem") as (other_origin, other_proxy):
                verify(args, tls_root, env, origin, (proxy, other_proxy, other_origin, loopback))


def verify(args, root, env, origin, browser=None):
    with server(args.server, root, env, web_dir=args.web, web_origin=origin) as address:
        remaining = check_https(args, root, address, origin, {**env, "TEND_URL": f"http://{address}"})
    with server(args.server, root, env, web_dir=args.web, web_origin=origin) as address:
        check(204, call(address, "GET", headers=remaining))
    with server(args.server, root, {**env, "TEND_TOKEN": ROTATED}, web_dir=args.web,
                web_origin=origin) as address:
        check(401, call(address, "GET", headers=remaining))
        check(401, login(address, web_headers(origin), True))
        check(204, login(address, web_headers(origin), True, token=ROTATED))
        check(401, call(address, "GET", CHAT, {"Authorization": f"Bearer {TOKEN}"}))
        check(200, call(address, "GET", CHAT, {"Authorization": f"Bearer {ROTATED}"}))
    with server(args.server, root, env, web_dir=args.web, web_origin=origin) as address:
        check(401, call(address, "GET", headers=remaining))
        print("PASS restart restores remembered sessions; rotation and rollback never resurrect them")
        if browser:
            proxy, other_proxy, other_origin, loopback = browser
            proxy.upstream = other_proxy.upstream = address
            browser_env = {**env, "TEND_WEB_TEST_ORIGIN": origin,
                           "TEND_WEB_TEST_OTHER_ORIGIN": other_origin,
                           "TEND_WEB_TEST_LOOPBACK": f"http://{loopback}"}
            if args.screenshots:
                args.screenshots.mkdir(parents=True, exist_ok=True)
                browser_env["TEND_SCREENSHOTS"] = str(args.screenshots.resolve())
            subprocess.run(["node", WEB / "tests" / "web-session.mjs"], env=browser_env,
                           check=True, timeout=110)


if __name__ == "__main__":
    main()
