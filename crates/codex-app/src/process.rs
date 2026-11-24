use anyhow::{Context, Result, bail};
use std::process::Command;

#[derive(Debug)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub started_at: String,
}

impl ProcessIdentity {
    pub(crate) fn observe(pid: u32) -> Result<Self> {
        let (state, started_at) = process_status(pid)?.context("App Server already exited")?;
        if exited(&state) {
            bail!("App Server already exited");
        }
        Ok(Self { pid, started_at })
    }

    pub fn is_stopped(&self) -> Result<bool> {
        Ok(process_status(self.pid)?
            .is_none_or(|(state, start)| exited(&state) || start != self.started_at))
    }
}

fn process_status(pid: u32) -> Result<Option<(String, String)>> {
    // Capture the creation time as well as the PID. A restarted Server must
    // distinguish the original child from an unrelated process with that PID.
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "stat=", "-o", "lstart="])
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .context("inspect App Server process")?;
    let data = std::str::from_utf8(&output.stdout)?.trim();
    if output.status.code() == Some(1) && data.is_empty() && output.stderr.is_empty() {
        return Ok(None);
    }
    if !output.status.success() {
        bail!("inspect App Server process failed ({})", output.status);
    }
    let (state, start) = data
        .split_once(char::is_whitespace)
        .context("missing App Server process start time")?;
    Ok(Some((state.to_owned(), start.trim().to_owned())))
}

fn exited(state: &str) -> bool {
    state.starts_with(['Z', 'X'])
}
