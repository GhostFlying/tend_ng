# Tend NG

Tend is a single-user assistant for work across repositories. Send work in Chat, receive results in the original conversation, and answer decisions when the responsible Agent needs you. The Router assigns work to Issue Agents, who coordinate Work and Review Agents. The Issue board provides a read-only view of work state, and each Issue has a lightweight discussion with its owner.

Tend runs a Server and one or more independent Runners. A Runner that has accepted work can finish it while the Server is unreachable. Codex App Server executes the Agents, and Tend exchanges complete messages, execution records, and delivery files over the trusted deployment network.

## Build

Open the repository in the devcontainer for Rust, Go, Node, and pnpm. For actual Agent runs, install Codex CLI and make its authenticated App Server available to the Server and Runner service users. The Server service user also needs GitHub CLI read access to the union of repositories whose PR and CI facts Tend should observe. Each Runner needs Git access to the repositories it registers.

```sh
cargo build --release --locked
pnpm --dir web install --frozen-lockfile
pnpm --dir web build
```

## Start

Use a local state directory for the Server database and Router working directory. Set the same private `TEND_TOKEN` in the Server, Runner, and CLI environments. Start with loopback, or bind the Server to an address on the trusted network.

```sh
mkdir -p .local/server .local/runner
target/release/tend-server --db .local/server/tend.sqlite \
  --router-cwd .local/server --web-dir web/dist
```

In a second shell, create `.local/runner/config.json`. Replace the example repository path with a local clone that has an `origin` remote.

```json
{
  "runner_id": "development-runner",
  "host_id": "development-host",
  "slot_limit": 2,
  "repos": [
    { "id": "example", "path": "/path/to/local/clone", "default_branch": "main" }
  ],
  "agents": [
    { "id": "coordinator", "category": "issue-router", "revision": "1", "reasoning_effort": "low" },
    { "id": "implementation", "category": "implementation", "revision": "1" },
    { "id": "economy", "category": "implementation-economy", "revision": "1", "reasoning_effort": "low" },
    { "id": "investigator", "category": "investigation", "revision": "1" },
    { "id": "reviewer", "category": "review", "revision": "1" }
  ]
}
```

Each Agent may set `model`, `reasoning_effort`, `description`, and verified capabilities such as `image_gen`. Configure the Router independently with `--router-model` and `--router-effort` when needed. Omitted model settings use the local Codex configuration.

Set `CODEX_HOME` in the service environment to use a dedicated default Codex home. To run selected Agents with a different home, name a local launcher in `app_servers` and set `app_server` on those Agents. This Runner configuration fragment selects an Ark home for the coordinator.

```json
{
  "app_servers": {
    "ark": {
      "program": "/path/to/codex",
      "codex_home": "/path/to/tend-ark-home"
    }
  },
  "agents": [
    {
      "id": "coordinator",
      "category": "issue-router",
      "revision": "1",
      "model": "deepseek-v4.1-flash",
      "reasoning_effort": "max",
      "app_server": "ark"
    }
  ]
}
```

The Runner stores the selected launcher with each accepted assignment and uses it to start work, reconcile native turns, and read rollout history. Update the Agent revision when changing its launcher. `args` can override the launcher's default `app-server --listen stdio://` arguments. Keep each home available for as long as its history is needed.

```sh
export TEND_URL=http://127.0.0.1:8832
target/release/tend-runner --config .local/runner/config.json --db .local/runner/tend.sqlite
```

