use super::{Service, normalized_pull};
use crate::{
    api_error, json_response, request_bytes,
    store::{
        Store,
        discussion::{Change, valid_draft, valid_title},
    },
};
use anyhow::Result;
use protocol::{
    CreateDiscussion, DISCUSSIONS_PATH, DiscussionReference, DiscussionSend, DiscussionSource,
    valid_id,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        mpsc::{self, SyncSender},
    },
    thread,
};
use tiny_http::{Method, Request};

type Reply = (u16, Value);

pub(super) fn serve(service: &Arc<Service>, db: &Path) -> Result<SyncSender<Request>> {
    let (sender, receiver) = mpsc::sync_channel::<Request>(16);
    let receiver = Arc::new(Mutex::new(receiver));
    for _ in 0..3 {
        let rx = Arc::clone(&receiver);
        let service = Arc::clone(service);
        let mut store = Store::open(db)?;
        store.enable_foreground_writes()?;
        thread::spawn(move || {
            loop {
                let request = rx.lock().expect("Discussion API queue").recv();
                let Ok(mut request) = request else { return };
                let (status, body) =
                    handle(&service, &mut store, &mut request).unwrap_or_else(|error| {
                        eprintln!("Discussion API: {error:#}");
                        api_error(500, "Server could not process the Discussion request")
                    });
                if let Ok(response) = json_response(status, &body) {
                    let _ = request.respond(response);
                }
            }
        });
    }
    Ok(sender)
}

fn handle(service: &Service, store: &mut Store, request: &mut Request) -> Result<Reply> {
    let (path, query) = request.url().split_once('?').unwrap_or((request.url(), ""));
    let values: std::collections::HashMap<_, _> = url::form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect();
    let rest = path
        .strip_prefix(DISCUSSIONS_PATH)
        .unwrap_or_default()
        .to_owned();
    let method = request.method().clone();
    if rest.is_empty() {
        return match method {
            Method::Get => Ok(ok(json!({"discussions":store.discussion_list()?}))),
            Method::Post => match input::<CreateDiscussion>(request)? {
                Ok(input)
                    if valid_id(&input.operation_id)
                        && input.title.as_deref().is_none_or(valid_title) =>
                {
                    change(
                        store.create_discussion(&input)?,
                        |chat| json!({"chat_id":chat}),
                    )
                }
                Ok(_) => Ok(api_error(400, "Valid operation ID and title required")),
                Err(reply) => Ok(reply),
            },
            _ => Ok(method_error()),
        };
    }
    if rest == "/candidates" {
        if method != Method::Get {
            return Ok(method_error());
        }
        let q = values.get("q").map_or("", String::as_str);
        let target = values.get("target").is_some_and(|v| v == "true");
        return Ok(ok(store.discussion_candidates(q, target)?));
    }
    if rest == "/workspaces" {
        if method != Method::Get {
            return Ok(method_error());
        }
        let Some(issue) = values
            .get("issue")
            .map(|id| store.resolve_issue(id))
            .transpose()?
            .flatten()
        else {
            return Ok(api_error(404, "Issue not found"));
        };
        if store.discussion_issue_ref(&issue)?.is_none() {
            return Ok(api_error(404, "Issue not found"));
        }
        let choices = store
            .workspace_choices(&issue)?
            .into_iter()
            .flat_map(|choice| {
                let status = match (&choice.runner, &choice.host, &choice.url, &service.settings) {
                    (None, _, _, _) => "workspace_not_assigned",
                    (_, _, _, None) => "server_host_not_configured",
                    (_, Some(host), _, Some(settings)) if *host != settings.host_id => {
                        "different_host"
                    }
                    (_, _, None, _) => "runner_inspection_not_configured",
                    _ => "check_on_selection",
                };
                let entries = choice.repos.clone();
                entries.into_iter().map(move |repo| {
                    json!({"task_id":choice.task,"issue_id":choice.issue,
                "title":choice.title,"short_number":choice.number,"repo_id":repo,"status":status})
                })
            })
            .collect::<Vec<_>>();
        return Ok(ok(json!({"choices":choices})));
    }
    let Some((chat, route)) = rest
        .strip_prefix('/')
        .map(|rest| rest.split_once('/').unwrap_or((rest, "")))
        .filter(|(chat, _)| valid_id(chat))
    else {
        return Ok(api_error(404, "Discussion not found"));
    };
    if !store.is_discussion(chat)? {
        return Ok(api_error(404, "Discussion not found"));
    }
    match (route, method) {
        ("", Method::Get) => {
            let Some(mut meta) = store.discussion_detail(chat)? else {
                return Ok(api_error(404, "Discussion not found"));
            };
            let registered = store.registered_repo_ids()?;
            let refs: Vec<_> = store
                .discussion_refs(chat)?
                .into_iter()
                .map(|reference| present(service, store, &registered, &reference))
                .collect();
            meta["references"] = json!(refs);
            meta["execution_configured"] = json!(service.settings.is_some());
            Ok(ok(meta))
        }
        ("", Method::Patch) => match input::<Value>(request)? {
            Ok(input) => match input["title"].as_str().filter(|title| valid_title(title)) {
                Some(title) if store.rename_discussion(chat, title)? => {
                    Ok(ok(json!({"title":title.trim()})))
                }
                _ => Ok(api_error(
                    400,
                    "A non-empty title of at most 120 characters is required",
                )),
            },
            Err(reply) => Ok(reply),
        },
        ("references", Method::Post) => match input::<DiscussionSource>(request)? {
            Ok(source) => {
                let registered = store.registered_repo_ids()?;
                let Some(reference) = build_ref(service, store, &registered, source)? else {
                    return Ok(api_error(
                        404,
                        "Reference source not found or unsupported link",
                    ));
                };
                store.add_discussion_ref(chat, &reference)?;
                Ok(ok(present(service, store, &registered, &reference)))
            }
            Err(reply) => Ok(reply),
        },
        ("references", Method::Delete) => {
            let Some(id) = values.get("id") else {
                return Ok(api_error(400, "Reference ID is required"));
            };
            store.remove_discussion_ref(chat, id)?;
            Ok(ok(json!({"id":id})))
        }
        ("draft", Method::Get) => {
            let draft = store.discussion_draft(chat, values.get("id").map(String::as_str))?;
            Ok(ok(json!({"draft_request":draft})))
        }
        ("draft", Method::Post) => match input::<Value>(request)? {
            Ok(input) => {
                let Some(id) = input["operation_id"].as_str().filter(|id| valid_id(id)) else {
                    return Ok(api_error(400, "Valid operation ID required"));
                };
                change(
                    store.request_discussion_draft(chat, id)?,
                    |_| json!({"id":id}),
                )
            }
            Err(reply) => Ok(reply),
        },
        ("handoffs", Method::Post) => match input::<DiscussionSend>(request)? {
            Ok(input)
                if valid_id(&input.operation_id)
                    && valid_id(&input.draft_id)
                    && input.target_issue_id.as_deref().is_none_or(valid_id)
                    && valid_draft(&input.draft) =>
            {
                change(store.send_discussion(chat, &input)?, |receipt| {
                    json!(receipt)
                })
            }
            Ok(_) => Ok(api_error(
                400,
                "Valid operation ID, draft ID, target and goal required",
            )),
            Err(reply) => Ok(reply),
        },
        ("references" | "draft" | "handoffs" | "", _) => Ok(method_error()),
        _ => Ok(api_error(404, "Discussion endpoint not found")),
    }
}

