mod artifacts;
mod codebase;
mod codebase_comments;
mod discussion;
mod github;
mod inspect;
mod pull_worker;
mod router;
mod runner_api;
mod store;
mod token_usage_api;
mod web;
mod web_session;

use anyhow::{Context, Result, anyhow, bail, ensure};
use protocol::{ApiError, DEFAULT_LISTEN, SendMessage, TOKEN_ENV};
use serde_json::{Value, json};
use std::{
    env,
    ffi::OsString,
    io::{Cursor, Read},
    path::{Path, PathBuf},
    sync::mpsc::TrySendError,
    thread,
    time::Duration,
};
use store::{Accepted, Store};
use tiny_http::{Header, Method, Request, Response, Server};

const USAGE: &str = "Usage: tend-server --db <sqlite-file> [--listen <addr>] [--router-cwd <dir>] [--router-name <name>] [--router-model <model>] [--router-effort <effort>] [--web-dir <dist-dir>] [--web-origin <https-origin>] [--github-cli <program>] [--codebase-cli <program>] [--discussion-config <json-file>] [--app-server <program> <args>...]\n\nTEND_TOKEN authenticates users of the API.";
const MAX_REQUEST_BYTES: u64 = protocol::MAX_REQUEST_BYTES as u64;

fn main() -> Result<()> {
    let mut args = env::args_os().skip(1);
    let mut db = None;
    let mut listen = DEFAULT_LISTEN.to_owned();
    let mut cwd = env::current_dir()?;
    let mut web_dir = None;
    let mut web_origin = None;
    let mut router_effort = None;
    let mut router_name = None;
    let mut discussion_config = None;
    let mut router_model = None;
    let mut program = OsString::from("codex");
    let mut github_cli = OsString::from("gh");
    let mut codebase_cli = OsString::from("bytedcli");
    let mut app_args = ["app-server", "--listen", "stdio://"]
        .map(OsString::from)
        .to_vec();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--db") => db = Some(PathBuf::from(args.next().context("--db needs a path")?)),
            Some("--listen") => {
                listen = args
                    .next()
                    .context("--listen needs an address")?
                    .into_string()
                    .map_err(|_| anyhow!("invalid listen address"))?;
            }
            Some("--router-cwd") => {
                cwd = PathBuf::from(args.next().context("--router-cwd needs a directory")?)
            }
            Some("--router-effort") => {
                let effort = args
                    .next()
                    .context("--router-effort needs a value")?
                    .into_string()
                    .map_err(|_| anyhow!("invalid Router effort"))?;
                ensure!(!effort.trim().is_empty(), "Router effort must not be empty");
                router_effort = Some(effort);
            }
            Some("--router-model") => {
                let model = args
                    .next()
                    .context("--router-model needs a value")?
                    .into_string()
                    .map_err(|_| anyhow!("invalid Router model"))?;
                ensure!(!model.trim().is_empty(), "Router model must not be empty");
                router_model = Some(model);
            }
            Some("--router-name") => {
                let name = args
                    .next()
                    .context("--router-name needs a value")?
                    .into_string()
                    .map_err(|_| anyhow!("invalid Router name"))?;
                ensure!(!name.trim().is_empty(), "Router name must not be empty");
                router_name = Some(name.trim().to_owned());
            }
            Some("--web-dir") => {
                let directory = PathBuf::from(args.next().context("--web-dir needs a directory")?);
                ensure!(
                    directory.join("index.html").is_file(),
                    "--web-dir needs a built Web app"
                );
                web_dir = Some(directory.canonicalize()?);
            }
            Some("--web-origin") => {
                web_origin = Some(
                    args.next()
                        .context("--web-origin needs an HTTPS origin")?
                        .into_string()
                        .map_err(|_| anyhow!("invalid --web-origin"))?,
                );
            }
            Some("--discussion-config") => {
                discussion_config = Some(PathBuf::from(
                    args.next()
                        .context("--discussion-config needs a JSON file")?,
                ));
            }
            Some("--codebase-cli") => {
                codebase_cli = args.next().context("--codebase-cli needs a program")?
            }
            Some("--github-cli") => {
                github_cli = args.next().context("--github-cli needs a program")?
            }
            Some("--app-server") => {
                program = args.next().context("--app-server needs a program")?;
                app_args = args.collect();
                break;
            }
            Some("--help") => {
                println!("{USAGE}");
                return Ok(());
            }
            _ => bail!("{USAGE}"),
        }
    }
    let db = db.context(USAGE)?;
    let cwd = cwd
        .canonicalize()
        .context("resolve Router working directory")?;
    let token = env::var(TOKEN_ENV).context("set TEND_TOKEN before starting Server")?;
    ensure!(!token.is_empty(), "TEND_TOKEN is empty");
    let auth = format!("Bearer {token}");
    let sessions = web_session::Sessions::new(web_origin.as_deref(), &token)?;
    let mut store = Store::initialize(&db)?;
    store.enable_foreground_writes()?;
    let server = Server::http(&listen).map_err(|error| anyhow!("{error}"))?;
    sessions.prepare(&store)?;
    let placement = Store::open(&db)?;
    let artifact_root = db.with_extension("artifacts");
    let transfers = artifacts::serve(&db)?;
    let inspections = inspect::serve(Store::open(&db)?, auth.clone());
    let synchronization = runner_api::serve(&db, codebase_cli.clone())?;
    let token_usage_maintenance = token_usage_api::serve(&db)?;
    let pull_links = serve_pull_links(&db, codebase_cli.clone())?;
    let discussion = std::sync::Arc::new(discussion::Service::new(
        discussion_config.as_deref(),
        &db,
        github_cli.clone(),
        auth.clone(),
    )?);
    let discussion_api = discussion.serve_api(&db)?;
    thread::spawn(move || schedule(placement));
    pull_worker::spawn(
        Store::open(&db)?,
        protocol::PullProvider::GitHub,
        github_cli.clone(),
    );
    pull_worker::spawn(
        Store::open(&db)?,
        protocol::PullProvider::Codebase,
        codebase_cli.clone(),
    );
    pull_worker::spawn_comments(Store::open(&db)?, codebase_cli.clone());
    pull_worker::spawn_discovery(
        Store::open(&db)?,
        protocol::PullProvider::GitHub,
        github_cli.clone(),
        codebase_cli.clone(),
    );
    pull_worker::spawn_discovery(
        Store::open(&db)?,
        protocol::PullProvider::Codebase,
        github_cli,
        codebase_cli,
    );
    let mut router_profile =
        json!({"id": "router", "model": &router_model, "reasoning_effort": &router_effort});
    if let Some(name) = &router_name {
        router_profile["name"] = json!(name);
    }
    router::spawn(router::Config {
        db,
        cwd,
        program,
        args: app_args,
        effort: router_effort,
        model: router_model,
        name: router_name,
        discussion,
    });
    eprintln!("Tend Server listening on {}", server.server_addr());

    for mut request in server.incoming_requests() {
        if let Some(root) = &web_dir
            && let Some(response) = web::asset(root, &request)
        {
            if let Err(error) = request.respond(response) {
                eprintln!("Send Web asset: {error}");
            }
            continue;
        }
        let session_route = request.url().split('?').next() == Some(web_session::PATH);
        let checked = if session_route {
            sessions.handle(&mut store, &mut request, &auth).map(Some)
        } else {
            sessions.authorize(&store, &request, &auth)
        };
        let response = match checked {
            Ok(response) => response,
            Err(error) => {
                eprintln!("Web session operation failed: {error:#}");
                let (_, body) = api_error(500, "Server could not check the request");
                Some(json_response(500, &body)?.boxed())
            }
        };
        if let Some(response) = response {
            if let Err(error) = request.respond(response) {
                eprintln!("Send authentication response: {error}");
            }
            continue;
        }
        let route = request.url().split('?').next().unwrap_or_default();
        if request.method() == &Method::Post
            && let Some(issue) = route
                .strip_prefix("/api/v1/issues/")
                .and_then(|p| p.strip_suffix("/pulls"))
            && (protocol::valid_id(issue) || pull_issue_number(issue).is_some())
        {
            let issue = issue.to_owned();
            if let Err(
                TrySendError::Full((request, _)) | TrySendError::Disconnected((request, _)),
            ) = pull_links.try_send((request, issue))
            {
                let (_, body) = api_error(503, "Pull association API is busy");
                let _ = request.respond(json_response(503, &body)?);
            }
            continue;
        }
        if token_usage_api::matches(route) {
            if let Err(TrySendError::Full(request) | TrySendError::Disconnected(request)) =
                token_usage_maintenance.try_send(request)
            {
                let (_, body) = api_error(503, "Token history API is busy");
                let _ = request.respond(json_response(503, &body)?);
            }
            continue;
        }
        if route == protocol::DISCUSSIONS_PATH || route.starts_with("/api/v1/discussions/") {
            if let Err(TrySendError::Full(request) | TrySendError::Disconnected(request)) =
                discussion_api.try_send(request)
            {
                let (_, body) = api_error(503, "Discussion API is busy");
                let _ = request.respond(json_response(503, &body)?);
            }
            continue;
        }
        let (status, body) = if let Some(run) = protocol::rollout_id(request.url()) {
            if request.method() != &Method::Get {
                let (_, body) = api_error(405, "method not allowed");
                let _ = request.respond(json_response(405, &body)?);
                continue;
            }
            let run = run.to_owned();
            if let Err(
                TrySendError::Full((request, _)) | TrySendError::Disconnected((request, _)),
            ) = inspections.try_send((request, run))
            {
                let (_, body) = api_error(503, "Rollout reader is busy");
                let _ = request.respond(json_response(503, &body)?);
            }
            continue;
        } else if let Some(endpoint) = artifacts::Endpoint::parse(request.url()) {
            if let Err(
                TrySendError::Full((request, _)) | TrySendError::Disconnected((request, _)),
            ) = transfers.try_send((request, endpoint))
            {
                let (_, body) = api_error(503, "File transfer worker is busy");
                let _ = request.respond(json_response(503, &body)?);
            }
            continue;
        } else if let Some(endpoint) = runner_api::Endpoint::parse(request.url()) {
            if let Err(
                TrySendError::Full((request, _)) | TrySendError::Disconnected((request, _)),
            ) = synchronization.try_send((request, endpoint))
            {
                let (_, body) = api_error(503, "Runner sync workers are busy");
                let _ = request.respond(json_response(503, &body)?);
            }
            continue;
        } else {
            handle(&mut store, &mut request, &router_profile, &artifact_root).unwrap_or_else(
                |error| {
                    eprintln!("User API failed: {error:#}");
                    api_error(500, "Server could not process the request")
                },
            )
        };
        let response = json_response(status, &body)?;
        if let Err(error) = request.respond(response) {
            eprintln!("Send user API response: {error}");
        }
    }
    Ok(())
}

