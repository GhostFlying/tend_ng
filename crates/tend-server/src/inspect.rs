use crate::{api_error, json_response, store::Store};
use anyhow::Result;
use serde_json::Value;
use std::{
    sync::mpsc::{self, SyncSender},
    thread,
    time::Duration,
};
use tiny_http::Request;

pub fn serve(store: Store, auth: String) -> SyncSender<(Request, String)> {
    let (sender, receiver) = mpsc::sync_channel::<(Request, String)>(4);
    thread::spawn(move || {
        let http = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(4))
            .redirects(0)
            .build();
        for (request, run) in receiver {
            let (status, body) = read(&store, &http, &auth, &run).unwrap_or_else(|error| {
                eprintln!("Read rollout for {run}: {error:#}");
                api_error(503, "Original Runner history is unavailable")
            });
            if let Ok(response) = json_response(status, &body) {
                let _ = request.respond(response);
            }
        }
    });
    sender
}

fn read(store: &Store, http: &ureq::Agent, auth: &str, run: &str) -> Result<(u16, Value)> {
    let Some(source) = store.run_origin(run)? else {
        return Ok(api_error(404, "Run has no recorded native turn"));
    };
    let Some(base) = source.url else {
        return Ok(api_error(503, "Runner has no inspection address"));
    };
    let base = base.trim_end_matches('/');
    let historical = source.reader != source.origin;
    let response = if historical {
        let path = protocol::retired_rollout_path(&source.origin, run);
        http.post(&format!("{base}{path}"))
            .set("Authorization", auth)
            .send_json(&source.native)
    } else {
        let path = protocol::rollout_path(run);
        http.get(&format!("{base}{path}"))
            .set("Authorization", auth)
            .call()
    };
    match response {
        Ok(response) => {
            let data: Value = response.into_json()?;
            if data["runner_id"] != source.origin
                || data["run_id"] != run
                || data["turn"]["id"] != source.native.turn_id
                || (historical && data["reader_id"] != source.reader)
            {
                return Ok(api_error(
                    503,
                    "Inspection response did not match the recorded Run and reader",
                ));
            }
            Ok((200, data))
        }
        Err(ureq::Error::Status(404, _)) => Ok(api_error(404, "Original native turn unavailable")),
        Err(_) => Ok(api_error(503, "Original Runner history is unavailable")),
    }
}
