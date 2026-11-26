use crate::{config::Config, execution, store::Store, workspace};
use anyhow::{Result, anyhow};
use codex_app::Turn;
use serde_json::json;
use std::{env, net::SocketAddr, sync::Arc, thread};
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
        for request in server.incoming_requests() {
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
    let Some((thread, turn, app_server)) = store.native_turn(run)? else {
        return Ok(None);
    };
    let mut app = execution::client(runtime, &env::current_dir()?, app_server.as_ref())?;
    let found = app.find_turn(&thread, Some(&turn), run)?;
    app.stop()?;
    Ok(found)
}
