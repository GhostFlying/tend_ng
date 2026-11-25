mod artifacts;
mod github;
mod inspect;
mod router;
mod runner_api;
mod store;
mod web;

use anyhow::{Context, Result, anyhow, bail, ensure};
use protocol::{ApiError, DEFAULT_LISTEN, SendMessage, TOKEN_ENV};
use serde_json::{Value, json};
use std::{
    env,
    ffi::OsString,
    io::{Cursor, Read},
    path::PathBuf,
    sync::mpsc::TrySendError,
    thread,
    time::Duration,
};
use store::{Accepted, Store};
use tiny_http::{Header, Method, Request, Response, Server};

const USAGE: &str = "Usage: tend-server --db <sqlite-file> [--listen <addr>] [--router-cwd <dir>] [--router-model <model>] [--router-effort <effort>] [--web-dir <dist-dir>] [--github-cli <program>] [--app-server <program> <args>...]\n\nTEND_TOKEN authenticates users of the API.";
const MAX_REQUEST_BYTES: u64 = 256 * 1024;

fn main() -> Result<()> {
    let mut args = env::args_os().skip(1);
    let mut db = None;
    let mut listen = DEFAULT_LISTEN.to_owned();
    let mut cwd = env::current_dir()?;
    let mut web_dir = None;
    let mut router_effort = None;
    let mut router_model = None;
    let mut program = OsString::from("codex");
    let mut github_cli = OsString::from("gh");
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
            Some("--web-dir") => {
                let directory = PathBuf::from(args.next().context("--web-dir needs a directory")?);
                ensure!(
                    directory.join("index.html").is_file(),
                    "--web-dir needs a built Web app"
                );
                web_dir = Some(directory.canonicalize()?);
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
    let mut store = Store::initialize(&db)?;
    store.enable_foreground_writes()?;
    let cwd = cwd
        .canonicalize()
        .context("resolve Router working directory")?;
    let token = env::var(TOKEN_ENV).context("set TEND_TOKEN before starting Server")?;
    ensure!(!token.is_empty(), "TEND_TOKEN is empty");
    let auth = format!("Bearer {token}");
    let server = Server::http(&listen).map_err(|error| anyhow!("{error}"))?;
    let placement = Store::open(&db)?;
    let transfers = artifacts::serve(&db)?;
    let inspections = inspect::serve(Store::open(&db)?, auth.clone());
    let synchronization = runner_api::serve(&db)?;
    thread::spawn(move || schedule(placement));
    github::spawn(Store::open(&db)?, github_cli.clone());
    github::spawn_discovery(Store::open(&db)?, github_cli);
    let router_profile =
        json!({"id": "router", "model": &router_model, "reasoning_effort": &router_effort});
    router::spawn(router::Config {
        db,
        cwd,
        program,
        args: app_args,
        effort: router_effort,
        model: router_model,
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
        let (status, body) = if !authenticated(&request, &auth) {
            api_error(401, "valid Tend token required")
        } else if let Some(run) = protocol::rollout_id(request.url()) {
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
            handle(&mut store, &mut request, &router_profile).unwrap_or_else(|error| {
                eprintln!("User API failed: {error:#}");
                api_error(500, "Server could not process the request")
            })
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
        .with_header(Header::from_bytes("Content-Type", "application/json").unwrap()))
}

fn authenticated(request: &Request, auth: &str) -> bool {
    request
        .headers()
        .iter()
        .any(|header| header.field.equiv("Authorization") && header.value.as_str() == auth)
}

fn handle(store: &mut Store, request: &mut Request, router: &Value) -> Result<(u16, Value)> {
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
        return Ok((200, json!({"candidates": store.agent_candidates(&query)?})));
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
            protocol::valid_id(id) && matches!(*operation, "replace" | "retire")
        })
    {
        if request.method() != &Method::Post {
            return Ok(api_error(405, "method not allowed"));
        }
        let runner = runner.to_owned();
        if operation == "retire" {
            return Ok(if store.retire_runner(&runner)? {
                (200, json!({"runner_id": runner, "state": "retired"}))
            } else {
                api_error(409, "Runner has no acknowledged drain")
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
            match store.replace_runner(&runner, &replacement.successor)? {
                Some(receipt) => (200, receipt),
                None => api_error(
                    409,
                    "replacement requires two live Runners on the same Host with the same workspace root",
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
            Method::Post => link_pull(store, &issue_id.to_owned(), request),
            _ => Ok(api_error(405, "method not allowed")),
        };
    }
    if let Some(issue_id) = path
        .strip_prefix("/api/v1/issues/")
        .and_then(|path| path.strip_suffix("/start"))
        .filter(|id| protocol::valid_id(id))
    {
        if request.method() != &Method::Post {
            return Ok(api_error(405, "method not allowed"));
        }
        return Ok(match store.start_recorded_issue(issue_id, None)? {
            Some(issue) => (200, serde_json::to_value(issue)?),
            None => api_error(409, "No startable recorded Issue with that ID"),
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

fn link_pull(store: &mut Store, issue: &str, request: &mut Request) -> Result<(u16, Value)> {
    let Some(bytes) = request_bytes(request)? else {
        return Ok(api_error(413, "pull request association request too large"));
    };
    let Ok(link) = serde_json::from_slice::<protocol::LinkPull>(&bytes) else {
        return Ok(api_error(400, "expected operation_id and url"));
    };
    if !protocol::valid_id(&link.operation_id) {
        return Ok(api_error(400, "valid operation ID required"));
    }
    let Some(pull) = protocol::GitHubPull::parse(&link.url) else {
        return Ok(api_error(400, "valid GitHub pull request URL required"));
    };
    Ok(match store.link_pull(issue, &link, &pull)? {
        store::pull::Linked::Saved(receipt) => (200, serde_json::to_value(receipt)?),
        store::pull::Linked::Conflict => {
            api_error(409, "operation ID is already used for another association")
        }
        store::pull::Linked::Missing => api_error(404, "Issue not found"),
    })
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
