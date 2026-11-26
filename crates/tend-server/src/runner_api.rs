use crate::{api_error, json_response, request_bytes, store::Store};
use anyhow::Result;
use protocol::{RUNNER_PROTOCOL, RUNNER_SYNC_BATCH, RunnerSync, valid_id};
use serde_json::Value;
use std::{
    ops::RangeInclusive,
    path::Path,
    sync::{
        Arc, Mutex,
        mpsc::{self, SyncSender},
    },
    thread,
};
use tiny_http::{Method, Request};

pub(crate) fn compatible_protocols(server_protocol: u32) -> RangeInclusive<u32> {
    server_protocol.saturating_sub(1).max(1)..=server_protocol
}

pub struct Endpoint {
    runner: String,
    task: Option<String>,
}

impl Endpoint {
    pub fn parse(url: &str) -> Option<Self> {
        let path = url.split('?').next()?.strip_prefix("/api/v1/runners/")?;
        if let Some(runner) = path.strip_suffix("/sync").filter(|id| valid_id(id)) {
            return Some(Self {
                runner: runner.to_owned(),
                task: None,
            });
        }
        let (runner, task) = path.split_once("/reconcile/")?;
        (valid_id(runner) && valid_id(task)).then(|| Self {
            runner: runner.to_owned(),
            task: Some(task.to_owned()),
        })
    }
}

pub fn serve(db: &Path) -> Result<SyncSender<(Request, Endpoint)>> {
    let (sender, receiver) = mpsc::sync_channel::<(Request, Endpoint)>(8);
    let receiver = Arc::new(Mutex::new(receiver));
    let root = db.with_extension("artifacts");
    for _ in 0..2 {
        let mut store = Store::open(db)?;
        let receiver = Arc::clone(&receiver);
        let root = root.clone();
        thread::spawn(move || {
            loop {
                let job = receiver.lock().expect("Runner sync request queue").recv();
                let Ok((mut request, endpoint)) = job else {
                    return;
                };
                let (status, body) = handle(&mut store, &mut request, endpoint, &root)
                    .unwrap_or_else(|error| {
                        eprintln!("Runner sync request: {error:#}");
                        api_error(500, "Server could not process Runner sync")
                    });
                if let Ok(response) = json_response(status, &body) {
                    let _ = request.respond(response);
                }
            }
        });
    }
    Ok(sender)
}

fn handle(
    store: &mut Store,
    request: &mut Request,
    endpoint: Endpoint,
    artifact_root: &Path,
) -> Result<(u16, Value)> {
    let runner_id = &endpoint.runner;
    if request.method() != &Method::Post {
        return Ok(api_error(405, "method not allowed"));
    }
    let Some(bytes) = request_bytes(request)? else {
        return Ok(api_error(413, "Runner sync too large"));
    };
    let Ok(sync) = serde_json::from_slice::<RunnerSync>(&bytes) else {
        return Ok(api_error(400, "invalid Runner sync"));
    };
    if !compatible_protocols(RUNNER_PROTOCOL).contains(&sync.protocol) {
        return Ok(api_error(426, "unsupported Runner protocol"));
    }
    if sync.profile.runner_id != *runner_id
        || !sync.profile.valid()
        || sync.receipts.len() > RUNNER_SYNC_BATCH
        || sync.runs.len() > RUNNER_SYNC_BATCH
        || sync.records.len() > RUNNER_SYNC_BATCH
        || sync.reclaim_receipts.len() > RUNNER_SYNC_BATCH
    {
        return Ok(api_error(400, "invalid Runner profile or receipt batch"));
    }
    if let Some(task) = &endpoint.task {
        if !store.reconcile_scope(task, &sync)? {
            return Ok(api_error(
                409,
                "Task is not registered for these Runner records",
            ));
        }
        store.reconcile_artifacts(artifact_root, task, runner_id, sync.repair_host_artifacts)?;
    }
    Ok(match store.sync_runner(sync, endpoint.task.as_deref())? {
        Some(reply) => (200, serde_json::to_value(reply)?),
        None => api_error(
            409,
            "Runner identity, assignment receipt, or Run observation conflicts with saved facts",
        ),
    })
}
