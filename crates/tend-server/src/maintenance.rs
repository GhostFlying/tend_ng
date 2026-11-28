use crate::{
    api_error, json_response, request_bytes,
    store::{
        Store,
        discussion::Change,
        maintenance::{Destination, TaskCursors},
    },
};
use anyhow::Result;
use protocol::{MAINTENANCE_SUGGESTIONS_PATH as ROOT, valid_id};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::mpsc::{self, SyncSender},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tiny_http::{Method, Request};

type Reply = (u16, Value);

pub fn matches(path: &str) -> bool {
    path == ROOT || path.strip_prefix(ROOT).is_some_and(|p| p.starts_with('/'))
}

pub fn serve(db: &Path, feishu_owner: Option<String>) -> Result<SyncSender<Request>> {
    let mut observer = Store::open(db)?;
    let mut store = Store::open(db)?;
    store.enable_foreground_writes()?;
    let feishu_configured = feishu_owner.is_some();
    thread::spawn(move || {
        let mut cursors = TaskCursors::default();
        let mut next_scan = None;
        loop {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            if next_scan.is_none_or(|at| now >= at) {
                if let Err(error) = observer.maintenance_probe_scan(feishu_owner.as_deref(), now) {
                    eprintln!("Maintenance Feishu observation failed: {error:#}");
                }
                next_scan = Some(now + 15);
            }
            let delay = match observer.maintenance_probe_tasks(&mut cursors, now) {
                Ok(true) => 1,
                Ok(false) => 15,
                Err(error) => {
                    eprintln!("Maintenance Task observation failed: {error:#}");
                    15
                }
            };
            thread::sleep(Duration::from_secs(delay));
        }
    });
    let (tx, rx) = mpsc::sync_channel::<Request>(16);
    thread::spawn(move || {
        for mut request in rx {
            let (status, body) = handle(&mut store, feishu_configured, &mut request)
                .unwrap_or_else(|error| {
                    eprintln!("Maintenance suggestions request failed: {error:#}");
                    api_error(500, "Maintenance suggestions could not process the request")
                });
            if let Ok(response) = json_response(status, &body) {
                let _ = request.respond(response);
            }
        }
    });
    Ok(tx)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ignore {
    revision: i64,
    ignored: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Discuss {
    revision: i64,
    chat_id: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Handoff {
    revision: i64,
}

fn handle(store: &mut Store, configured: bool, request: &mut Request) -> Result<Reply> {
    let (path, query) = request.url().split_once('?').unwrap_or((request.url(), ""));
    let method = request.method().clone();
    if path == ROOT {
        if method != Method::Get {
            return Ok(api_error(405, "method not allowed"));
        }
        let query: std::collections::HashMap<_, _> =
            url::form_urlencoded::parse(query.as_bytes()).collect();
        let view = query.get("view").map_or("active", |value| value.as_ref());
        if !matches!(view, "active" | "history" | "ignored") {
            return Ok(api_error(400, "Invalid maintenance list view"));
        }
        let before = match query.get("before") {
            Some(raw) => match raw.parse::<i64>() {
                Ok(value) if value > 0 => value,
                _ => return Ok(api_error(400, "Invalid maintenance page cursor")),
            },
            None => i64::MAX,
        };
        return Ok((200, store.maintenance_list(view, before, configured)?));
    }
    let Some((id, action)) = path
        .strip_prefix(ROOT)
        .and_then(|p| p.strip_prefix('/'))
        .map(|p| p.split_once('/').unwrap_or((p, "")))
        .filter(|(id, _)| valid_id(id))
    else {
        return Ok(api_error(404, "Maintenance suggestion not found"));
    };
    if method == Method::Get && action.is_empty() {
        return Ok(store.maintenance_detail(id)?.map_or_else(
            || api_error(404, "Maintenance suggestion not found"),
            |detail| (200, detail),
        ));
    }
    if method != Method::Post || !matches!(action, "ignore" | "discussions" | "handoff") {
        return Ok(api_error(405, "method not allowed"));
    }
    let (id, action) = (id.to_owned(), action.to_owned());
    let Some(bytes) = request_bytes(request)? else {
        return Ok(api_error(413, "Maintenance request too large"));
    };
    let result = match action.as_str() {
        "ignore" => match serde_json::from_slice::<Ignore>(&bytes) {
            Ok(input) if input.revision > 0 => {
                return Ok(change(
                    store.maintenance_ignore(&id, input.revision, input.ignored)?,
                    |_| json!({"ignored":input.ignored}),
                ));
            }
            _ => Change::Invalid,
        },
        "discussions" => match serde_json::from_slice::<Discuss>(&bytes) {
            Ok(input)
                if input.revision > 0
                    && input
                        .chat_id
                        .as_deref()
                        .is_none_or(|id| id != "main" && valid_id(id)) =>
            {
                let target = input
                    .chat_id
                    .as_deref()
                    .map_or(Destination::New, Destination::Existing);
                store.maintenance_share(&id, input.revision, target)?
            }
            _ => Change::Invalid,
        },
        "handoff" => match serde_json::from_slice::<Handoff>(&bytes) {
            Ok(input) if input.revision > 0 => {
                store.maintenance_share(&id, input.revision, Destination::Main)?
            }
            _ => Change::Invalid,
        },
        _ => unreachable!(),
    };
    Ok(change(result, |value| value))
}

fn change<T>(result: Change<T>, body: impl FnOnce(T) -> Value) -> Reply {
    match result {
        Change::Ok(receipt) => (200, body(receipt)),
        Change::Missing => api_error(404, "Maintenance suggestion or discussion not found"),
        Change::Conflict => api_error(409, "Maintenance evidence revision changed"),
        Change::Invalid => api_error(400, "Invalid maintenance action"),
    }
}
