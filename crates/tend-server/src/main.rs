mod artifacts;
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

const USAGE: &str = "Usage: tend-server --db <sqlite-file> [--listen <addr>] [--router-cwd <dir>] [--web-dir <dist-dir>] [--app-server <program> <args>...]\n\nTEND_TOKEN authenticates users of the API.";
const MAX_REQUEST_BYTES: u64 = 256 * 1024;

fn main() -> Result<()> {
    let mut args = env::args_os().skip(1);
    let mut db = None;
    let mut listen = DEFAULT_LISTEN.to_owned();
    let mut cwd = env::current_dir()?;
    let mut web_dir = None;
    let mut program = OsString::from("codex");
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
    if sync.protocol != RUNNER_PROTOCOL {
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
