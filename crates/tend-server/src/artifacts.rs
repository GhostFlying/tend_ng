use crate::{json_response, store::Store};
use anyhow::Result;
use serde_json::json;
use std::{
    fs::{self, File},
    io::{self, ErrorKind, Read},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        mpsc::{self, SyncSender},
    },
    thread,
};
use tiny_http::{Header, Method, Request, Response, ResponseBox};

pub enum Endpoint {
    Download(String),
    Upload { runner: String, id: String },
}

impl Endpoint {
    pub fn parse(path: &str) -> Option<Self> {
        if let Some(id) = path
            .strip_prefix("/api/v1/artifacts/")
            .filter(|id| protocol::valid_id(id))
        {
            return Some(Self::Download(id.to_owned()));
        }
        let (runner, id) = path
            .strip_prefix("/api/v1/runners/")?
            .split_once("/artifacts/")?;
        if protocol::valid_id(runner) && protocol::valid_id(id) {
            return Some(Self::Upload {
                runner: runner.to_owned(),
                id: id.to_owned(),
            });
        }
        None
    }
}

pub fn serve(db: &Path) -> Result<SyncSender<(Request, Endpoint)>> {
    let root = db.with_extension("artifacts");
    fs::create_dir_all(&root)?;
    let (sender, receiver) = mpsc::sync_channel::<(Request, Endpoint)>(8);
    let receiver = Arc::new(Mutex::new(receiver));
    for _ in 0..2 {
        let mut store = Store::open(db)?;
        let receiver = Arc::clone(&receiver);
        let root = root.clone();
        thread::spawn(move || {
            loop {
                let job = receiver.lock().expect("file request queue").recv();
                let Ok((mut request, endpoint)) = job else {
                    return;
                };
                let response =
                    handle(&mut store, &root, &mut request, endpoint).unwrap_or_else(|error| {
                        eprintln!("Delivery request: {error:#}");
                        error_response(500, "Could not transfer delivery file")
                    });
                if let Err(error) = request.respond(response) {
                    eprintln!("Send delivery response: {error}");
                }
            }
        });
    }
    Ok(sender)
}

fn handle(
    store: &mut Store,
    root: &Path,
    request: &mut Request,
    endpoint: Endpoint,
) -> Result<ResponseBox> {
    match endpoint {
        Endpoint::Download(id) if request.method() == &Method::Get => {
            let Some(artifact) = store.artifact(&id, None)? else {
                return Ok(error_response(404, "Delivery file not found"));
            };
            if !artifact.available {
                return Ok(error_response(409, "Delivery file is pending upload"));
            }
            let path = root.join(&id);
            match File::open(&path) {
                Ok(file) => Ok(Response::from_file(file)
                    .with_header(
                        Header::from_bytes("Content-Type", "application/octet-stream").unwrap(),
                    )
                    .with_header(Header::from_bytes("Content-Disposition", "attachment").unwrap())
                    .boxed()),
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    store.mark_artifact_missing(&id, &path)?;
                    Ok(error_response(409, "Delivery file is pending restoration"))
                }
                Err(error) => Err(error.into()),
            }
        }
        Endpoint::Upload { runner, id } if request.method() == &Method::Put => {
            let Some(artifact) = store.artifact(&id, Some(&runner))? else {
                return Ok(error_response(
                    404,
                    "Delivery is not registered for this Runner",
                ));
            };
            let path = root.join(&id);
            if !artifact.available || !path.is_file() {
                let partial = root.join(format!("{id}-{}.part", protocol::new_id()));
                let uploaded = receive_file(request.as_reader(), &partial, &path, artifact.bytes);
                if !matches!(uploaded, Ok(true)) {
                    let _ = fs::remove_file(&partial);
                }
                if !uploaded? {
                    return Ok(error_response(
                        400,
                        "File length differs from registered delivery",
                    ));
                }
                File::open(root)?.sync_all()?;
                store.finish_artifact(artifact)?;
            }
            Ok(json_response(200, &json!({"id": id}))?.boxed())
        }
        _ => Ok(error_response(405, "method not allowed")),
    }
}

fn receive_file(
    input: &mut dyn Read,
    partial: &Path,
    destination: &PathBuf,
    expected: u64,
) -> Result<bool> {
    let mut file = File::create(partial)?;
    if io::copy(&mut input.take(expected.saturating_add(1)), &mut file)? != expected {
        return Ok(false);
    }
    file.sync_all()?;
    fs::rename(partial, destination)?;
    Ok(true)
}

fn error_response(status: u16, message: &str) -> ResponseBox {
    json_response(status, &json!({"error": message}))
        .expect("serialize error")
        .boxed()
}
