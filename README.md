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

```sh
export TEND_URL=http://127.0.0.1:8832
target/release/tend-runner --config .local/runner/config.json --db .local/runner/tend.sqlite
```

Open [Tend on loopback](http://127.0.0.1:8832/) and enter the token, or use the CLI with the same environment.

```sh
target/release/tend runner list
target/release/tend chat send main 'Read the example repository and return a short quickstart file.'
target/release/tend chat read main
target/release/tend --help
```

Register `inspect.listen` and `inspect.url` on a Runner to open its native Run history from the Server. `inspect.url` is the HTTP base address that the Server can reach. For parallel replacement on the same Host, give both Runners the same absolute `workspace_root` and `host_id`, plus distinct Runner IDs and database files.

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
