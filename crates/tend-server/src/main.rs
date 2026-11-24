mod router;
mod store;

use anyhow::{Context, Result, anyhow, bail, ensure};
use protocol::{ApiError, DEFAULT_LISTEN, SendMessage, TOKEN_ENV};
use serde_json::{Value, json};
use std::{env, ffi::OsString, io::Read, path::PathBuf};
use store::{Accepted, Store};
use tiny_http::{Header, Method, Request, Response, Server};

const USAGE: &str = "Usage: tend-server --db <sqlite-file> [--listen <addr>] [--router-cwd <dir>] [--app-server <program> <args>...]\n\nTEND_TOKEN authenticates users of the API.";
const MAX_REQUEST_BYTES: u64 = 256 * 1024;

fn main() -> Result<()> {
    let mut args = env::args_os().skip(1);
    let mut db = None;
    let mut listen = DEFAULT_LISTEN.to_owned();
    let mut cwd = env::current_dir()?;
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
    router::spawn(router::Config {
        db,
        cwd,
        program,
        args: app_args,
    });
    eprintln!("Tend Server listening on {}", server.server_addr());

    for mut request in server.incoming_requests() {
        let (status, body) = handle(&mut store, &mut request, &auth).unwrap_or_else(|error| {
            eprintln!("User API failed: {error:#}");
            api_error(500, "Server could not process the request")
        });
        let response = Response::from_string(serde_json::to_string(&body)?)
            .with_status_code(status)
            .with_header(Header::from_bytes("Content-Type", "application/json").unwrap());
        if let Err(error) = request.respond(response) {
            eprintln!("Send user API response: {error}");
        }
    }
    Ok(())
}

fn handle(store: &mut Store, request: &mut Request, auth: &str) -> Result<(u16, Value)> {
    let authenticated = request
        .headers()
        .iter()
        .any(|header| header.field.equiv("Authorization") && header.value.as_str() == auth);
    if !authenticated {
        return Ok(api_error(401, "valid Tend token required"));
    }

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
    let chat_id = path
        .strip_prefix("/api/v1/chats/")
        .and_then(|path| path.strip_suffix("/messages"))
        .filter(|id| protocol::valid_id(id));
    let Some(chat_id) = chat_id else {
        return Ok(api_error(404, "endpoint not found"));
    };

    match request.method() {
        Method::Get => {
            let before = if query.is_empty() {
                i64::MAX
            } else {
                let Some(before) = query
                    .strip_prefix("before=")
                    .and_then(|before| before.parse::<i64>().ok())
                    .filter(|before| *before > 0)
                else {
                    return Ok(api_error(400, "invalid message cursor"));
                };
                before
            };
            Ok((200, serde_json::to_value(store.messages(chat_id, before)?)?))
        }
        Method::Post => {
            let chat_id = chat_id.to_owned();
            let mut bytes = Vec::new();
            request
                .as_reader()
                .take(MAX_REQUEST_BYTES + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() as u64 > MAX_REQUEST_BYTES {
                return Ok(api_error(413, "message request too large"));
            }
            let Ok(message) = serde_json::from_slice::<SendMessage>(&bytes) else {
                return Ok(api_error(400, "expected operation_id and text"));
            };
            if !protocol::valid_id(&message.operation_id) || message.text.trim().is_empty() {
                return Ok(api_error(
                    400,
                    "valid operation ID and message text required",
                ));
            }
            match store.accept(&chat_id, &message)? {
                Accepted::Received(receipt) => Ok((200, serde_json::to_value(receipt)?)),
                Accepted::Conflict => Ok(api_error(
                    409,
                    "operation ID is already used for a different message",
                )),
            }
        }
        _ => Ok(api_error(405, "method not allowed")),
    }
}

fn api_error(status: u16, error: &str) -> (u16, Value) {
    (
        status,
        json!(ApiError {
            error: error.to_owned()
        }),
    )
}
