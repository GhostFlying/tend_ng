use crate::{
    api_error, artifacts, json_response, request_bytes,
    store::{Store, web_sessions::Digest},
};
use anyhow::{Result, anyhow, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};
use subtle::ConstantTimeEq;
use tiny_http::{Header, Method, Request, Response, ResponseBox};
use url::Url;

pub(crate) const PATH: &str = "/api/v1/web/session";
const SESSION_TTL: i64 = 12 * 60 * 60;
const REMEMBER_TTL: i64 = 30 * 24 * 60 * 60;
const HTTPS_COOKIE: &str = "__Host-tend-session";
const LOOPBACK_COOKIE: &str = "tend-loopback-session";

pub(crate) struct Sessions {
    https: Option<Entry>,
    generation: Digest,
}

struct Entry {
    origin: String,
    host: String,
    secure: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Login {
    remember: bool,
}

impl Sessions {
    pub(crate) fn new(https: Option<&str>, token: &str) -> Result<Self> {
        let https = https.map(Entry::https).transpose()?;
        // Domain separation keeps the credential fingerprint distinct from session digests.
        let mut hash = Sha256::new();
        hash.update(b"tend web credential generation v1\0");
        hash.update(token);
        Ok(Self {
            https,
            generation: hash.finalize().into(),
        })
    }

    pub(crate) fn prepare(&self, store: &Store) -> Result<()> {
        store.prepare_web_sessions(&self.generation, now()?)
    }

    fn entry(&self, request: &Request) -> Option<Entry> {
        // A remote plaintext client cannot claim to be our TLS terminator.
        if !request.remote_addr()?.ip().to_canonical().is_loopback() {
            return None;
        }
        let host = single_header(request, "Host")?;
        if let Some(entry) = &self.https {
            return host.eq_ignore_ascii_case(&entry.host).then(|| Entry {
                origin: entry.origin.clone(),
                host: entry.host.clone(),
                secure: true,
            });
        }
        Entry::loopback(host)
    }

    pub(crate) fn authorize(
        &self,
        store: &Store,
        request: &Request,
        auth: &str,
    ) -> Result<Option<ResponseBox>> {
        match bearer(request, auth) {
            Some(true) => return Ok(None),
            Some(false) => return Ok(Some(error(401, "valid Tend token required")?)),
            None => {}
        }
        if !web_route(request.method(), request.url()) {
            return Ok(Some(error(401, "valid Tend token required")?));
        }
        let Some(entry) = self.entry(request) else {
            return Ok(Some(error(401, "valid Tend session required")?));
        };
        if request.method() != &Method::Get && !entry.allows_change(request) {
            return Ok(Some(error(403, "same-origin Tend Web request required")?));
        }
        if self.valid(store, request, &entry)? {
            Ok(None)
        } else {
            Ok(Some(error(401, "valid Tend session required")?))
        }
    }

    pub(crate) fn handle(
        &self,
        store: &mut Store,
        request: &mut Request,
        auth: &str,
    ) -> Result<ResponseBox> {
        let bearer = bearer(request, auth);
        if bearer == Some(false) {
            return error(401, "valid Tend token required");
        }
        match request.method() {
            Method::Post => {
                if bearer != Some(true) {
                    return error(401, "valid Tend token required");
                }
                let Some(entry) = self.entry(request) else {
                    return error(
                        403,
                        "Web login requires the configured HTTPS address or local loopback",
                    );
                };
                if !entry.allows_change(request) {
                    return error(403, "same-origin Tend Web request required");
                }
                let Some(body) = request_bytes(request)? else {
                    return error(413, "Web session request too large");
                };
                let Ok(Login { remember }) = serde_json::from_slice(&body) else {
                    return error(400, "expected a remember boolean");
                };
                if remember && !entry.secure {
                    return error(403, "remembering this browser requires HTTPS");
                }
                let mut bytes = [0u8; 32];
                getrandom::fill(&mut bytes)
                    .map_err(|_| anyhow!("could not generate Web session randomness"))?;
                let now = now()?;
                let ttl = if remember { REMEMBER_TTL } else { SESSION_TTL };
                store.issue_web_session(
                    &digest(&bytes),
                    entry.cookie_digest(request).as_ref(),
                    &self.generation,
                    &entry.origin,
                    now,
                    now + ttl,
                )?;
                Ok(empty()
                    .with_header(
                        entry.cookie(&URL_SAFE_NO_PAD.encode(bytes), remember.then_some(ttl)),
                    )
                    .boxed())
            }
            Method::Get | Method::Head => {
                if bearer.is_none()
                    && let Some(entry) = self.entry(request)
                    && self.valid(store, request, &entry)?
                {
                    return Ok(empty().boxed());
                }
                error(401, "valid Tend session required")
            }
            Method::Delete => {
                if bearer.is_some() {
                    return error(401, "valid Tend session required");
                }
                let Some(entry) = self.entry(request) else {
                    return error(403, "same-origin Tend Web request required");
                };
                if !entry.allows_change(request) {
                    return error(403, "same-origin Tend Web request required");
                }
                if let Some(id) = entry.cookie_digest(request) {
                    store.revoke_web_session(&id, &self.generation, &entry.origin)?;
                }
                Ok(empty().with_header(entry.cookie("", Some(0))).boxed())
            }
            _ => error(405, "method not allowed"),
        }
    }

    fn valid(&self, store: &Store, request: &Request, entry: &Entry) -> Result<bool> {
        let Some(id) = entry.cookie_digest(request) else {
            return Ok(false);
        };
        store.valid_web_session(&id, &self.generation, &entry.origin, now()?)
    }
}

impl Entry {
    fn https(value: &str) -> Result<Self> {
        let url = Url::parse(value).map_err(|_| anyhow!("invalid --web-origin"))?;
        let origin = url.origin().ascii_serialization();
        ensure!(
            url.scheme() == "https" && value == origin,
            "--web-origin must be a canonical HTTPS origin (no path or trailing slash)"
        );
        let host = origin["https://".len()..].to_owned();
        Ok(Self {
            origin,
            host,
            secure: true,
        })
    }