Open [Tend on loopback](http://127.0.0.1:8832/) and enter the token once to start a Web session. The browser does not store the shared token. Use the CLI with the same environment as before.

```sh
target/release/tend runner list
target/release/tend chat send main 'Read the example repository and return a short quickstart file.'
target/release/tend chat read main
target/release/tend --help
```

Register `inspect.listen` and `inspect.url` on a Runner to open its native Run history from the Server. `inspect.url` is the HTTP base address that the Server can reach. For parallel replacement on the same Host, give both Runners the same absolute `workspace_root` and `host_id`, plus distinct Runner IDs and database files.

## Web sign-in over HTTPS

To use Web remotely or enable **Remember this browser (7 days)**, put a trusted TLS endpoint in front of the Server. Give the Web instance a dedicated hostname and configure its exact public origin. For example:

```sh
target/release/tend-server --db .local/server/tend.sqlite \
  --listen 127.0.0.1:8832 --router-cwd .local/server --web-dir web/dist \
  --web-origin https://tend.example.com
```

Terminate TLS with a reverse proxy that preserves the public `Host` header, or use `tend-tsnet --inbound-tls :443=127.0.0.1:8832` with the Web origin set to the node's certificate hostname. Forward to the Server over loopback from the same machine. Web session requests from non-loopback peers are rejected. The Server does not infer HTTPS from forwarded headers. Serve the Web and its API from the same origin. Do not log Authorization, Cookie, or Set-Cookie headers at the proxy.

- Remember is off by default. The session Cookie is shared by new tabs on the same entry, and the Server expires it at 12 hours. Some browsers may restore session Cookies when they restore the previous browser session.
- Remembered sessions expire after 7 days and survive closing and reopening a browser with the same profile and cookies. Both lifetimes are absolute and are not extended by use. A Server restart with the same token and database does not expire an otherwise valid session.
- Without `--web-origin`, only local HTTP loopback addresses can sign in to Web, with a nonpersistent Cookie. With it, Web sign-in is accepted only on the configured entry. On a remote HTTP page, Web shows HTTPS guidance without collecting a token. Existing Bearer API clients on trusted links are unchanged.
- Disconnect revokes that browser session and signs out other tabs. To invalidate all existing Web sessions, rotate `TEND_TOKEN` and start the Server with the new value. Update the CLI and Runner token when rotating as before.

See the [Web session contract](docs/web-session.md) for API and verification details.

## Ordinary report history

`tend report backfill <old-report-id> <new-report-id> [operation-id] [--old-message <message-id>] [--new-message <message-id>]` explicitly links two ordinary reports already published in the same Issue's original Chat. Supply a publication message ID for each report without a saved mapping. Keep the operation ID for a retry. Check the result with `tend issue read '#123'` without changing Chat or the Issue's execution state. See the [API, idempotency and read-only verification commands](docs/plans/report-version-backfill.md#implemented-contract).

## Transport and verification

Use a LAN address, the host Tailscale daemon, or the separately built tsnet process. These transports carry the same Tend HTTP API and token. Build the latter from its own Go module.

```sh
cd transport/tsnet
go build -o ../../target/tend-tsnet .
../../target/tend-tsnet --help
```

Run the native workflow smoke using a dedicated authenticated Codex home outside your personal Codex directory. The fixture starts temporary Server and Runner instances on loopback, creates a local Git repository, and checks delivery and Attention in Chat. Add `--image` to exercise native image generation.

```sh
PYTHONDONTWRITEBYTECODE=1 python3 tests/e2e/live_codex.py \
  target/release/tend-server target/release/tend target/release/tend-runner \
  --codex-home .local/tend-smoke-codex
```

Set `TEND_SQLITE_TIMING=1` on the Server to report lock acquisition, transaction, and commit times for Chat receipts, Attention answers, and Runner writes.

The [Server and Runner contract](docs/server-runner.md) covers reconnects, original Run history, and same-host replacement. See the [phase-one acceptance log](docs/plans/phase-one-closeout.md) for fixture entry points, native Codex checks, and SQLite diagnostics. The [product design](docs/design.md) and [engineering guide](docs/engineering.md) describe the implementation boundaries.

### 独立讨论

Web 的「讨论」与 `tend discussion --help` 使用相同用户 API。要启用讨论答复，在 Server 加 `--discussion-config /path/to/discussion.json`（放在 `--app-server` **之前**）。未配置时仍可创建讨论和整理引用，发言会显示讨论执行未配置，绝不回退到主 Router。配置示例：

```json
{
  "host_id": "devbox",
  "program": "/path/to/standalone/codex",
  "codex_home": "/path/to/discussion-codex-home",
  "cwd": "/path/to/empty-discussion-cwd",
  "repos": {
    "tend_ng": "/path/to/existing/tend_ng",
    "lynx": "/path/to/existing/lynx"
  }
}
```

`host_id` 必须与同机 Runner 登记的官方 Host ID 一致。仓库映射取 Server 本机实际 checkout，只有同一 repo ID 已登记才可附加和读取。主 Chat 继续使用自己的原配置。讨论需要 Linux 和支持自定义原生只读权限的 Codex App Server（以 0.172.0 的受限沙箱验证），使用独立 Codex home（不放在 `/tmp`）和空白工作目录。该 home 仅配置讨论服务的模型认证，不复制主 Router 的配置、插件或 MCP。可在讨论配置内用 `model`、`effort` 指定模型，或用 `model_api_keys` 把模型服务所需的 `*_API_KEY` 环境名称映射到独立 Codex home 内的密钥文件；不从 Server 环境继承。资料只读权限由实际 App Server 确认。同机 Task worktree 还需要 Runner 的已有 `inspect` 配置。

```sh
tend discussion create "整理想法"
tend discussion attach "$DISCUSSION_ID" repo tend_ng
tend chat send "$DISCUSSION_ID" "先梳理问题和可选方案"
tend discussion draft "$DISCUSSION_ID"
tend discussion draft-read "$DISCUSSION_ID"
```

CLI 显式发送使用 `tend discussion handoff`，先按帮助审阅目标、约束和授权终点。丢失响应时用相同的操作 ID 和原 JSON 重试。
