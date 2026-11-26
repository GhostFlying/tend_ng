use crate::{
    config::{AppServer, Config},
    execution,
    store::Store,
    workspace,
};
use anyhow::{Result, anyhow};
use codex_app::Turn;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{Value, json};
use std::{env, io::Read, net::SocketAddr, path::Path, sync::Arc, thread};
use tiny_http::{Header, Method, Request, Response, Server};

pub fn serve(
    listen: SocketAddr,
    store: Store,
    runtime: execution::Runtime,
    config: Arc<Config>,
    auth: String,
) -> Result<()> {
    let server = Server::http(listen).map_err(|error| anyhow!("{error}"))?;
    thread::spawn(move || {
        let runner = &config.runner_id;
        let history = crate::host_data_root(&runtime.root, &config.host_id).join("history");
        let own_index = history.join(format!("{runner}.sqlite"));
        for mut request in server.incoming_requests() {
            let (status, body) = if !authenticated(&request, &auth) {
                (401, json!({"error": "valid Tend token required"}))
            } else if let Some((issue, task, assignment)) =
                protocol::discussion_workspace_ids(request.url())
            {
                if request.method() != &Method::Get {
                    (405, json!({"error": "method not allowed"}))
                } else {
                    let result = store
                        .discussion_workspace_binding(issue, task, assignment)
                        .and_then(|binding| {
                            let Some((relative, stamp, repos)) = binding else {
                                return Ok(None);
                            };
                            workspace::discussion_mounts(
                                &runtime.root,
                                &relative,
                                &stamp,
                                &repos,
                                &config.repos,
                            )
                            .map(Some)
                        });
                    let (mounts, unavailable) = match result {
                        Ok(Some(mounts)) if !mounts.is_empty() => (mounts, None),
                        Ok(Some(_)) => (Vec::new(), Some("no_registered_mount".to_owned())),
                        Ok(None) => (Vec::new(), Some("workspace_not_recorded".to_owned())),
                        Err(error) => {
                            eprintln!("Inspect workspace for {task}: {error:#}");
                            (
                                Vec::new(),
                                Some("workspace_reclaimed_or_changed".to_owned()),
                            )
                        }
                    };
                    (
                        200,
                        json!(protocol::DiscussionWorkspace {
                            assignment_id: assignment.to_owned(),
                            task_id: task.to_owned(),
                            issue_id: issue.to_owned(),
                            runner_id: runner.clone(),
                            host_id: config.host_id.clone(),
                            mounts,
                            unavailable,
                        }),
                    )
                }
            } else if let Some((origin, run)) = protocol::retired_rollout_ids(request.url()) {
                if request.method() != &Method::Post {
                    (405, json!({"error": "method not allowed"}))
                } else {
                    let origin = origin.to_owned();
                    let run = run.to_owned();
                    match serde_json::from_reader::<_, protocol::NativeTurnRef>(
                        request.as_reader().take(65536),
                    ) {
                        Ok(native) => {
                            let index = history.join(format!("{origin}.sqlite"));
                            let result =
                                indexed_launch(&index, &native.thread_id).and_then(|launch| {
                                    match launch {
                                        Some(launch) => {
                                            find_native(&runtime, &native, &run, Some(&launch))
                                        }
                                        None => Ok(None),
                                    }
                                });
                            turn_response(&run, &origin, Some(runner), result)
                        }
                        Err(_) => (400, json!({"error":"Invalid native turn reference"})),
                    }
                }
            } else if let Some(run) = protocol::rollout_id(request.url()) {
                if request.method() != &Method::Get {
                    (405, json!({"error": "method not allowed"}))
                } else {
                    turn_response(
                        run,
                        runner,
                        None,
                        read_turn(&store, &runtime, &own_index, run),
                    )
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

fn turn_response(
    run: &str,
    origin: &str,
    reader: Option<&str>,
    result: Result<Option<Turn>>,
) -> (u16, Value) {
    match result {
        Ok(Some(turn)) => {
            let mut value = json!({"run_id":run,"runner_id":origin,"turn":turn});
            if let Some(reader) = reader {
                value["reader_id"] = json!(reader);
            }
            (200, value)
        }
        Ok(None) => (404, json!({"error":"Original native turn unavailable"})),
        Err(error) => {
            eprintln!("Read native turn {run}: {error:#}");
            (
                503,
                json!({"error":"Original App Server history is unavailable"}),
            )
        }
    }
}

fn authenticated(request: &Request, auth: &str) -> bool {
    request
        .headers()
        .iter()
        .any(|header| header.field.equiv("Authorization") && header.value.as_str() == auth)
}

fn read_turn(
    store: &Store,
    runtime: &execution::Runtime,
    own_index: &Path,
    run: &str,
) -> Result<Option<Turn>> {
    let Some((thread, turn, app_server)) = store.native_turn(run)? else {
        return Ok(None);
    };
    let app_server = if own_index.try_exists()? {
        indexed_launch(own_index, &thread)?.or(app_server)
    } else {
        app_server
    };
    find_native(
        runtime,
        &protocol::NativeTurnRef {
            thread_id: thread,
            turn_id: turn,
        },
        run,
        app_server.as_ref(),
    )
}

fn indexed_launch(index: &Path, thread: &str) -> Result<Option<AppServer>> {
    let db = Connection::open_with_flags(index, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let launch: Option<String> = db
        .query_row(
            "SELECT p.launch FROM threads t JOIN profiles p ON p.id = t.profile_id WHERE t.id = ?1",
            [thread],
            |row| row.get(0),
        )
        .optional()?;
    Ok(launch.as_deref().map(serde_json::from_str).transpose()?)
}

fn find_native(
    runtime: &execution::Runtime,
    native: &protocol::NativeTurnRef,
    run: &str,
    app_server: Option<&AppServer>,
) -> Result<Option<Turn>> {
    let mut app = execution::client(runtime, &env::current_dir()?, app_server)?;
    let found = app.find_turn(&native.thread_id, Some(&native.turn_id), run)?;
    app.stop()?;
    Ok(found)
}
