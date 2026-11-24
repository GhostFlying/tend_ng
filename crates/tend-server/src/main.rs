mod artifacts;
mod github;
mod inspect;
mod router;
mod store;
mod web;

use anyhow::{Context, Result, anyhow, bail, ensure};
use protocol::{
    ApiError, DEFAULT_LISTEN, RUNNER_PROTOCOL, RUNNER_SYNC_BATCH, RunnerSync, SendMessage,
    TOKEN_ENV,
};
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

const USAGE: &str = "Usage: tend-server --db <sqlite-file> [--listen <addr>] [--router-cwd <dir>] [--web-dir <dist-dir>] [--github-cli <program>] [--app-server <program> <args>...]\n\nTEND_TOKEN authenticates users of the API.";
const MAX_REQUEST_BYTES: u64 = 256 * 1024;

fn main() -> Result<()> {
    let mut args = env::args_os().skip(1);
    let mut db = None;
    let mut listen = DEFAULT_LISTEN.to_owned();
    let mut cwd = env::current_dir()?;
    let mut web_dir = None;
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
    let artifact_root = db.with_extension("artifacts");
    thread::spawn(move || schedule(placement));
    github::spawn(Store::open(&db)?, github_cli.clone());
    github::spawn_discovery(Store::open(&db)?, github_cli);
    router::spawn(router::Config {
        db,
        cwd,
        program,
        args: app_args,
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
        } else {
            handle(&mut store, &mut request, &artifact_root).unwrap_or_else(|error| {
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

fn handle(store: &mut Store, request: &mut Request, artifact_root: &Path) -> Result<(u16, Value)> {
    let (path, query) = request.url().split_once('?').unwrap_or((request.url(), ""));
    if path == "/health" && request.method() == &Method::Get {
        return Ok((200, json!({ "status": "ok" })));
    }
    if path == "/api/v1/runners" && request.method() == &Method::Get {
        return Ok((200, serde_json::to_value(store.runner_instances()?)?));
    }
    if let Some(version) = path
        .strip_prefix("/api/v1/upgrade-check/")
        .and_then(|version| version.parse::<u32>().ok())
        .filter(|version| *version > 0)
        && request.method() == &Method::Get
    {
        let blockers = store.upgrade_blockers(version)?;
        return Ok((
            200,
            json!({"target_protocol":version,"allowed":blockers.is_empty(),"blockers":blockers}),
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
    if let Some(chat_id) = path
        .strip_prefix("/api/v1/chats/")
        .and_then(|path| path.strip_suffix("/issues"))
        .filter(|id| protocol::valid_id(id))
    {
        if request.method() != &Method::Get {
            return Ok(api_error(405, "method not allowed"));
        }
        let Some(before) = page_cursor(query) else {
            return Ok(api_error(400, "invalid Issue cursor"));
        };
        return Ok((
            200,
            serde_json::to_value(store.issue_board(chat_id, before)?)?,
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
        .filter(|id| protocol::valid_id(id))
    {
        if request.method() != &Method::Get {
            return Ok(api_error(405, "method not allowed"));
        }
        return Ok(match store.issue(issue_id)? {
            Some(issue) => (200, serde_json::to_value(issue)?),
            None => api_error(404, "issue not found"),
        });
    }
    if let Some(runner_id) = path
        .strip_prefix("/api/v1/runners/")
        .and_then(|path| path.strip_suffix("/sync"))
        .filter(|id| protocol::valid_id(id))
    {
        let runner_id = runner_id.to_owned();
        return handle_runner_sync(store, request, &runner_id, None);
    }
    if let Some((runner_id, task)) = path
        .strip_prefix("/api/v1/runners/")
        .and_then(|path| path.split_once("/reconcile/"))
        .filter(|(runner, task)| protocol::valid_id(runner) && protocol::valid_id(task))
    {
        let scope = (task.to_owned(), artifact_root);
        let runner_id = runner_id.to_owned();
        return handle_runner_sync(store, request, &runner_id, Some(scope));
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

fn handle_runner_sync(
    store: &mut Store,
    request: &mut Request,
    runner_id: &str,
    reconcile: Option<(String, &Path)>,
) -> Result<(u16, Value)> {
    if request.method() != &Method::Post {
        return Ok(api_error(405, "method not allowed"));
    }
    let Some(bytes) = request_bytes(request)? else {
        return Ok(api_error(413, "Runner sync too large"));
    };
    let Ok(sync) = serde_json::from_slice::<RunnerSync>(&bytes) else {
        return Ok(api_error(400, "invalid Runner sync"));
    };
    if sync.protocol == 0
        || sync.protocol > RUNNER_PROTOCOL
        || sync.protocol < RUNNER_PROTOCOL.saturating_sub(1)
    {
        return Ok(api_error(426, "unsupported Runner protocol"));
    }
    if sync.profile.runner_id != runner_id
        || !sync.profile.valid()
        || sync.receipts.len() > RUNNER_SYNC_BATCH
        || sync.runs.len() > RUNNER_SYNC_BATCH
        || sync.records.len() > RUNNER_SYNC_BATCH
    {
        return Ok(api_error(400, "invalid Runner profile or receipt batch"));
    }
    if let Some((task, root)) = &reconcile {
        if !store.reconcile_scope(task, &sync)? {
            return Ok(api_error(
                409,
                "Task is not registered for these Runner records",
            ));
        }
        store.reconcile_artifacts(root, task, runner_id)?;
    }
    Ok(
        match store.sync_runner(sync, reconcile.as_ref().map(|(task, _)| task.as_str()))? {
            Some(reply) => (200, serde_json::to_value(reply)?),
            None => api_error(
                409,
                "Runner identity, assignment receipt, or Run observation conflicts with saved facts",
            ),
        },
    )
}

fn api_error(status: u16, error: &str) -> (u16, Value) {
    (
        status,
        json!(ApiError {
            error: error.to_owned()
        }),
    )
}
