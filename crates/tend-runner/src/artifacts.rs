use anyhow::{Context, Result, ensure};
use protocol::WorkAction;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, ErrorKind, Read},
    path::{Path, PathBuf},
    sync::mpsc::{self, SyncSender},
    thread,
    time::Duration,
};

pub fn snapshot(root: &Path, workspace: &Path, path: &Path, id: &str) -> Result<WorkAction> {
    let source = workspace
        .join(path)
        .canonicalize()
        .context("resolve delivery file")?;
    ensure!(
        source.starts_with(workspace) && source.is_file(),
        "select a file in the assigned workspace"
    );
    let name = source
        .file_name()
        .and_then(|name| name.to_str())
        .context("delivery file name must be UTF-8")?;
    fs::create_dir_all(root)?;
    let partial = root.join(format!("{id}.part"));
    let mut copy = File::create(&partial)?;
    let bytes = io::copy(&mut File::open(source.as_path())?, &mut copy)?;
    copy.sync_all()?;
    fs::rename(&partial, root.join(id))?;
    File::open(root)?.sync_all()?;
    Ok(WorkAction::SubmitArtifact {
        name: name.to_owned(),
        bytes,
    })
}

#[derive(Clone)]
pub struct Transfer {
    pub root: PathBuf,
    pub base: String,
    pub runner: String,
    pub auth: String,
}

impl Transfer {
    // Reviews read the immutable host copy offline. A missing local copy
    // can be restored under the same ID from the Server.
    pub fn review_file(&self, id: &str) -> Result<Option<PathBuf>> {
        ensure!(protocol::valid_id(id), "invalid review artifact");
        fs::create_dir_all(&self.root)?;
        let root = self.root.canonicalize()?;
        let path = root.join(id);
        match path.symlink_metadata() {
            Ok(metadata) => {
                ensure!(metadata.is_file(), "review artifact is not a regular file");
                return Ok(Some(path));
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }

        let endpoint = protocol::artifact_path(id);
        let response = ureq::AgentBuilder::new()
            .redirects(0)
            .timeout_connect(Duration::from_secs(5))
            .timeout_read(Duration::from_secs(30))
            .build()
            .get(&format!("{}{endpoint}", self.base))
            .set("Authorization", &self.auth)
            .call();
        let response = match response {
            Ok(response) => response,
            Err(
                error @ (ureq::Error::Transport(_) | ureq::Error::Status(409 | 429 | 500..=599, _)),
            ) => {
                eprintln!("Wait for review artifact {id}: {error}");
                return Ok(None);
            }
            Err(error) => return Err(error).context("download bound review artifact"),
        };
        ensure!(
            response.status() == 200,
            "unexpected review download status"
        );
        let bytes: u64 = response
            .header(protocol::ARTIFACT_BYTES_HEADER)
            .or_else(|| response.header("Content-Length"))
            .context("missing review artifact length")?
            .parse()?;
        let partial = root.join(format!("{id}-{}.part", protocol::new_id()));
        let mut copy = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&partial)?;
        match io::copy(
            &mut response.into_reader().take(bytes.saturating_add(1)),
            &mut copy,
        ) {
            Ok(copied) if copied == bytes => {
                let committed = copy.sync_all().and_then(|()| fs::rename(&partial, &path));
                if committed.is_err() {
                    let _ = fs::remove_file(&partial);
                }
                committed?;
                File::open(&root)?.sync_all()?;
                Ok(Some(path))
            }
            result => {
                let _ = fs::remove_file(&partial);
                eprintln!(
                    "Retry incomplete review artifact {id}: expected {bytes} bytes, got {result:?}"
                );
                Ok(None)
            }
        }
    }

    pub fn start(self) -> SyncSender<Vec<String>> {
        // The next sync supplies a fresh list after this worker finishes.
        let (send, receive) = mpsc::sync_channel(0);
        thread::spawn(move || {
            while let Ok(ids) = receive.recv() {
                let _ = self.upload(ids);
            }
        });
        send
    }

    pub fn upload(&self, ids: Vec<String>) -> Result<()> {
        let http = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(5))
            .timeout_read(Duration::from_secs(30))
            .timeout_write(Duration::from_secs(30))
            .build();
        let mut result = Ok(());
        for id in ids {
            if let Err(error) = self.send_file(&http, &id) {
                eprintln!("Upload delivery {id}: {error:#}");
                result = Err(error);
            }
        }
        result
    }

    fn send_file(&self, http: &ureq::Agent, id: &str) -> Result<()> {
        ensure!(protocol::valid_id(id), "invalid delivery file ID");
        let file = File::open(self.root.join(id))?;
        let size = file.metadata()?.len();
        let path = protocol::artifact_upload_path(&self.runner, id);
        http.put(&format!("{}{path}", self.base))
            .set("Authorization", &self.auth)
            .set("Content-Type", "application/octet-stream")
            .set("Content-Length", &size.to_string())
            .send(file)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_download_only_publishes_a_complete_snapshot_and_then_reads_offline() -> Result<()> {
        struct Directory(PathBuf);
        impl Drop for Directory {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }

        let root = Directory(std::env::temp_dir().join(protocol::new_id()));
        let server =
            tiny_http::Server::http("127.0.0.1:0").map_err(|error| anyhow::anyhow!("{error}"))?;
        let source = Transfer {
            root: root.0.clone(),
            base: format!("http://{}", server.server_addr()),
            runner: "successor".into(),
            auth: "Bearer fixture".into(),
        };
        let served = thread::spawn(move || {
            for (status, body) in [(409, "pending"), (200, "cut"), (200, "saved snapshot")] {
                let request = server
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap()
                    .unwrap();
                assert_eq!(request.url(), "/api/v1/artifacts/original");
                assert!(
                    request
                        .headers()
                        .iter()
                        .any(|header| header.field.equiv("Authorization")
                            && header.value.as_str() == "Bearer fixture")
                );
                let response = tiny_http::Response::from_string(body)
                    .with_status_code(status)
                    .with_header(
                        tiny_http::Header::from_bytes(protocol::ARTIFACT_BYTES_HEADER, "14")
                            .unwrap(),
                    );
                request.respond(response).unwrap();
            }
        });
        for _ in 0..2 {
            assert_eq!(source.review_file("original")?, None);
            assert_eq!(fs::read_dir(&root.0)?.count(), 0);
        }
        let received = source
            .review_file("original")?
            .context("candidate was not published")?;
        assert_eq!(received, root.0.canonicalize()?.join("original"));
        assert_eq!(fs::read(&received)?, b"saved snapshot");
        served.join().unwrap();
        assert_eq!(source.review_file("original")?, Some(received));
        Ok(())
    }
}
