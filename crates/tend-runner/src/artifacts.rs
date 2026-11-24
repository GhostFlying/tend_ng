use anyhow::{Context, Result, ensure};
use protocol::WorkAction;
use std::{
    fs::{self, File},
    io,
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

pub struct Transfer {
    pub root: PathBuf,
    pub base: String,
    pub runner: String,
    pub auth: String,
}

impl Transfer {
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
