use anyhow::{Context, Result, bail, ensure};
use protocol::{
    ApiError, Attention, DEFAULT_LISTEN, IssueBoard, IssueDispatch, IssueMessagePage, LinkPull,
    MessagePage, NotificationPage, PullLink, PullLinkReceipt, Receipt, SendMessage, TOKEN_ENV,
};
use std::{env, io, time::Duration};

const USAGE: &str = "Usage:\n  tend chat send <chat-id> <text> [operation-id] [--reply-to <message-id>]\n  tend chat read <chat-id> [before]\n  tend attention list <chat-id>\n  tend notification list <chat-id> [before]\n  tend issue list <chat-id> [before]\n  tend issue read <issue-id>\n  tend pr link <issue-id> <url> [operation-id]\n  tend pr list <issue-id>\n  tend issue messages <issue-id> [before]\n  tend issue message <issue-id> <text> [operation-id] [--reply-to <message-id>]\n  tend run rollout <run-id>\n  tend artifact download <file-id>\n\nTEND_URL sets the Server address. TEND_TOKEN authenticates the request.";

fn main() -> Result<()> {
    let args: Vec<_> = env::args().skip(1).collect();
    if args.is_empty() || args == ["--help"] {
        println!("{USAGE}");
        return Ok(());
    }
    let [namespace, command, id, rest @ ..] = args.as_slice() else {
        bail!("{USAGE}");
    };
    ensure!(protocol::valid_id(id), "{USAGE}");

    let base = env::var("TEND_URL").unwrap_or_else(|_| format!("http://{DEFAULT_LISTEN}"));
    let endpoint = match (namespace.as_str(), command.as_str()) {
        ("chat", _) => protocol::messages_path(id),
        ("issue", "read") => protocol::issue_path(id),
        ("issue", "list") => protocol::issue_board_path(id),
        ("pr", "link" | "list") => protocol::issue_pulls_path(id),
        ("issue", "messages" | "message") => protocol::issue_messages_path(id),
        ("run", "rollout") => protocol::rollout_path(id),
        ("artifact", _) => protocol::artifact_path(id),
        ("attention", _) => protocol::attentions_path(id),
        ("notification", "list") => protocol::notifications_path(id),
        _ => bail!("{USAGE}"),
    };
    let path = format!("{}{}", base.trim_end_matches('/'), endpoint);
    let token = env::var(TOKEN_ENV).context("set TEND_TOKEN before calling Tend")?;
    ensure!(!token.is_empty(), "TEND_TOKEN is empty");
    let auth = format!("Bearer {token}");
    let http = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(5))
        .build();

    match (namespace.as_str(), command.as_str(), rest) {
        ("chat", "send", [text, arguments @ ..]) | ("issue", "message", [text, arguments @ ..]) => {
            let (operation, reply) = match arguments {
                [] => (None, None),
                [operation] => (Some(operation), None),
                [flag, reply] if flag == "--reply-to" => (None, Some(reply)),
                [operation, flag, reply] if flag == "--reply-to" => (Some(operation), Some(reply)),
                _ => bail!("{USAGE}"),
            };
            let operation_id = operation.cloned().unwrap_or_else(protocol::new_id);
            ensure!(protocol::valid_id(&operation_id), "invalid operation ID");
            ensure!(
                reply.is_none_or(|id| protocol::valid_id(id)),
                "invalid reply message ID"
            );
            let request = SendMessage {
                operation_id,
                text: text.clone(),
                reply_to: reply.cloned(),
            };
            let receipt: Receipt = send(&http, &path, &auth, &serde_json::to_value(&request)?)
                .and_then(|response| Ok(response.into_json()?))
                .with_context(|| format!("send operation {}", request.operation_id))?;
            println!("{}", serde_json::to_string(&receipt)?);
        }
        ("chat", "read", [] | [_])
        | ("issue", "messages" | "list", [] | [_])
        | ("notification", "list", [] | [_]) => {
            let mut request = http.get(&path).set("Authorization", &auth);
            if let Some(before) = rest.first() {
                request = request.query("before", &before.parse::<i64>()?.to_string());
            }
            let response = checked(request.call())?;
            let page = match command.as_str() {
                "messages" => serde_json::to_string(&response.into_json::<IssueMessagePage>()?)?,
                "list" if namespace == "issue" => {
                    serde_json::to_string(&response.into_json::<IssueBoard>()?)?
                }
                "list" => serde_json::to_string(&response.into_json::<NotificationPage>()?)?,
                _ => serde_json::to_string(&response.into_json::<MessagePage>()?)?,
            };
            println!("{page}");
        }
        ("pr", "link", [url, operation @ ..]) if operation.len() <= 1 => {
            let operation_id = operation.first().cloned().unwrap_or_else(protocol::new_id);
            ensure!(protocol::valid_id(&operation_id), "invalid operation ID");
            let request = LinkPull {
                operation_id,
                url: url.clone(),
            };
            let receipt: PullLinkReceipt =
                send(&http, &path, &auth, &serde_json::to_value(&request)?)
                    .and_then(|response| Ok(response.into_json()?))
                    .with_context(|| format!("link operation {}", request.operation_id))?;
            println!("{}", serde_json::to_string(&receipt)?);
        }
        ("pr", "list", []) => {
            let links: Vec<PullLink> =
                checked(http.get(&path).set("Authorization", &auth).call())?.into_json()?;
            println!("{}", serde_json::to_string(&links)?);
        }
        ("issue", "read", []) => {
            let issue: IssueDispatch =
                checked(http.get(&path).set("Authorization", &auth).call())?.into_json()?;
            println!("{}", serde_json::to_string(&issue)?);
        }
        ("artifact", "download", []) | ("run", "rollout", []) => {
            let response = checked(http.get(&path).set("Authorization", &auth).call())?;
            io::copy(&mut response.into_reader(), &mut io::stdout().lock())?;
        }
        ("attention", "list", []) => {
            let attentions: Vec<Attention> =
                checked(http.get(&path).set("Authorization", &auth).call())?.into_json()?;
            println!("{}", serde_json::to_string(&attentions)?);
        }
        _ => bail!("{USAGE}"),
    }

    Ok(())
}

fn send(
    http: &ureq::Agent,
    path: &str,
    auth: &str,
    body: &serde_json::Value,
) -> Result<ureq::Response> {
    let post = || http.post(path).set("Authorization", auth).send_json(body);
    checked(match post() {
        Err(ureq::Error::Transport(_)) => post(),
        result => result,
    })
}

fn checked(result: Result<ureq::Response, ureq::Error>) -> Result<ureq::Response> {
    match result {
        Ok(response) => Ok(response),
        Err(ureq::Error::Status(status, response)) => {
            let message = response
                .into_json::<ApiError>()
                .map(|error| error.error)
                .unwrap_or_else(|_| "request failed".to_owned());
            bail!("Server returned HTTP {status}: {message}");
        }
        Err(error) => Err(error.into()),
    }
}