    fn loopback(host: &str) -> Option<Self> {
        let host = host.to_ascii_lowercase();
        let origin = format!("http://{host}");
        let url = Url::parse(&origin).ok()?;
        if !matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
            || url.origin().ascii_serialization() != origin
        {
            return None;
        }
        Some(Self {
            origin,
            host,
            secure: false,
        })
    }

    fn allows_change(&self, request: &Request) -> bool {
        single_header(request, "Origin") == Some(&self.origin)
            && single_header(request, "X-Tend-Web") == Some("1")
    }

    fn cookie_name(&self) -> &str {
        if self.secure {
            HTTPS_COOKIE
        } else {
            LOOPBACK_COOKIE
        }
    }

    fn cookie_digest(&self, request: &Request) -> Option<Digest> {
        let mut matches = request
            .headers()
            .iter()
            .filter(|header| header.field.equiv("Cookie"))
            .flat_map(|header| header.value.as_str().split(';'))
            .filter_map(|part| {
                let (name, value) = part.trim().split_once('=')?;
                (name == self.cookie_name()).then_some(value)
            });
        let value = matches.next()?;
        if value.len() != 43 || matches.next().is_some() {
            return None;
        }
        let mut bytes = [0u8; 32];
        (URL_SAFE_NO_PAD.decode_slice(value, &mut bytes).ok()? == bytes.len())
            .then(|| digest(&bytes))
    }

    fn cookie(&self, value: &str, max_age: Option<i64>) -> Header {
        let mut cookie = format!(
            "{}={value}; Path=/; HttpOnly; SameSite=Lax",
            self.cookie_name()
        );
        if self.secure {
            cookie.push_str("; Secure");
        }
        if let Some(max_age) = max_age {
            cookie.push_str(&format!("; Max-Age={max_age}"));
        }
        Header::from_bytes("Set-Cookie", cookie).expect("Web cookie is ASCII")
    }
}

fn now() -> Result<i64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_secs()
        .try_into()?)
}

fn digest(bytes: &[u8]) -> Digest {
    Sha256::digest(bytes).into()
}

// A present but invalid or duplicated Authorization header must never fall back to cookies.
fn bearer(request: &Request, auth: &str) -> Option<bool> {
    let mut values = request
        .headers()
        .iter()
        .filter(|header| header.field.equiv("Authorization"));
    let value = values.next()?;
    Some(
        values.next().is_none()
            && bool::from(value.value.as_str().as_bytes().ct_eq(auth.as_bytes())),
    )
}

fn single_header<'a>(request: &'a Request, name: &'static str) -> Option<&'a str> {
    let mut values = request
        .headers()
        .iter()
        .filter(|header| header.field.equiv(name));
    let value = values.next()?;
    values.next().is_none().then(|| value.value.as_str())
}

fn web_route(method: &Method, url: &str) -> bool {
    let path = url.split('?').next().unwrap_or_default();
    if method == &Method::Get {
        if matches!(
            path,
            protocol::ISSUE_BOARD_PATH
                | protocol::TOP_LEVEL_BOARD_PATH
                | protocol::EXECUTION_HOSTS_PATH
                | "/api/v1/runners"
                | "/api/v1/agents"
        ) {
            return true;
        }
        if protocol::rollout_id(url).is_some()
            || matches!(
                artifacts::Endpoint::parse(url),
                Some(artifacts::Endpoint::Download(_))
            )
        {
            return true;
        }
    }
    let Some(path) = path.strip_prefix("/api/v1/") else {
        return false;
    };
    let segments: Vec<_> = path.split('/').collect();
    let valid = |id: &str| protocol::valid_id(id);
    let issue = |id: &str| {
        valid(id)
            || id
                .strip_prefix("%23")
                .is_some_and(|n| protocol::parse_issue_number(n).is_some())
    };
    match (method, segments.as_slice()) {
        (Method::Get | Method::Post, ["discussions"]) => true,
        (Method::Get, ["discussions", "workspaces" | "candidates"]) => true,
        (Method::Get | Method::Patch, ["discussions", id])
        | (Method::Get | Method::Post, ["discussions", id, "draft"])
        | (Method::Post | Method::Delete, ["discussions", id, "references"])
        | (Method::Post, ["discussions", id, "handoffs"]) => valid(id),
        (
            Method::Get,
            [
                "chats",
                id,
                "messages" | "notifications" | "attentions" | "issues",
            ],
        )
        | (Method::Post, ["chats", id, "messages"]) => valid(id),
        (Method::Get, ["chats", id, "attentions", attention])
        | (Method::Post, ["chats", id, "attentions", attention, "approve"]) => {
            valid(id) && valid(attention)
        }
        (Method::Get, ["issues", id])
        | (
            Method::Get,
            [
                "issues",
                id,
                "messages" | "overview" | "attention-replies" | "token-usage",
            ],
        )
        | (Method::Post, ["issues", id, "messages" | "start" | "state"]) => issue(id),
        (Method::Post, ["issues", id, "attentions", attention, "answer"]) => {
            issue(id) && valid(attention)
        }
        _ => false,
    }
}

fn empty() -> Response<std::io::Empty> {
    Response::empty(204).with_header(Header::from_bytes("Cache-Control", "no-store").unwrap())
}

fn error(status: u16, message: &str) -> Result<ResponseBox> {
    Ok(json_response(status, &api_error(status, message).1)?.boxed())
}

#[cfg(test)]
mod tests;