fn schedule(mut store: Store) {
    let mut after = 0;
    loop {
        if let Err(error) = store.release_external() {
            eprintln!("Release external facts: {error:#}");
        }
        if let Err(error) = store.release_codebase_comments() {
            eprintln!("Release Codebase review comments: {error:#}");
        }
        if let Err(error) = store.release_external_routes() {
            eprintln!("Release routed external facts: {error:#}");
        }
        if let Err(error) = store.release_child_windows() {
            eprintln!("Release child update windows: {error:#}");
        }
        if let Err(error) = store.schedule_assignments(&mut after) {
            eprintln!("Schedule Runner assignments: {error:#}");
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn json_response(status: u16, body: &Value) -> Result<Response<Cursor<Vec<u8>>>> {
    Ok(Response::from_string(serde_json::to_string(body)?)
        .with_status_code(status)
        .with_header(Header::from_bytes("Content-Type", "application/json").unwrap())
        .with_header(Header::from_bytes("Cache-Control", "no-store").unwrap()))
}

fn handle(
    store: &mut Store,
    request: &mut Request,
    router: &Value,
    artifact_root: &Path,
) -> Result<(u16, Value)> {
    let (path, query) = request.url().split_once('?').unwrap_or((request.url(), ""));
    let normalized;
    let path = if let Some(rest) = path.strip_prefix("/api/v1/issues/") {
        let reference = rest.split('/').next().unwrap_or_default();
        // A number with a literal '#' is a URL fragment in clients.
        let decoded = reference
            .strip_prefix("%23")
            .map(|digits| format!("#{digits}"));
        let lookup = decoded.as_deref().unwrap_or(reference);
        if protocol::parse_issue_number(lookup).is_some() {
            let Some(id) = store.resolve_issue(lookup)? else {
                return Ok(api_error(404, "Issue not found"));
            };
            normalized = format!("/api/v1/issues/{id}{}", &rest[reference.len()..]);
            normalized.as_str()
        } else {
            path
        }
    } else {
        path
    };
    if path == "/health" && request.method() == &Method::Get {
        return Ok((200, json!({ "status": "ok" })));
    }
    if path == protocol::REPORT_REVISIONS_PATH {
        if request.method() != &Method::Post {
            return Ok(api_error(405, "method not allowed"));
        }
        let Some(bytes) = request_bytes(request)? else {
            return Ok(api_error(413, "request too large"));
        };
        let Ok(input) = serde_json::from_slice::<protocol::BackfillReportRevision>(&bytes) else {
            return Ok(api_error(
                400,
                "expected operation_id, old_report_id and new_report_id",
            ));
        };
        use store::report_revisions::Rejection;
        return match store.backfill_report_revision(&input) {
            Ok(receipt) => Ok((200, serde_json::to_value(receipt)?)),
            Err(error) => match error.downcast::<Rejection>() {
                Ok(Rejection::Invalid(message)) => Ok(api_error(400, message)),
                Ok(Rejection::Missing(message)) => Ok(api_error(404, message)),
                Ok(Rejection::Conflict(message)) => Ok(api_error(409, message)),
                Err(error) => Err(error),
            },
        };
    }
    if path == "/api/v1/agent-candidates" {
        if request.method() != &Method::Post {
            return Ok(api_error(405, "method not allowed"));
        }
        let Some(bytes) = request_bytes(request)? else {
            return Ok(api_error(413, "candidate query too large"));
        };
        let Ok(query) = serde_json::from_slice::<protocol::AgentQuery>(&bytes) else {
            return Ok(api_error(400, "invalid candidate query"));
        };
        if !query.valid() {
            return Ok(api_error(400, "invalid category or task requirements"));
        }
        return Ok((
            200,
            json!({"candidates": store.agent_candidates(&query, false)?}),
        ));
    }
    if let Some(message) = path
        .strip_prefix("/api/v1/messages/")
        .and_then(|path| path.strip_suffix("/required-facts"))
        .filter(|id| protocol::valid_id(id))
    {
        return require_external(store, &message.to_owned(), request);
    }
    if let Some(id) = path
        .strip_prefix("/api/v1/workspace-reclaims/")
        .filter(|id| protocol::valid_id(id))
        && request.method() == &Method::Get
    {
        return Ok(match store.workspace_reclaim(id)? {
            Some(receipt) => (200, receipt),
            None => api_error(404, "workspace reclaim not found"),
        });
    }
    if let Some(issue) = path
        .strip_prefix("/api/v1/issues/")
        .and_then(|path| path.strip_suffix("/reclaim"))
        .filter(|id| protocol::valid_id(id))
    {
        let issue = issue.to_owned();
        if request.method() != &Method::Post {
            return Ok(api_error(405, "method not allowed"));
        }
        let Some(body) = request_bytes(request)? else {
            return Ok(api_error(413, "request too large"));
        };
        let Ok(input) = serde_json::from_slice::<protocol::ControlRequest>(&body) else {
            return Ok(api_error(400, "expected operation ID"));
        };
        if !protocol::valid_id(&input.operation_id) {
            return Ok(api_error(400, "valid operation ID required"));
        }
        return control_response(store.reclaim_workspace(&issue, &input.operation_id)?);
    }
    if path == protocol::EXECUTION_HOSTS_PATH && request.method() == &Method::Get {
        return Ok((200, serde_json::to_value(store.execution_hosts()?)?));
    }
    if path == protocol::TOKEN_OVERVIEW_PATH {
        return token_usage_api::overview(store, request.method(), query);
    }
    if path == "/api/v1/runners" && request.method() == &Method::Get {
        return Ok((200, serde_json::to_value(store.runner_instances()?)?));
    }
    if path == "/api/v1/agents" && request.method() == &Method::Get {
        return Ok((200, store.agent_directory(router)?));
    }
    if let Some(version) = path
        .strip_prefix("/api/v1/upgrade-check/")
        .and_then(|version| version.parse::<u32>().ok())
        .filter(|version| *version > 0)
        && request.method() == &Method::Get
    {
        let compatible = runner_api::compatible_protocols(version);
        let blockers = store.upgrade_blockers(&compatible)?;
        return Ok((
            200,
            json!({
                "target_protocol": version,
                "compatible_range": {"min": compatible.start(), "max": compatible.end()},
                "allowed": blockers.is_empty(),
                "blockers": blockers
            }),
        ));
    }
    if let Some((runner, operation)) = path
        .strip_prefix("/api/v1/runners/")
        .and_then(|path| path.split_once('/'))
        .filter(|(id, operation)| {
            protocol::valid_id(id) && matches!(*operation, "replace" | "retire" | "retirement")
        })
    {
        if operation == "retirement" && request.method() == &Method::Get {
            return Ok(match store.runner_retirement(runner, artifact_root)? {
                Some(proof) => (200, serde_json::to_value(proof)?),
                None => api_error(404, "Runner not found"),
            });
        }
        if operation == "retirement" || request.method() != &Method::Post {
            return Ok(api_error(405, "method not allowed"));
        }
        let runner = runner.to_owned();
        if operation == "retire" {
            return Ok(if store.retire_runner(&runner, artifact_root)? {
                (200, json!({"runner_id": runner, "state": "retired"}))
            } else {
                api_error(
                    409,
                    "Runner requires an acknowledged drain and all original deliveries",
                )
            });
        }
        let Some(bytes) = request_bytes(request)? else {
            return Ok(api_error(413, "request too large"));
        };
        let Ok(replacement) = serde_json::from_slice::<protocol::ReplaceRunner>(&bytes) else {
            return Ok(api_error(400, "expected successor Runner ID"));
        };
        if !protocol::valid_id(&replacement.successor) {
            return Ok(api_error(400, "valid successor Runner ID required"));
        }
        return Ok(
            match store.replace_runner(
                &runner,
                &replacement.successor,
                replacement.predecessor_workspace_root.as_deref(),
            )? {
                Some(receipt) => (200, receipt),
                None => api_error(
                    409,
                    "replacement requires two live Runners on the same Host with the same workspace root, an unused successor and no unfinished incoming replacement",
                ),
            },
        );
    }
    if let Some(rest) = path.strip_prefix("/api/v1/controls/") {
        if let Some(id) = rest
            .strip_suffix("/queue")
            .filter(|id| protocol::valid_id(id))
        {
            if request.method() != &Method::Post {
                return Ok(api_error(405, "method not allowed"));
            }
            return control_response(store.queue_steer(id)?);
        }
        if protocol::valid_id(rest) && request.method() == &Method::Get {
            return Ok(match store.control(rest)? {
                Some(view) => (200, serde_json::to_value(view)?),
                None => api_error(404, "control not found"),
            });
        }
    }
    if let Some((id, stop)) = path
        .strip_prefix("/api/v1/tasks/")
        .and_then(|tail| tail.strip_suffix("/stop"))
        .map(|id| (id, true))
        .or_else(|| {
            path.strip_prefix("/api/v1/runs/")
                .and_then(|tail| tail.strip_suffix("/steer"))
                .map(|id| (id, false))
        })
        .filter(|(id, _)| protocol::valid_id(id))
    {
        let id = id.to_owned();
        return request_control(store, request, &id, stop);
    }
    if let Some(issue) = path
        .strip_prefix("/api/v1/issues/")
        .and_then(|path| path.strip_suffix("/reviews"))
        .filter(|id| protocol::valid_id(id))
    {
        return if request.method() == &Method::Get {
            Ok((200, serde_json::to_value(store.issue_reviews(issue)?)?))
        } else {
            Ok(api_error(405, "method not allowed"))
        };
    }
    if let Some(issue) = path
        .strip_prefix("/api/v1/issues/")
        .and_then(|path| path.strip_suffix("/state"))
        .filter(|id| protocol::valid_id(id))
    {
        let issue = issue.to_owned();
        if request.method() != &Method::Post {
            return Ok(api_error(405, "method not allowed"));
        }
        let Some(bytes) = request_bytes(request)? else {
            return Ok(api_error(413, "request too large"));
        };
        let Ok(input) = serde_json::from_slice::<protocol::IssueTransition>(&bytes) else {
            return Ok(api_error(400, "expected operation_id, state and reason"));
        };
        if !protocol::valid_id(&input.operation_id)
            || !matches!(input.state.as_str(), "open" | "cancelled")
            || input.reason.trim().is_empty()
        {
            return Ok(api_error(
                400,
                "a valid issue transition and reason are required",
            ));
        }
        return control_response(match store.transition_issue(&issue, &input)? {
            store::control::ResultState::Found(()) => store::control::ResultState::Found(input),
            store::control::ResultState::Conflict => store::control::ResultState::Conflict,
            store::control::ResultState::Missing => store::control::ResultState::Missing,
        });
    }
    let issue_chat = path
        .strip_prefix("/api/v1/chats/")
        .and_then(|path| path.strip_suffix("/issues"))
        .filter(|id| protocol::valid_id(id));
    let roots_only = path == protocol::TOP_LEVEL_BOARD_PATH;
    if roots_only || path == protocol::ISSUE_BOARD_PATH || issue_chat.is_some() {
        if request.method() != &Method::Get {
            return Ok(api_error(405, "method not allowed"));
        }
        let before = if query.is_empty() {
            None
        } else {
            let Some(cursor) = query
                .strip_prefix("before=")
                .and_then(protocol::IssueBoardCursor::decode)
            else {
                return Ok(api_error(
                    400,
                    "invalid Issue board cursor; restart at the first page",
                ));
            };
            Some(cursor)
        };
        return Ok((
            200,
            serde_json::to_value(store.issue_board(issue_chat, before.as_ref(), roots_only)?)?,
        ));
    }
    if let Some(issue) = path
        .strip_prefix("/api/v1/issues/")
        .and_then(|path| path.strip_suffix("/watches"))
        .filter(|id| protocol::valid_id(id))
    {
        return if request.method() == &Method::Get {
            Ok((200, serde_json::to_value(store.external_watches(issue)?)?))
        } else {
            Ok(api_error(405, "method not allowed"))
        };
    }
    if let Some(issue_id) = path
        .strip_prefix("/api/v1/issues/")
        .and_then(|path| path.strip_suffix("/pull-discoveries"))
        .filter(|id| protocol::valid_id(id))
    {
        return match request.method() {
            Method::Get => {
                let operation = query
                    .strip_prefix("operation=")
                    .filter(|id| protocol::valid_id(id));
                if !query.is_empty() && operation.is_none() {
                    return Ok(api_error(400, "invalid discovery operation ID"));
                }
                Ok(match store.discoveries(issue_id, operation)? {
                    Some(jobs) => (200, serde_json::to_value(jobs)?),
                    None => api_error(404, "Issue not found"),
                })
            }
            Method::Post => find_pull(store, &issue_id.to_owned(), request),
            _ => Ok(api_error(405, "method not allowed")),
        };
    }
    if let Some(issue_id) = path
        .strip_prefix("/api/v1/issues/")
        .and_then(|path| path.strip_suffix("/token-usage"))
        .filter(|id| protocol::valid_id(id))
    {
        if request.method() != &Method::Get {
            return Ok(api_error(405, "method not allowed"));
        }
        return Ok(match store.issue_token_usage(issue_id)? {
            Some(usage) => (200, serde_json::to_value(usage)?),
            None => api_error(404, "Issue not found"),
        });
    }
    if let Some(issue_id) = path
        .strip_prefix("/api/v1/issues/")
        .and_then(|path| path.strip_suffix("/overview"))
        .filter(|id| protocol::valid_id(id))
    {
        if request.method() != &Method::Get {
            return Ok(api_error(405, "method not allowed"));
        }
        return Ok(match store.issue_overview(issue_id)? {
            Some(issue) => (200, serde_json::to_value(issue)?),
            None => api_error(404, "Issue not found"),
        });
    }
    if let Some((issue, attention)) = path
        .strip_prefix("/api/v1/issues/")
        .and_then(|path| path.strip_suffix("/answer"))
        .and_then(|path| path.split_once("/attentions/"))
        .filter(|(issue, attention)| protocol::valid_id(issue) && protocol::valid_id(attention))
    {
        if request.method() != &Method::Post {
            return Ok(api_error(405, "method not allowed"));
        }
        let (issue, attention) = (issue.to_owned(), attention.to_owned());
        let Some(bytes) = request_bytes(request)? else {
            return Ok(api_error(413, "Attention reply is too large"));
        };
        let Ok(input) = serde_json::from_slice::<protocol::AnswerIssueAttention>(&bytes) else {
            return Ok(api_error(400, "expected operation_id and text"));
        };
        if !protocol::valid_id(&input.operation_id) || input.text.trim().is_empty() {
            return Ok(api_error(
                400,
                "a valid operation ID and non-empty reply are required",
            ));
        }
        use store::attention::IssueReply;
        return Ok(
            match store.answer_issue_attention(&issue, &attention, &input)? {
                IssueReply::Recorded(receipt) => (200, serde_json::to_value(receipt)?),
                IssueReply::NotFound => {
                    api_error(404, "ordinary Attention not found on this Issue")
                }
                IssueReply::Conflict(message) => api_error(409, message),
            },
        );
    }
    if let Some(issue) = path
        .strip_prefix("/api/v1/issues/")
        .and_then(|path| path.strip_suffix("/attention-replies"))
        .filter(|id| protocol::valid_id(id))
    {
        if request.method() != &Method::Get {
            return Ok(api_error(405, "method not allowed"));
        }
        let Some(before) = page_cursor(query) else {
            return Ok(api_error(400, "invalid Attention reply cursor"));
        };
        return Ok(match store.issue_attention_history(issue, before)? {
            Some(page) => (200, serde_json::to_value(page)?),
            None => api_error(404, "Issue not found"),
        });
    }
    if let Some(issue_id) = path
        .strip_prefix("/api/v1/issues/")
        .and_then(|path| path.strip_suffix("/pulls"))
        .filter(|id| protocol::valid_id(id))
    {
        return match request.method() {
            Method::Get => Ok(match store.issue_pulls(issue_id)? {
                Some(pulls) => (200, serde_json::to_value(pulls)?),
                None => api_error(404, "Issue not found"),
            }),
            _ => Ok(api_error(405, "method not allowed")),
        };
    }
    if let Some((issue_id, action)) = path
        .strip_prefix("/api/v1/issues/")
        .and_then(|path| path.split_once('/'))
        .filter(|(id, action)| protocol::valid_id(id) && matches!(*action, "start" | "backlog"))
    {
        if request.method() != &Method::Post {
            return Ok(api_error(405, "method not allowed"));
        }
        let (issue_id, action) = (issue_id.to_owned(), action.to_owned());
        let Some(bytes) = request_bytes(request)? else {
            return Ok(api_error(413, "request too large"));
        };
        let input = if bytes.is_empty() {
            protocol::IssueAdmission::default()
        } else if let Ok(input) = serde_json::from_slice::<protocol::IssueAdmission>(&bytes) {
            input
        } else {
            return Ok(api_error(400, "expected an operation ID"));
        };
        if input
            .operation_id
            .as_deref()
            .is_some_and(|id| !protocol::valid_id(id))
        {
            return Ok(api_error(400, "invalid operation ID"));
        }
        let result = if action == "backlog" {
            let Some(operation) = &input.operation_id else {
                return Ok(api_error(400, "a valid operation ID is required"));
            };
            store.return_to_backlog(&issue_id, None, operation)?
        } else {
            store.start_issue(&issue_id, None, input.operation_id.as_deref())?
        };
        use store::control::ResultState;
        return Ok(match result {
            ResultState::Found(receipt) => (200, receipt),
            ResultState::Missing => api_error(404, "Issue not found"),
            ResultState::Conflict => api_error(
                409,
                "Issue admission conflict: verify its state, subtree stop receipts and operation ID",
            ),
        });
    }
    if let Some(issue_id) = path
        .strip_prefix("/api/v1/issues/")
        .filter(|id| protocol::valid_id(id))
    {
        if request.method() != &Method::Get {
            return Ok(api_error(405, "method not allowed"));
        }
        if query == "include=reports" {
            return Ok(match store.issue_with_reports(issue_id)? {
                Some(issue) => (200, issue),
                None => api_error(404, "issue not found"),
            });
        }
        return Ok(match store.issue(issue_id)? {
            Some(issue) => (200, serde_json::to_value(issue)?),
            None => api_error(404, "issue not found"),
        });
    }
    if let Some((chat, attention)) = path
        .strip_prefix("/api/v1/chats/")
        .and_then(|path| path.strip_suffix("/approve"))
        .and_then(|path| path.split_once("/attentions/"))
        .filter(|(chat, id)| protocol::valid_id(chat) && protocol::valid_id(id))
    {
        if request.method() != &Method::Post {
            return Ok(api_error(405, "method not allowed"));
        }
        let (chat, attention) = (chat.to_owned(), attention.to_owned());
        let Ok(input) =
            serde_json::from_reader::<_, protocol::ApproveProposal>(request.as_reader())
        else {
            return Ok(api_error(400, "expected a proposal version"));
        };
        if input.version == 0 {
            return Ok(api_error(400, "invalid proposal version"));
        }
        use store::proposals::Approval;
        return Ok(match store.approve_proposal(&chat, &attention, &input)? {
            Approval::Recorded(decision) => (200, serde_json::to_value(decision)?),
            Approval::NotFound => api_error(404, "structured proposal not found in this Chat"),
            Approval::Conflict => api_error(
                409,
                "proposal changed or is not available for approval; reload it before deciding",
            ),
        });
    }
    if let Some((chat, attention)) = path
        .strip_prefix("/api/v1/chats/")
        .and_then(|path| path.split_once("/attentions/"))
        .filter(|(chat, id)| protocol::valid_id(chat) && protocol::valid_id(id))
    {
        if request.method() != &Method::Get {
            return Ok(api_error(405, "method not allowed"));
        }
        return Ok(match store.attention(chat, attention)? {
            Some(attention) => (200, serde_json::to_value(attention)?),
            None => api_error(404, "Attention not found in this Chat"),
        });
    }
    if let Some(chat_id) = path
        .strip_prefix("/api/v1/chats/")
        .and_then(|path| path.strip_suffix("/attentions"))
        .filter(|id| protocol::valid_id(id))
    {
        if request.method() != &Method::Get {
            return Ok(api_error(405, "method not allowed"));
        }
        return Ok((
            200,
            serde_json::to_value(store.attentions(chat_id, false)?)?,
        ));
    }
    if let Some(chat_id) = path
        .strip_prefix("/api/v1/chats/")
        .and_then(|path| path.strip_suffix("/notifications"))
        .filter(|id| protocol::valid_id(id))
    {
        if request.method() != &Method::Get {
            return Ok(api_error(405, "method not allowed"));
        }
        let Some(before) = page_cursor(query) else {
            return Ok(api_error(400, "invalid notification cursor"));
        };
        return Ok((
            200,
            serde_json::to_value(store.notifications(chat_id, before)?)?,
        ));
    }
    let target = path
        .strip_suffix("/messages")
        .and_then(|path| {
            path.strip_prefix("/api/v1/chats/")
                .map(|id| (id, false))
                .or_else(|| path.strip_prefix("/api/v1/issues/").map(|id| (id, true)))
        })
        .filter(|(id, _)| protocol::valid_id(id));
    let Some((id, issue)) = target else {
        return Ok(api_error(404, "endpoint not found"));
    };

    match request.method() {
        Method::Get => {
            let Some(before) = page_cursor(query) else {
                return Ok(api_error(400, "invalid message cursor"));
            };
            if issue {
                return Ok(match store.issue_messages(id, before)? {
                    Some(page) => (200, serde_json::to_value(page)?),
                    None => api_error(404, "Issue not found"),
                });
            }
            Ok((200, serde_json::to_value(store.messages(id, before)?)?))
        }
        Method::Post => {
            let id = id.to_owned();
            let Some(bytes) = request_bytes(request)? else {
                return Ok(api_error(413, "message request too large"));
            };
            let Ok(message) = serde_json::from_slice::<SendMessage>(&bytes) else {
                return Ok(api_error(400, "expected operation_id and text"));
            };
            if !protocol::valid_id(&message.operation_id) || message.text.trim().is_empty() {
                return Ok(api_error(
                    400,
                    "valid operation ID and message text required",
                ));
            }
            if message
                .reply_to
                .as_ref()
                .is_some_and(|id| !protocol::valid_id(id))
            {
                return Ok(api_error(400, "valid reply message ID required"));
            }
            let accepted = if issue {
                let Some(accepted) = store.post_issue_message(&id, &message)? else {
                    return Ok(api_error(404, "Issue or assigned Task not found"));
                };
                accepted
            } else {
                store.accept(&id, &message)?
            };
            match accepted {
                Accepted::Received(receipt) => Ok((200, serde_json::to_value(receipt)?)),
                Accepted::Conflict => Ok(api_error(
                    409,
                    "operation ID is already used for a different message",
                )),
                Accepted::InvalidReply => Ok(api_error(
                    404,
                    "Quoted message not found in this conversation",
                )),
            }
        }
        _ => Ok(api_error(405, "method not allowed")),
    }
}

fn request_control(
    store: &mut Store,
    request: &mut Request,
    id: &str,
    stop: bool,
) -> Result<(u16, Value)> {
    if request.method() != &Method::Post {
        return Ok(api_error(405, "method not allowed"));
    }
    let Some(bytes) = request_bytes(request)? else {
        return Ok(api_error(413, "request too large"));
    };
    let Ok(input) = serde_json::from_slice::<protocol::ControlRequest>(&bytes) else {
        return Ok(api_error(400, "expected a control operation ID"));
    };
    if !protocol::valid_id(&input.operation_id) {
        return Ok(api_error(400, "valid operation ID required"));
    }
    let action = if stop {
        protocol::ControlAction::Stop
    } else {
        let Some(text) = input.text.filter(|text| !text.trim().is_empty()) else {
            return Ok(api_error(400, "Steer requires complete message text"));
        };
        protocol::ControlAction::Steer { text }
    };
    control_response(store.new_control(&input.operation_id, id, &action)?)
}

fn control_response<T: serde::Serialize>(
    result: store::control::ResultState<T>,
) -> Result<(u16, Value)> {
    use store::control::ResultState;
    Ok(match result {
        ResultState::Found(value) => (200, serde_json::to_value(value)?),
        ResultState::Conflict => api_error(409, "control conflicts with saved intent or outcome"),
        ResultState::Missing => api_error(404, "control target not found"),
    })
}

fn find_pull(store: &mut Store, issue: &str, request: &mut Request) -> Result<(u16, Value)> {
    let Some(bytes) = request_bytes(request)? else {
        return Ok(api_error(413, "pull request discovery request too large"));
    };
    let Ok(find) = serde_json::from_slice::<protocol::FindPull>(&bytes) else {
        return Ok(api_error(400, "expected operation_id and task_id"));
    };
    if !protocol::valid_id(&find.operation_id) || !protocol::valid_id(&find.task_id) {
        return Ok(api_error(400, "valid operation and Task IDs required"));
    }
    Ok(match store.find_pull(issue, &find)? {
        store::discovery::Accepted::Saved => (200, serde_json::to_value(find)?),
        store::discovery::Accepted::Conflict => {
            api_error(409, "operation ID is already used for another discovery")
        }
        store::discovery::Accepted::Missing => api_error(404, "Issue not found"),
    })
}

fn pull_issue_number(issue: &str) -> Option<String> {
    let decoded = issue.strip_prefix("%23").map(|digits| format!("#{digits}"));
    let number = decoded.as_deref().unwrap_or(issue);
    protocol::parse_issue_number(number).map(|_| number.to_owned())
}

fn serve_pull_links(
    db: &Path,
    program: OsString,
) -> Result<std::sync::mpsc::SyncSender<(Request, String)>> {
    use std::sync::{Arc, Mutex, mpsc};
    let (send, receive) = mpsc::sync_channel::<(Request, String)>(8);
    let receive = Arc::new(Mutex::new(receive));
    for _ in 0..2 {
        let mut store = Store::open(db)?;
        let receive = Arc::clone(&receive);
        let program = program.clone();
        thread::spawn(move || {
            loop {
                let job = receive
                    .lock()
                    .expect("Pull association request queue")
                    .recv();
                let Ok((mut request, issue)) = job else {
                    return;
                };
                let (status, body) = link_pull(&mut store, &issue, &mut request, &program)
                    .unwrap_or_else(|error| {
                        eprintln!("Link pull request: {error:#}");
                        api_error(500, "Server could not process pull association")
                    });
                if let Ok(response) = json_response(status, &body) {
                    let _ = request.respond(response);
                }
            }
        });
    }
    Ok(send)
}

fn link_pull(
    store: &mut Store,
    issue: &str,
    request: &mut Request,
    codebase_cli: &std::ffi::OsStr,
) -> Result<(u16, Value)> {
    let resolved;
    let issue = if let Some(number) = pull_issue_number(issue) {
        let Some(id) = store.resolve_issue(&number)? else {
            return Ok(api_error(404, "Issue not found"));
        };
        resolved = id.into_owned();
        &resolved
    } else {
        issue
    };
    let Some(bytes) = request_bytes(request)? else {
        return Ok(api_error(413, "pull request association request too large"));
    };
    let Ok(link) = serde_json::from_slice::<protocol::LinkPull>(&bytes) else {
        return Ok(api_error(400, "expected operation_id and url"));
    };
    if !protocol::valid_id(&link.operation_id) {
        return Ok(api_error(400, "valid operation ID required"));
    }
    let Some(pull) = protocol::PullRef::parse(&link.url) else {
        return Ok(api_error(
            400,
            "valid GitHub PR or Codebase MR URL required",
        ));
    };
    Ok(
        match store.link_pull(issue, &link, &pull, Some(codebase_cli))? {
            store::pull::Linked::Saved(receipt) => (200, serde_json::to_value(receipt)?),
            store::pull::Linked::Conflict => {
                api_error(409, "operation ID is already used for another association")
            }
            store::pull::Linked::Missing => api_error(404, "Issue not found"),
        },
    )
}

fn require_external(
    store: &mut Store,
    message: &str,
    request: &mut Request,
) -> Result<(u16, Value)> {
    use store::external_routing::Registration;
    if request.method() != &Method::Post {
        return Ok(api_error(405, "method not allowed"));
    }
    let Some(bytes) = request_bytes(request)? else {
        return Ok(api_error(413, "external watch request too large"));
    };
    let Ok(mut watch) = serde_json::from_slice::<protocol::RequireExternal>(&bytes) else {
        return Ok(api_error(400, "expected operation_id and condition"));
    };
    if !protocol::valid_id(&watch.operation_id) || !watch.condition.normalize() {
        return Ok(api_error(
            400,
            "valid operation ID and GitHub condition required",
        ));
    }
    Ok(
        match store.require_external(&watch.operation_id, message, &watch.condition)? {
            Registration::Saved(id) => (200, json!({"watch_id": id})),
            Registration::Missing => api_error(404, "original user message not found"),
            Registration::Conflict => api_error(
                409,
                "operation ID is already used for another external watch",
            ),
        },
    )
}

fn page_cursor(query: &str) -> Option<i64> {
    if query.is_empty() {
        Some(i64::MAX)
    } else {
        query
            .strip_prefix("before=")?
            .parse::<i64>()
            .ok()
            .filter(|before| *before > 0)
    }
}

fn request_bytes(request: &mut Request) -> Result<Option<Vec<u8>>> {
    let mut bytes = Vec::new();
    request
        .as_reader()
        .take(MAX_REQUEST_BYTES + 1)
        .read_to_end(&mut bytes)?;
    Ok((bytes.len() as u64 <= MAX_REQUEST_BYTES).then_some(bytes))
}

fn api_error(status: u16, error: &str) -> (u16, Value) {
    (
        status,
        json!(ApiError {
            error: error.to_owned()
        }),
    )
}