fn build_ref(
    service: &Service,
    store: &Store,
    registered: &[String],
    source: DiscussionSource,
) -> Result<Option<DiscussionReference>> {
    let (id, label, href, source) = match source {
        DiscussionSource::Repo { repo_id } if registered.contains(&repo_id) => {
            let href = service
                .repo_root(&repo_id, registered)
                .and_then(|(path, _)| super::repo_link(&path))
                .or_else(|| Some("/hosts".into()));
            (
                format!("repo:{repo_id}"),
                repo_id.clone(),
                href,
                DiscussionSource::Repo { repo_id },
            )
        }
        DiscussionSource::Issue { issue_id } => return store.discussion_issue_ref(&issue_id),
        DiscussionSource::Pull { url } => {
            let Some((id, label, url)) = normalized_pull(&url) else {
                return Ok(None);
            };
            (id, label, Some(url.clone()), DiscussionSource::Pull { url })
        }
        DiscussionSource::Worktree {
            issue_id,
            task_id,
            repo_id,
        } => {
            let Some(choice) = store.workspace_choice(&issue_id, &task_id)? else {
                return Ok(None);
            };
            if !choice.repos.contains(&repo_id) {
                return Ok(None);
            }
            (
                format!("worktree:{task_id}:{repo_id}"),
                format!("#{} · {}", choice.number, repo_id),
                Some(format!("/issues/{}", choice.number)),
                DiscussionSource::Worktree {
                    issue_id,
                    task_id,
                    repo_id,
                },
            )
        }
        _ => return Ok(None),
    };
    Ok(Some(DiscussionReference {
        id,
        label,
        href,
        source,
    }))
}

fn present(
    service: &Service,
    store: &Store,
    registered: &[String],
    reference: &DiscussionReference,
) -> Value {
    let mut row = json!(reference);
    row["status"] = json!(service.resource(store, reference, registered, true).status);
    row
}

fn change<T>(result: Change<T>, body: impl FnOnce(T) -> Value) -> Result<Reply> {
    Ok(match result {
        Change::Ok(value) => ok(body(value)),
        Change::Missing => api_error(
            404,
            "Discussion source, draft, reference or execution target not found",
        ),
        Change::Conflict => api_error(
            409,
            "This operation ID or draft is already bound to another request",
        ),
        Change::Invalid => api_error(400, "Invalid Discussion input"),
    })
}
fn input<T: DeserializeOwned>(r: &mut Request) -> Result<std::result::Result<T, Reply>> {
    let Some(bytes) = request_bytes(r)? else {
        return Ok(Err(api_error(413, "Discussion request too large")));
    };
    Ok(serde_json::from_slice(&bytes).map_err(|_| api_error(400, "Invalid Discussion request")))
}
fn ok(value: Value) -> Reply {
    (200, value)
}
fn method_error() -> Reply {
    api_error(405, "method not allowed")
}
