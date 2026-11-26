use crate::{api_error, json_response, request_bytes, store::Store};
use anyhow::Result;
use protocol::{StartTokenBackfill, TOKEN_BACKFILL_PATH, TOKEN_REBUILD_PATH, TokenUsageScope};
use serde_json::Value;
use std::{
    path::Path,
    sync::mpsc::{self, SyncSender},
    thread,
};
use tiny_http::{Method, Request};

pub fn matches(path: &str) -> bool {
    path == TOKEN_REBUILD_PATH
        || path == TOKEN_BACKFILL_PATH
        || path
            .strip_prefix(TOKEN_BACKFILL_PATH)
            .is_some_and(|tail| tail.starts_with('/'))
}

pub fn serve(db: &Path) -> Result<SyncSender<Request>> {
    let mut store = Store::open(db)?;
    let (sender, queue) = mpsc::sync_channel::<Request>(8);
    thread::spawn(move || {
        for mut request in queue {
            let (status, body) = handle(&mut store, &mut request).unwrap_or_else(|error| {
                eprintln!("Token history API: {error:#}");
                api_error(500, "Server could not process token history request")
            });
            if let Ok(response) = json_response(status, &body) {
                let _ = request.respond(response);
            }
        }
    });
    Ok(sender)
}

fn handle(store: &mut Store, request: &mut Request) -> Result<(u16, Value)> {
    let (path, query) = request.url().split_once('?').unwrap_or((request.url(), ""));
    if path == TOKEN_REBUILD_PATH || path == TOKEN_BACKFILL_PATH {
        let rebuild = path == TOKEN_REBUILD_PATH;
        if request.method() != &Method::Post {
            return Ok(api_error(405, "method not allowed"));
        }
        let Some(bytes) = request_bytes(request)? else {
            return Ok(api_error(413, "request too large"));
        };
        if rebuild {
            let Ok(scope) = serde_json::from_slice::<TokenUsageScope>(&bytes) else {
                return Ok(api_error(400, "a valid Token usage scope is required"));
            };
            if !scope.valid() {
                return Ok(api_error(400, "a valid Token usage scope is required"));
            }
            return Ok((200, store.rebuild_token_usage(&scope)?));
        }
        let Ok(job) = serde_json::from_slice::<StartTokenBackfill>(&bytes) else {
            return Ok(api_error(
                400,
                "a valid backfill job and scope are required",
            ));
        };
        if !protocol::valid_id(&job.job_id) || !job.scope.valid() {
            return Ok(api_error(
                400,
                "a valid backfill job and scope are required",
            ));
        }
        return Ok(match store.start_token_backfill(&job)? {
            Some(receipt) => (202, receipt),
            None => api_error(
                409,
                "backfill job ID already has a different scope or policy",
            ),
        });
    }
    let tail = path
        .strip_prefix(TOKEN_BACKFILL_PATH)
        .and_then(|tail| tail.strip_prefix('/'));
    let Some((job, audit)) = tail.and_then(|tail| {
        let (job, audit) = tail
            .strip_suffix("/audit")
            .map_or((tail, false), |job| (job, true));
        protocol::valid_id(job).then_some((job, audit))
    }) else {
        return Ok(api_error(404, "endpoint not found"));
    };
    if request.method() != &Method::Get {
        return Ok(api_error(405, "method not allowed"));
    }
    let after = if query.is_empty() {
        Some(0)
    } else {
        query
            .strip_prefix("after=")
            .and_then(|after| after.parse::<i64>().ok())
            .filter(|after| *after >= 0)
    };
    let Some(after) = after else {
        return Ok(api_error(400, "invalid after cursor"));
    };
    let result = if audit {
        store.token_backfill_audit(job, after)?
    } else {
        store.token_backfill_status(job, after)?
    };
    Ok(result.map_or_else(
        || api_error(404, "backfill job not found"),
        |receipt| (200, receipt),
    ))
}
