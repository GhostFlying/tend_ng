//! Explicit history maintenance. The only files considered belong to a
//! previously recorded Tend attempt. No discovery runs on the read path.
use super::{Source, Store, bind_child, persist};
use anyhow::Result;
use codex_app::{
    ProcessIdentity, rollout_home,
    usage::{ChildRollouts, Rollouts, native_snapshot, rollout_index},
    validate_home,
};
use protocol::{NativeTokenRecord, RequestMeter, StartTokenBackfill, TokenSourceGap};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    sync::mpsc::{SyncSender, sync_channel},
    thread,
};

pub(in crate::store) fn enqueue(db: &Connection, job: &str) -> Result<()> {
    db.execute(
        "INSERT INTO token_agent_backfill(job, attempt)
         SELECT ?1, a.id FROM token_agent_attempts a LEFT JOIN token_agent_turns t
           ON a.thread=t.thread AND a.turn=t.turn
         WHERE t.attempt=a.id OR a.turn IS NULL",
        [job],
    )?;
    Ok(())
}

pub(in crate::store) fn backfill_status(db: &Connection, job: &str) -> Result<Value> {
    let mut query = db.prepare_cached(
        "SELECT a.source, b.state, b.detail, count(*), sum(b.responses)
         FROM token_agent_backfill b JOIN token_agent_attempts a ON a.id=b.attempt
         WHERE b.job=?1 GROUP BY a.source,b.state,b.detail ORDER BY a.source,b.state,b.detail",
    )?;
    let rows = query.query_map([job], |r| {
        Ok(json!({
            "source":r.get::<_, String>(0)?, "state":r.get::<_, String>(1)?,
            "detail":r.get::<_, Option<String>>(2)?, "attempts":r.get::<_, i64>(3)?,
            "native_keys":r.get::<_, i64>(4)?
        }))
    })?;
    Ok(Value::Array(rows.collect::<rusqlite::Result<_>>()?))
}

pub(crate) fn spawn_history(
    db: &Path,
    router_home: PathBuf,
    discussion_home: Option<PathBuf>,
) -> Result<SyncSender<()>> {
    let mut store = Store::open(db)?;
    let (wake, pending) = sync_channel(1);
    thread::spawn(move || {
        for () in pending {
            if let Err(error) = drain(&mut store, &router_home, discussion_home.as_deref()) {
                eprintln!("Server model usage history: {error:#}");
            }
        }
    });
    // Only resume explicit jobs already accepted by a previous Server.
    let _ = wake.try_send(());
    Ok(wake)
}

struct Item {
    job: StartTokenBackfill,
    attempt: String,
    source: String,
    thread: String,
    turn: Option<String>,
    rollout: Option<PathBuf>,
    codex_home: Option<PathBuf>,
    process: Option<ProcessIdentity>,
    settled: bool,
}

fn next(db: &Connection) -> Result<Option<Item>> {
    let mut query = db.prepare_cached(
        "SELECT j.request, a.id, a.source, a.thread, a.turn, a.rollout,
                a.process_pid, a.process_start, i.outcome IS NOT NULL, f.codex_home
         FROM token_agent_backfill b JOIN token_backfill_jobs j ON b.job=j.id
         JOIN token_agent_attempts a ON a.id=b.attempt
         LEFT JOIN token_feishu_attempt_launch f ON f.attempt=a.id
         LEFT JOIN router_attempts ra ON ra.id=a.id LEFT JOIN router_inputs i ON ra.seq=i.seq
         WHERE b.state='pending' ORDER BY b.job,b.attempt LIMIT 1",
    )?;
    let row = query
        .query_row([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<u32>>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get(8)?,
                r.get::<_, Option<String>>(9)?,
            ))
        })
        .optional()?;
    row.map(
        |(job, attempt, source, thread, turn, rollout, pid, started, settled, codex_home)| {
            Ok(Item {
                job: serde_json::from_str(&job)?,
                attempt,
                source,
                thread,
                turn,
                rollout: rollout.map(PathBuf::from),
                codex_home: codex_home.map(PathBuf::from),
                process: pid
                    .zip(started)
                    .map(|(pid, started_at)| ProcessIdentity { pid, started_at }),
                settled,
            })
        },
    )
    .transpose()
}

type Indexes = HashMap<PathBuf, Result<Rollouts, TokenSourceGap>>;

fn io_gap(error: io::Error) -> TokenSourceGap {
    if error.kind() == io::ErrorKind::NotFound {
        TokenSourceGap::SourceMissing
    } else {
        TokenSourceGap::SourceUnreadable
    }
}

fn original_home<'a>(
    item: &'a Item,
    configured: Option<&'a Path>,
) -> Result<&'a Path, TokenSourceGap> {
    use TokenSourceGap::*;
    // The saved path names the original home, even after a service changes
    // its config or Codex archives the file. Never rescan a different home.
    let home = match &item.rollout {
        Some(path) => rollout_home(path).ok_or(BindingMissing)?,
        None => configured.ok_or(BindingMissing)?,
    };
    let info = fs::symlink_metadata(home).map_err(io_gap)?;
    if !info.is_dir() || validate_home(home).ok().as_deref() != Some(home) {
        return Err(SourceUnreadable);
    }
    Ok(home)
}

