use crate::{
    api_error, json_response, request_bytes,
    store::{Store, discussion::Change},
};
use anyhow::Result;
use protocol::{
    FEISHU_SUGGESTIONS_PATH, FeishuDiscussionRequest, FeishuHandoffRequest, FeishuIgnoreRequest,
    valid_id,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::mpsc::{self, SyncSender, TrySendError},
    thread,
};
use tiny_http::{Method, Request};

type Reply = (u16, Value);

pub fn serve(
    db: &Path,
    owner: Option<String>,
    assessor: Option<String>,
    scan: Option<SyncSender<()>>,
) -> Result<SyncSender<Request>> {
    let (tx, rx) = mpsc::sync_channel::<Request>(16);
    let mut store = Store::open(db)?;
    store.enable_foreground_writes()?;
    thread::spawn(move || {
        for mut request in rx {
            let (status, body) = handle(
                &mut store,
                owner.as_deref(),
                assessor.as_deref(),
                scan.as_ref(),
                &mut request,
            )
            .unwrap_or_else(|_| {
                eprintln!("Feishu suggestions API failed");
                api_error(500, "Feishu suggestions could not process the request")
            });
            if let Ok(response) = json_response(status, &body) {
                let _ = request.respond(response);
            }
        }
    });
    Ok(tx)
}

fn handle(
    store: &mut Store,
    owner: Option<&str>,
    agent: Option<&str>,
    scan: Option<&SyncSender<()>>,
    request: &mut Request,
) -> Result<Reply> {
    let (path, query) = request.url().split_once('?').unwrap_or((request.url(), ""));
    let query: std::collections::HashMap<_, _> = url::form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect();
    let method = request.method().clone();
    let rest = path
        .strip_prefix(FEISHU_SUGGESTIONS_PATH)
        .unwrap_or_default()
        .to_owned();
    if rest.is_empty() && method == Method::Get {
        let before = if let Some(before) = query.get("before") {
            let Some((at, id)) = before
                .split_once(':')
                .and_then(|(at, id)| Some((at.parse::<i64>().ok()?, id)))
                .filter(|(_, id)| valid_id(id))
            else {
                return Ok(api_error(400, "Invalid Feishu page cursor"));
            };
            Some((at, id))
        } else {
            None
        };
        let mut body = match owner {
            Some(owner) => store.feishu_list(owner, before)?,
            None => json!({"suggestions":[],"next_before":null}),
        };
        body["scan"] = store.feishu_status(owner)?;
        body["assessor"] = agent.map_or(
            Value::Null,
            |id| json!({"id":id,"category":super::agent::category()}),
        );
        return Ok((200, body));
    }
    if rest == "/sync" && method == Method::Post {
        let Some(scan) = scan else {
            return Ok(api_error(409, "Feishu scanning is not configured"));
        };
        return Ok(match scan.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) => (202, json!({"requested":true})),
            Err(TrySendError::Disconnected(())) => api_error(503, "Feishu scanner is unavailable"),
        });
    }
    let Some(owner) = owner else {
        return Ok(api_error(404, "Feishu suggestions are not configured"));
    };
    let Some((id, action)) = rest
        .strip_prefix('/')
        .map(|rest| rest.split_once('/').unwrap_or((rest, "")))
        .filter(|(id, _)| valid_id(id))
    else {
        return Ok(api_error(404, "Feishu suggestion not found"));
    };
    match (action, method) {
        ("", Method::Get) => Ok(store.feishu_detail(owner, id)?.map_or_else(
            || api_error(404, "Feishu suggestion not found"),
            |item| (200, item),
        )),
        ("ignore", Method::Post) => {
            let input = match input::<FeishuIgnoreRequest>(request)? {
                Ok(input) => input,
                Err(reply) => return Ok(reply),
            };
            if input.revision <= 0 {
                return Ok(invalid());
            }
            Ok(change(
                store.feishu_ignore(owner, id, input.revision, input.ignored)?,
                |()| json!({"ignored":input.ignored}),
            ))
        }
        ("discussions", Method::Post) => {
            let input = match input::<FeishuDiscussionRequest>(request)? {
                Ok(input) => input,
                Err(reply) => return Ok(reply),
            };
            if input.revision <= 0 || input.chat_id.as_deref().is_some_and(|id| !valid_id(id)) {
                return Ok(invalid());
            }
            Ok(change(
                store.feishu_discuss(owner, id, input.revision, input.chat_id.as_deref())?,
                |chat| json!({"chat_id":chat}),
            ))
        }
        ("handoff", Method::Post) => {
            let input = match input::<FeishuHandoffRequest>(request)? {
                Ok(input) => input,
                Err(reply) => return Ok(reply),
            };
            if input.revision <= 0 || input.retry_generation.is_some_and(|n| n <= 0) {
                return Ok(invalid());
            }
            Ok(change(
                store.feishu_handoff(owner, id, input.revision, input.retry_generation)?,
                |receipt| json!(receipt),
            ))
        }
        ("handoff", Method::Get) => {
            let Some(revision) = query
                .get("revision")
                .and_then(|n| n.parse::<i64>().ok())
                .filter(|n| *n > 0)
            else {
                return Ok(invalid());
            };
            Ok((
                200,
                json!(store.feishu_handoff_receipt(owner, id, revision)?),
            ))
        }
        ("" | "ignore" | "discussions" | "handoff", _) => Ok(api_error(405, "Method not allowed")),
        _ => Ok(api_error(404, "Feishu suggestion endpoint not found")),
    }
}

fn invalid() -> Reply {
    api_error(
        400,
        "A valid Feishu suggestion revision and target are required",
    )
}

fn input<T: DeserializeOwned>(request: &mut Request) -> Result<std::result::Result<T, Reply>> {
    let Some(bytes) = request_bytes(request)? else {
        return Ok(Err(api_error(413, "Request too large")));
    };
    Ok(serde_json::from_slice(&bytes).map_err(|_| api_error(400, "Invalid Feishu action")))
}

fn change<T>(value: Change<T>, body: impl FnOnce(T) -> Value) -> Reply {
    match value {
        Change::Ok(value) => (200, body(value)),
        Change::Missing => api_error(404, "Feishu suggestion or discussion not found"),
        Change::Conflict => api_error(
            409,
            "Feishu suggestion changed. Read the current revision or original receipt.",
        ),
        Change::Invalid => invalid(),
    }
}
