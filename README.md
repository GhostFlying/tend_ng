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
  "workspace_root": "/path/to/runner/workspaces-root",
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

To name the main-session Router, add `--router-name <name>` to the Tend Server startup arguments before `--app-server`. The agreed name for Tend is `--router-name Tend`. The Server records that name on new main-session dispatches after it starts with the flag. If unset, the UI shows only main-session provenance. The setting does not change Router execution, the `router` profile ID, other Chats or earlier dispatches.

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

Register `inspect.listen` and `inspect.url` on a Runner to open its native Run history from the Server. `inspect.url` is the HTTP base address that the Server can reach.

## Runner 常规升级（默认同机新旧代交接）

先让 Server 使用支持共享接替预算和退役文件确认的版本（Runner 注册行含 `host_data_policy: 1`），再把目标 [main-build](https://github.com/GhostFlying/tend_ng/releases/tag/main-build) 归档解压到本机版本目录。保留归档的 `bin/` 和 `REVISION`，不要覆盖前驱正在使用的文件。用与前驱相同的 Linux 用户、PID 命名空间和服务环境运行以下入口（需要 Python 3.9+）。保持前驱运行。若由服务管理器启动，确保它不会在前驱正常退出后重启旧代。

若前驱用 systemd 管理，升级前用 `systemctl edit <旧 unit>` 加以下 drop-in，再执行 `systemctl daemon-reload`。用户级 unit 在两条命令中都加 `--user`。不要因此重启正在运行的前驱。旧 unit 的 `Restart=always` 会在 drain 正常退出后再次拉起旧代。旧用户 unit 退出后，升级脚本会核验其配置与库确属前驱，再移出自动启动并清理该 unit。系统级或自定义 supervisor 的启动规则由部署方维护。

```ini
[Service]
Restart=on-failure
```

```sh
export TEND_URL=http://127.0.0.1:8832
# 在现有服务环境中加载 TEND_TOKEN、CODEX_HOME 和 launcher 所需的环境变量。
/path/to/extracted-main-build/bin/tend-runner-upgrade \
  --config /path/to/current/runner.json --db /path/to/current/runner.sqlite
```

如果前驱配置了 `inspect`，同一命令加上 `--inspect-listen <后继监听地址>` 和 `--inspect-url <Server 可达的后继 HTTP 地址>`，两者都不要占用前驱端点。脚本从前驱派生配置，保留 Host、repo、Agent、工作目录和启动参数，使用独立 ID 与 SQLite 库。以前未配 `workspace_root` 的 Runner 沿用前驱实际使用的数据库旁 `.runs` 目录。

脚本先核对前驱数据库的持久 Runner/Host 身份，再启动 standby、登记接替。两代重叠时共用前驱原有的 slot 预算，同 Host 其他 Runner 的额度不变。已启动的 Run 由前驱自然执行到停妥，未完成的 Task、待处理输入和 workspace 交给后继。确认前驱 drain 已入账、原进程已退出且后继在线后，脚本登记退役。没有待续任务时不人为派发验证任务。

前驱库旁的 `<原库文件名>.upgrade/` 保存目标 REVISION、接替状态和 `runner.log`，后继配置和库以新 ID 命名并放在前驱库同一目录。失败或中断后，按提示排查并用同一归档和前驱路径重跑；不要删除这些文件或手改生成的配置。后继启动失败且尚未登记接替时，可以用新参数修正 inspection 端点。等待默认一小时，活跃 Run 尚未停妥时可用 `--timeout <秒>` 调整。命令不会停止任一 Runner，已登记的接替会继续生效。

成功回执给出后继配置、数据库、启动目录和记录启动参数的本机状态文件。后继配置和数据库是下一次常规升级的输入。主机重启或同一 Runner 故障恢复时，用该代的原二进制、原参数、同一 Runner ID 和数据库启动。原位重启只用于这种本机恢复。收到旧代最终同步确认后，入口把原件、历史读取索引迁入 Host 共享存储，登记退役，复查原件与回执，再回收该代配置、SQLite 和已迁移的私有文件。Server 保留原 Runner 身份和 Run 归属。已退役的 Runner 从 Web 常用目录移出，CLI 的 `tend runner list` 仍可查看全部代际，`tend runner retirement <id>` 可查看确认时间和原件状态。

### 从旧的每代私有目录迁入

每个 Host 使用同一个 `workspace_root`，各代在 `.tend-host/<host_id>/artifacts/<file_id>` 保留不可变交付，在 `history/<origin_runner>.sqlite`（同一 Host 数据目录内）按原 thread 记录只读 launcher 和 Codex home。原生 rollout 仍在原 Codex home。共享仓库、workspace、Codex home、升级日志和独立备份均保留。版本目录可能同时供其他服务使用，独立管理。

正在运行的旧格式前驱可直接走常规升级。需要原位保留同一 Runner ID、排队或等待中的 Task 时，在确认无运行中 Run 并停妥该实例后先执行 `adopt`，再用新二进制启动原实例。它也能在隔离的关停快照上演练。已经退役的旧格式实例使用 `prepare` 和 `cleanup`，其中 `--successor` 填当时登记的直接后继。两种入口均要给出**原服务**使用的默认 Codex home 和自定义 App Server 命令（如有）。使用升级辅助脚本的常规路径会从原进程保存这些设置。

```sh
# 使用当前主机的原始配置。避免从运行中的 SQLite 直接复制单个 db 文件。
storage=/path/to/main-build/bin/tend-runner-retire-files
"$storage" adopt --db "$RUNNER_DB" --config "$RUNNER_CONFIG" --runner "$RUNNER_ID" \
  --host "$TEND_HOST_ID" --workspace-root "$WORKSPACE_ROOT" --codex-home "$ORIGINAL_CODEX_HOME"

# 对已退役的旧实例，在两个步骤中传入相同的参数和私有 receipt 路径。
for action in prepare cleanup; do
  "$storage" "$action" --db "$OLD_DB" --config "$OLD_CONFIG" --runner "$OLD_ID" \
    --host "$TEND_HOST_ID" --successor "$IMMEDIATE_SUCCESSOR_ID" --workspace-root "$WORKSPACE_ROOT" \
    --receipt "$UPGRADE_STATE/retirement.json" --codex-home "$ORIGINAL_CODEX_HOME" || break
done
```

自定义启动器在参数末尾加 `--app-server <program> <arg> ...`。已退出且确由前驱独占的用户 unit，可在该选项前加 `--unit <name>.service`。补传尚未完成时，运行中的后继会从已发布的共享目录继续补传，随后用相同路径重试。准备及清理都需要 Server 可达，单机 `adopt` 保留原库和旧副本。

## Web sign-in over HTTPS

To use Web remotely or enable **Remember this browser (30 days)**, put a trusted TLS endpoint in front of the Server. Give the Web instance a dedicated hostname and configure its exact public origin. For example:

```sh
target/release/tend-server --db .local/server/tend.sqlite \
  --listen 127.0.0.1:8832 --router-cwd .local/server --web-dir web/dist \
  --web-origin https://tend.example.com
```

Terminate TLS with a reverse proxy that preserves the public `Host` header, or use `tend-tsnet --inbound-tls :443=127.0.0.1:8832` with the Web origin set to the node's certificate hostname. Forward to the Server over loopback from the same machine. Web session requests from non-loopback peers are rejected. The Server does not infer HTTPS from forwarded headers. Serve the Web and its API from the same origin. Do not log Authorization, Cookie, or Set-Cookie headers at the proxy.

- Remember is off by default. The session Cookie is shared by new tabs on the same entry, and the Server expires it at 12 hours. Some browsers may restore session Cookies when they restore the previous browser session.
- Newly issued remembered sessions expire after 30 days (`Max-Age=2592000`) and survive closing and reopening a browser with the same profile and cookies. Both lifetimes are absolute and are not extended by use. Existing sessions retain their stored expiry. A Server restart with the same token and database does not expire an otherwise valid session.
- Without `--web-origin`, only local HTTP loopback addresses can sign in to Web, with a nonpersistent Cookie. With it, Web sign-in is accepted only on the configured entry. On a remote HTTP page, Web shows HTTPS guidance without collecting a token. Existing Bearer API clients on trusted links are unchanged.
- Disconnect revokes that browser session and signs out other tabs. To invalidate all existing Web sessions, rotate `TEND_TOKEN` and start the Server with the new value. Update the CLI and Runner token when rotating as before.

See the [Web session contract](docs/web-session.md) for API and verification details.

## Ordinary report history

`tend report backfill <old-report-id> <new-report-id> [operation-id] [--old-message <message-id>] [--new-message <message-id>]` explicitly links two ordinary reports already published in the same Issue's original Chat. Supply a publication message ID for each report without a saved mapping. Keep the operation ID for a retry. Check the result with `tend issue read '#123'` without changing Chat or the Issue's execution state. See the [API, idempotency and read-only verification commands](docs/plans/report-version-backfill.md#implemented-contract).

## Token usage

Open `/token-usage` from Resources on desktop or More on mobile. The page shows the system-wide usage observed for Issue Runs (including their native child Agents), the Main Chat Router, Discussion and handoff turns, and Feishu assessments. The source rows show what was observed, not what the provider billed. Browse top-level Issues or expand immediate children to compare direct and subtree values. The Token usage section on Issue detail remains available.

```sh
tend token-usage overview
tend token-usage overview --parent '#123'
tend token-usage overview --before <next_before>
tend issue token-usage '#123'
```

Use `next_before` with the same parent when paging through children. These reads use saved projections and never start historical collection or rebuild. See the [system-wide Agent usage contract](docs/plans/system-agent-token-usage.md).

### Historical maintenance

With the existing `TEND_URL` and `TEND_TOKEN`, start with a narrow backfill and inspect its coverage and audit. Use `--all` explicitly to scan every verified Started Run and Server attempt with retained native evidence. The receipt's `server` section reports status by source and gap. Feishu assessments stay ephemeral and have no rollout to backfill.

```sh
tend token-usage backfill --issue '#123' --job usage-123
tend token-usage status usage-123
tend token-usage audit usage-123
tend token-usage rebuild --issue '#123'
```

Backfill runs asynchronously where the original Codex rollout is stored. It fills missing request facts and quarantines disagreements by default. Missing or unavailable sources never authorize deletion or imply zero. Rebuild uses stored request facts without reading Rollout files. See the [historical token usage contract](docs/plans/token-usage-backfill.md) for the source, conflict policies, selectors and operational limits.

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

Add `--direct-owner` to check that the live Router chooses a single top-level Task from a natural-language request and that its owner works in the assigned repository and returns the original file to Chat.

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

`host_id` 必须与同机 Runner 登记的官方 Host ID 一致。仓库映射取 Server 本机实际 checkout，只有同一 repo ID 已登记才可附加和读取。主 Chat 继续使用自己的原配置。讨论需要 Linux 和支持自定义原生只读 profile 与 `runtimeWorkspaceRoots` 的 Codex App Server（以 0.172.0 的受限沙箱验证），使用独立 Codex home（不放在 `/tmp`）和空白工作目录。该 home 仅配置讨论服务的模型认证，不复制主 Router 的配置、插件或 MCP。可在讨论配置内用 `model`、`effort` 指定模型，或用 `model_api_keys` 把模型服务所需的 `*_API_KEY` 环境名称映射到独立 Codex home 内的密钥文件；不从 Server 环境继承。资料只读权限由实际 App Server 确认。同机 Task worktree 还需要 Runner 的已有 `inspect` 配置。

```sh
tend discussion create "整理想法"
tend discussion attach "$DISCUSSION_ID" repo tend_ng
tend chat send "$DISCUSSION_ID" "先梳理问题和可选方案"
tend discussion handoff-now "$DISCUSSION_ID"
tend discussion handoff-status "$DISCUSSION_ID"
# If failed, retry using the recorded operation ID and failed generation.
tend discussion handoff-retry "$DISCUSSION_ID" "$OPERATION_ID" "$FAILED_GENERATION"
```

Web 通过独立的「+ 新建讨论」按钮创建讨论，空讨论显示「未命名讨论」，未手动命名时使用首条真实用户输入的完整空白归一化标题，界面只在渲染时截断。迁移 42 回填仍可识别为默认的旧标题，旧 Server 无法读取升级后的数据库。归档只改变列表归属，仍可使用原链接查看消息和引用，也可从「已归档」列表打开原讨论并恢复。CLI 使用同一接口：

```sh
tend discussion list
tend discussion archive "$DISCUSSION_ID"
tend discussion list --archived
tend discussion restore "$DISCUSSION_ID"
```

Web 的「转交主 Chat」和 CLI `tend discussion handoff-now` 会用原讨论上下文写出下一步请求、最终结论或待决事项，再投递到主 Chat。重复请求使用同一个操作 ID，失败不发送，可查证后重试。主 Chat Router 根据可查证的讨论原话行动：已明确授权的工作正常派发，只讨论或待决定的事项不启动实施。生成正文、引用和点击不新增授权，转交也不能代答 Attention。可从「预览 / 编辑」手动发送，CLI 保留 `draft`、`draft-read` 和 `handoff`，另有 `draft-manual` 不依赖模型。手动发送丢失响应时使用相同操作 ID 和原 JSON 重试。 [讨论 API 与迁移边界](docs/chat-routing.md#独立讨论与显式转交)

### 飞书建议（可选 Server 配置）

给 Server 传入 `--feishu-suggestions-config <json-file>` 才会启用读取。Server 运行用户需能执行正式 `lark-cli` 并已有飞书用户 OAuth 授权。用 `lark-cli whoami` 核对 profile 与 `onBehalfOf.openId`，填写应用范围内的用户身份。不开通 Bot 授权、不把令牌放进 Tend 或配置文件。

```json
{
  "owner_open_id": "ou_example",
  "profile": "your-lark-cli-profile",
  "agent": {
    "id": "FeishuTriage",
    "home": "./feishu-assessor-home",
    "cwd": "./empty-feishu-assessor-cwd"
  }
}
```

`profile` 可省略，`cli` 默认 `lark-cli`（需在 Server 的 PATH 上）。采集子进程用 GNU `timeout` 限时，使用 UTC 解析 CLI 文本时间。省略 `agent` 时仍采集并显示来源，不生成模型研判。配置具名研判时，`home` 是独立用户目录（默认 `codex-ark` 从其中的 `.codex-ark` 读取已准备好的 Ark 配置），`cwd` 是单独的空目录。可选覆盖 `program`、`model`、`reasoning_effort`，默认值分别为 `codex-ark`、`deepseek-v4.1-flash`、`high`。`timeout_seconds` 默认 120 秒，限制整个本机研判进程。扫描及研判不使用 Runner，显式快转沿已有 Discussion 配置与权限。用 `tend feishu-suggestions --help` 查 CLI，用 `tend feishu-suggestions list` 查最后一次成功扫描与来源缺口。
