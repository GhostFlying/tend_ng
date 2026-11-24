use crate::{execution, store::Store};
use anyhow::{Result, anyhow};
use codex_app::Turn;
use serde_json::json;
use std::{env, net::SocketAddr, thread};
use tiny_http::{Header, Method, Request, Response, Server};

pub fn serve(
    listen: SocketAddr,
    store: Store,
    runtime: execution::Runtime,
    runner: String,
    auth: String,
) -> Result<()> {
    let server = Server::http(listen).map_err(|error| anyhow!("{error}"))?;
    thread::spawn(move || {
        for request in server.incoming_requests() {
            let (status, body) = if !authenticated(&request, &auth) {
                (401, json!({"error": "valid Tend token required"}))
            } else if let Some(run) = protocol::rollout_id(request.url()) {
                if request.method() != &Method::Get {
                    (405, json!({"error": "method not allowed"}))
                } else {
                    match read_turn(&store, &runtime, run) {
                        Ok(Some(turn)) => (
                            200,
                            json!({"run_id": run, "runner_id": runner, "turn": turn}),
                        ),
                        Ok(None) => (404, json!({"error": "Original native turn unavailable"})),
                        Err(error) => {
                            eprintln!("Read native turn {run}: {error:#}");
                            (
                                503,
                                json!({"error": "Original App Server history is unavailable"}),
                            )
                        }
                    }
                }
            } else {
                (404, json!({"error": "endpoint not found"}))
            };
            let response = Response::from_string(body.to_string())
                .with_status_code(status)
                .with_header(Header::from_bytes("Content-Type", "application/json").unwrap());
            let _ = request.respond(response);
        }
    });
    Ok(())
}

fn authenticated(request: &Request, auth: &str) -> bool {
    request
        .headers()
        .iter()
        .any(|header| header.field.equiv("Authorization") && header.value.as_str() == auth)
}

fn read_turn(store: &Store, runtime: &execution::Runtime, run: &str) -> Result<Option<Turn>> {
    let Some((thread, turn)) = store.native_turn(run)? else {
        return Ok(None);
    };
    let mut app = execution::client(runtime, &env::current_dir()?)?;
    let found = app.find_turn(&thread, Some(&turn), run)?;
    app.stop()?;
    Ok(found)
}
