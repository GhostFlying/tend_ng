use crate::{config::Config, inspect::indexed_launch, store::Store, usage::native_snapshot};
use protocol::{
    NativeTokenRecord, RUNNER_SYNC_BATCH, TokenBackfillContent, TokenBackfillPage,
    TokenBackfillWork,
    TokenSourceGap::{self, *},
};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::Duration,
};

pub struct Worker {
    work: SyncSender<TokenBackfillWork>,
    results: Receiver<TokenBackfillPage>,
    inflight: Option<TokenBackfillWork>,
    pub page: Option<TokenBackfillPage>,
}

impl Worker {
    pub fn start(store: Store, config: Arc<Config>, root: PathBuf) -> Self {
        let (work, input) = mpsc::sync_channel::<TokenBackfillWork>(1);
        let (output, results) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let mut scanner = Scanner {
                store,
                history: crate::host_data_root(&root, &config.host_id).join("history"),
                runner: config.runner_id.clone(),
                job: String::new(),
                homes: HashMap::new(),
                cached: None,
            };
            for work in input {
                let content = scanner.read(&work);
                if output
                    .send(TokenBackfillPage {
                        item: work.item,
                        offset: work.offset,
                        content,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            work,
            results,
            inflight: None,
            page: None,
        }
    }

    pub fn accept(&mut self, work: Option<TokenBackfillWork>) {
        // A successful reply's offset is the durable acknowledgement.
        if let (Some(page), Some(next)) = (&self.page, &work)
            && page.item == next.item
            && page.offset == next.offset
        {
            return;
        }
        self.page = None;
        if self.inflight.is_none()
            && let Some(work) = work
            && self.work.try_send(work.clone()).is_ok()
        {
            self.inflight = Some(work);
        }
    }

    pub fn wait(&mut self, interval: Duration) {
        if self.inflight.is_some()
            && let Ok(page) = self.results.recv_timeout(interval)
        {
            self.inflight = None;
            self.page = Some(page);
        } else if self.inflight.is_none() {
            thread::sleep(interval);
        }
    }
}

type Rollouts = HashMap<String, Option<PathBuf>>;
type Snapshot = (i64, String, Vec<NativeTokenRecord>, bool);

struct Scanner {
    store: Store,
    history: PathBuf,
    runner: String,
    job: String,
    homes: HashMap<PathBuf, Result<Rollouts, TokenSourceGap>>,
    cached: Option<Snapshot>,
}

impl Scanner {
    fn read(&mut self, work: &TokenBackfillWork) -> TokenBackfillContent {
        if self.job != work.job_id {
            self.job.clone_from(&work.job_id);
            self.homes.clear();
        }
        if !self
            .cached
            .as_ref()
            .is_some_and(|(item, _, _, _)| *item == work.item)
        {
            self.cached = None;
            match self.snapshot(work) {
                Ok((records, active)) => {
                    let bytes = serde_json::to_vec(&(&records, active))
                        .expect("serializable Token evidence");
                    self.cached = Some((
                        work.item,
                        format!("{:x}", Sha256::digest(bytes)),
                        records,
                        active,
                    ));
                }
                Err(reason) => return TokenBackfillContent::Gap { reason },
            }
        }
        let (_, snapshot, records, active) = self.cached.as_ref().unwrap();
        let Ok(offset) = usize::try_from(work.offset) else {
            return TokenBackfillContent::Gap {
                reason: SourceChanged,
            };
        };
        let Some(rest) = records.get(offset..).filter(|rest| !rest.is_empty()) else {
            return TokenBackfillContent::Gap {
                reason: SourceChanged,
            };
        };
        TokenBackfillContent::Records {
            snapshot: snapshot.clone(),
            records: rest[..rest.len().min(RUNNER_SYNC_BATCH)].to_vec(),
            done: rest.len() <= RUNNER_SYNC_BATCH,
            prefix: *active,
        }
    }

    fn snapshot(
        &mut self,
        work: &TokenBackfillWork,
    ) -> Result<(Vec<NativeTokenRecord>, bool), TokenSourceGap> {
        let index = self.history.join(format!("{}.sqlite", work.runner_id));
        let indexed = match index.try_exists() {
            Ok(true) => indexed_launch(&index, &work.thread_id).map_err(|_| SourceUnreadable)?,
            Ok(false) => None,
            Err(_) => return Err(SourceUnreadable),
        };
        let (launch, active) = if work.runner_id == self.runner {
            let (thread, turn, launch) = self
                .store
                .native_turn(&work.run_id)
                .map_err(|_| SourceUnreadable)?
                .ok_or(BindingMissing)?;
            if thread != work.thread_id || turn != work.turn_id {
                return Err(BindingMissing);
            }
            let active = self
                .store
                .native_turn_active(&work.run_id)
                .map_err(|_| SourceUnreadable)?;
            (indexed.or(launch), active)
        } else {
            (indexed, false)
        };
        let path = launch
            .and_then(|app| app.codex_home)
            .filter(|path| path.is_absolute())
            .ok_or(BindingMissing)?;
        let index = self
            .homes
            .entry(path.clone())
            .or_insert_with(|| rollout_index(&path));
        let file = match index.as_ref().map_err(|error| *error)?.get(&work.thread_id) {
            Some(Some(path)) => File::open(path).map_err(|error| match error.kind() {
                io::ErrorKind::NotFound => SourceMissing,
                _ => SourceUnreadable,
            })?,
            Some(None) => return Err(SourceAmbiguous),
            None => return Err(SourceMissing),
        };
        native_snapshot(file, &work.thread_id, &work.turn_id, active)
            .map(|records| (records, active))
    }
}

/// Index only Codex's two session directories in the frozen home. Do not
/// descend through symlinked entries or trust any pathname supplied by Server.
fn rollout_index(home: &Path) -> Result<Rollouts, TokenSourceGap> {
    let mut result = Rollouts::new();
    let mut dirs = vec![home.join("sessions"), home.join("archived_sessions")];
    while let Some(dir) = dirs.pop() {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => return Err(SourceUnreadable),
        };
        for entry in entries {
            let entry = entry.map_err(|_| SourceUnreadable)?;
            let kind = entry.file_type().map_err(|_| SourceUnreadable)?;
            if kind.is_dir() {
                dirs.push(entry.path());
            } else if kind.is_file()
                && let Some(name) = entry.file_name().to_str()
                // Codex normally uses UUIDs, but its thread IDs are opaque
                // here. Index by the suffix after its fixed ISO timestamp.
                && let Some(thread) = rollout_thread(name)
            {
                result
                    .entry(thread.to_owned())
                    .and_modify(|slot| *slot = None)
                    .or_insert_with(|| Some(entry.path()));
            }
        }
    }
    Ok(result)
}

fn rollout_thread(name: &str) -> Option<&str> {
    let stem = name.strip_prefix("rollout-")?.strip_suffix(".jsonl")?;
    let (date, thread) = stem.split_at_checked(20)?;
    (date.ends_with('-') && !thread.is_empty()).then_some(thread)
}