fn open_rollout(item: &Item, home: &Path, indexes: &mut Indexes) -> Result<File, TokenSourceGap> {
    use TokenSourceGap::*;
    let open = |path: &Path| {
        if !fs::symlink_metadata(path).map_err(io_gap)?.is_file()
            || path.canonicalize().map_err(io_gap)? != path
        {
            return Err(SourceUnreadable);
        }
        File::open(path).map_err(io_gap)
    };
    if let Some(path) = &item.rollout {
        match open(path) {
            Err(SourceMissing) => {}
            result => return result,
        }
    }
    let index = indexes
        .entry(home.to_owned())
        .or_insert_with(|| rollout_index(home))
        .as_ref()
        .map_err(|error| *error)?;
    match index.get(&item.thread) {
        Some(Some(path)) => open(path),
        Some(None) => Err(SourceAmbiguous),
        None => Err(SourceMissing),
    }
}

pub(super) fn drain(store: &mut Store, router: &Path, discussion: Option<&Path>) -> Result<()> {
    let mut indexes = Indexes::new();
    let mut children = HashMap::<PathBuf, ChildRollouts>::new();
    let mut previous = String::new();
    while let Some(item) = next(&store.db)? {
        if previous != item.job.job_id {
            indexes.clear();
            children.clear();
            previous.clone_from(&item.job.job_id);
        }
        let Some(turn) = &item.turn else {
            finish(&store.db, &item, "gap", Some("turn_identity_missing"), 0)?;
            continue;
        };
        if item.source == Source::Feishu.key() && item.codex_home.is_none() {
            finish(&store.db, &item, "gap", Some("ephemeral_no_history"), 0)?;
            continue;
        }
        let configured_home = match item.source.as_str() {
            source if source == Source::Feishu.key() => item.codex_home.as_deref(),
            source if source == Source::Router.key() => Some(router),
            _ => discussion,
        };
        let home = match original_home(&item, configured_home) {
            Ok(home) => home,
            Err(gap) => {
                let reason = serde_json::to_value(gap)?;
                finish(&store.db, &item, "gap", reason.as_str(), 0)?;
                continue;
            }
        };
        let active = !item.settled
            && item
                .process
                .as_ref()
                .is_none_or(|process| !process.is_stopped().unwrap_or(false));
        let root = open_rollout(&item, home, &mut indexes)
            .and_then(|file| native_snapshot(file, &item.thread, turn, active));
        let (mut records, child_gap) = match indexes
            .entry(home.to_owned())
            .or_insert_with(|| rollout_index(home))
        {
            Ok(rollouts) => children
                .entry(home.to_owned())
                .or_insert_with(|| ChildRollouts::from_rollouts(rollouts))
                .snapshot(&item.thread, turn, active),
            Err(reason) => (Vec::new(), Some(*reason)),
        };
        let gap = match root {
            Ok(root) => {
                records.extend(root);
                child_gap
            }
            Err(gap) if records.is_empty() => {
                let reason = serde_json::to_value(gap)?;
                finish(&store.db, &item, "gap", reason.as_str(), 0)?;
                continue;
            }
            Err(gap) => Some(gap),
        };
        apply(store, &item, turn, &records, active, gap)?;
    }
    Ok(())
}

fn apply(
    store: &mut Store,
    item: &Item,
    turn: &str,
    records: &[NativeTokenRecord],
    active: bool,
    gap: Option<TokenSourceGap>,
) -> Result<()> {
    let mut invalid = false;
    let mut conflicts = false;
    for batch in records.chunks(protocol::RUNNER_SYNC_BATCH) {
        let tx = store
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for native in batch {
            let Some(counts) = native.counts.filter(|n| {
                (n.valid() || n.missing()) && !native.invalid && native.variant.is_none()
            }) else {
                invalid = true;
                continue;
            };
            let meter = RequestMeter {
                counts,
                model: native.model.clone(),
                provider_counts: false,
            };
            if let Some(child) = &native.child {
                let key = (child.thread_id.as_str(), child.turn_id.as_str());
                if let Some(owner) = bind_child(&tx, &item.attempt, (&item.thread, turn), key)? {
                    conflicts |= persist(
                        &tx,
                        &owner,
                        key.0,
                        key.1,
                        &native.response_id,
                        meter,
                        Some(item.job.policy),
                    )?;
                } else {
                    conflicts = true;
                }
            } else {
                conflicts |= persist(
                    &tx,
                    &item.attempt,
                    &item.thread,
                    turn,
                    &native.response_id,
                    meter,
                    Some(item.job.policy),
                )?;
            }
        }
        tx.commit()?;
    }
    let gap = gap.map(serde_json::to_value).transpose()?;
    let detail = if invalid {
        Some("invalid_native_keys")
    } else if conflicts {
        Some("conflicting_request_evidence")
    } else {
        gap.as_ref().and_then(Value::as_str)
    };
    finish(
        &store.db,
        item,
        if active {
            "active_prefix"
        } else if detail.is_some() {
            "gap"
        } else {
            "scanned"
        },
        detail,
        records.len(),
    )
}

fn finish(
    db: &Connection,
    item: &Item,
    state: &str,
    detail: Option<&str>,
    found: usize,
) -> Result<()> {
    db.execute(
        "UPDATE token_agent_backfill SET state=?3, detail=?4, responses=?5 WHERE job=?1 AND attempt=?2",
        params![item.job.job_id, item.attempt, state, detail, found],
    )?;
    Ok(())
}
