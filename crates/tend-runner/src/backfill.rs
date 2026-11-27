use crate::{config::Config, inspect::indexed_launch, store::Store, usage::native_snapshot};
use protocol::{
    NativeTokenRecord, RUNNER_SYNC_BATCH, TokenBackfillContent, TokenBackfillPage,
    TokenBackfillWork,
    TokenSourceGap::{self, *},
};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::File,
    io,
    path::PathBuf,
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
                children: HashMap::new(),
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

use crate::usage::{ChildRollouts, Rollouts, rollout_index};
struct Snapshot {
    item: i64,
    digest: String,
    records: Vec<NativeTokenRecord>,
    active: bool,
    child_gap: Option<TokenSourceGap>,
    child_checked: bool,
}
type NativeScan = (Vec<NativeTokenRecord>, bool, Option<TokenSourceGap>);

struct Scanner {
    store: Store,
    history: PathBuf,
    runner: String,
    job: String,
    homes: HashMap<PathBuf, Result<Rollouts, TokenSourceGap>>,
    children: HashMap<PathBuf, ChildRollouts>,
    cached: Option<Snapshot>,
}

impl Scanner {
    fn read(&mut self, work: &TokenBackfillWork) -> TokenBackfillContent {
        if self.job != work.job_id {
            self.job.clone_from(&work.job_id);
            self.homes.clear();
            self.children.clear();
            self.cached = None;
        }
        if !self.cached.as_ref().is_some_and(|scan| {
            scan.item == work.item && scan.child_checked == work.include_children
        }) {
            self.cached = None;
            match self.snapshot(work) {
                Ok((records, active, child_gap)) => {
                    let bytes = if work.include_children {
                        serde_json::to_vec(&(&records, active, child_gap))
                    } else {
                        serde_json::to_vec(&(&records, active))
                    }
                    .expect("serializable Token evidence");
                    self.cached = Some(Snapshot {
                        item: work.item,
                        digest: format!("{:x}", Sha256::digest(bytes)),
                        records,
                        active,
                        child_gap,
                        child_checked: work.include_children,
                    });
                }
                Err(reason) => return TokenBackfillContent::Gap { reason },
            }
        }
        let scan = self.cached.as_ref().unwrap();
        let Ok(offset) = usize::try_from(work.offset) else {
            return TokenBackfillContent::Gap {
                reason: SourceChanged,
            };
        };
        let Some(rest) = scan.records.get(offset..).filter(|rest| !rest.is_empty()) else {
            return TokenBackfillContent::Gap {
                reason: SourceChanged,
            };
        };
        TokenBackfillContent::Records {
            snapshot: scan.digest.clone(),
            records: rest[..rest.len().min(RUNNER_SYNC_BATCH)].to_vec(),
            done: rest.len() <= RUNNER_SYNC_BATCH,
            prefix: scan.active,
            child_gap: scan.child_gap,
            child_checked: scan.child_checked,
        }
    }

    fn snapshot(&mut self, work: &TokenBackfillWork) -> Result<NativeScan, TokenSourceGap> {
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
        let index = index.as_ref().map_err(|error| *error)?;
        let root = match index.get(&work.thread_id) {
            Some(Some(path)) => File::open(path)
                .map_err(|error| match error.kind() {
                    io::ErrorKind::NotFound => SourceMissing,
                    _ => SourceUnreadable,
                })
                .and_then(|file| native_snapshot(file, &work.thread_id, &work.turn_id, active)),
            Some(None) => Err(SourceAmbiguous),
            None => Err(SourceMissing),
        };
        if !work.include_children {
            return root.map(|rows| (rows, active, None));
        }
        let children = self
            .children
            .entry(path)
            .or_insert_with(|| ChildRollouts::from_rollouts(index));
        let (mut rows, child_gap) = children.snapshot(&work.thread_id, &work.turn_id, active);
        let gap = match root {
            Ok(root) => {
                rows.extend(root);
                child_gap
            }
            Err(reason) if rows.is_empty() => return Err(reason),
            Err(reason) => Some(reason),
        };
        rows.sort_by_cached_key(NativeTokenRecord::cursor);
        Ok((rows, active, gap))
    }
}
