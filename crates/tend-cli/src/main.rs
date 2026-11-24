use anyhow::{Context, Result, bail, ensure};
use protocol::{
    ApiError, DEFAULT_LISTEN, IssueDispatch, MessagePage, Receipt, SendMessage, TOKEN_ENV,
};
use std::{env, io, time::Duration};

const USAGE: &str = "Usage:\n  tend chat send <chat-id> <text> [operation-id]\n  tend chat read <chat-id> [before]\n  tend issue read <issue-id>\n  tend artifact download <file-id>\n\nTEND_URL sets the Server address. TEND_TOKEN authenticates the request.";

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
    let endpoint = match namespace.as_str() {
        "chat" => protocol::messages_path(id),
        "issue" => protocol::issue_path(id),
        "artifact" => protocol::artifact_path(id),
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
        ("chat", "send", [text] | [text, _]) => {
            let operation_id = rest.get(1).cloned().unwrap_or_else(protocol::new_id);
            ensure!(protocol::valid_id(&operation_id), "invalid operation ID");
            let request = SendMessage {
                operation_id,
                text: text.clone(),
            };
            let send = || {
                http.post(&path)
                    .set("Authorization", &auth)
                    .send_json(&request)
            };
            let response = match send() {
                Err(ureq::Error::Transport(_)) => send(),
                result => result,
            };
            let receipt: Receipt = checked(response)
                .and_then(|response| Ok(response.into_json()?))
                .with_context(|| format!("send operation {}", request.operation_id))?;
            println!("{}", serde_json::to_string(&receipt)?);
        }
        ("chat", "read", [] | [_]) => {
            let mut request = http.get(&path).set("Authorization", &auth);
            if let Some(before) = rest.first() {
                request = request.query("before", &before.parse::<i64>()?.to_string());
            }
            let page: MessagePage = checked(request.call())?.into_json()?;
            println!("{}", serde_json::to_string(&page)?);
        }
        ("issue", "read", []) => {
            let issue: IssueDispatch =
                checked(http.get(&path).set("Authorization", &auth).call())?.into_json()?;
            println!("{}", serde_json::to_string(&issue)?);
        }
        ("artifact", "download", []) => {
            let response = checked(http.get(&path).set("Authorization", &auth).call())?;
            io::copy(&mut response.into_reader(), &mut io::stdout().lock())?;
        }
        _ => bail!("{USAGE}"),
    }

    Ok(())
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
