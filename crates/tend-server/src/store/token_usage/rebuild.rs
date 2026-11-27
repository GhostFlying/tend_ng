//! Snapshot authoritative facts without taking the writer lock. Reconcile
//! changes by revision into shadows and publish both projections together.
use super::{
    Summary, Tally,
    backfill::{SCOPE_JOINS, scope_filter},
    overview,
};
use crate::store::{Store, timing};
use anyhow::{Context, Result};
use protocol::{RequestMeter, TokenUsageScope};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

const STAGE_BATCH: usize = 128;
const DIRTY_BATCH: i64 = 32;

pub(in crate::store) fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(include_str!("rebuild.sql"))?;
    Ok(())
}

pub(in crate::store) fn abandon(db: &Connection) -> Result<()> {
    db.execute(
        "UPDATE token_rebuild_state SET collecting = 0 WHERE id = 1 AND collecting",
        [],
    )?;
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Level {
    Direct,
    Subtree,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Subtree => "subtree",
        }
    }
}

type IssueKey = (String, Level);
type ScopeIssues = BTreeSet<String>;

enum RunTally {
    Rebuilt(Tally),
    Stored(Option<(String, i64)>),
}

impl RunTally {
    fn decode(&self) -> Result<Tally> {
        match self {
            Self::Rebuilt(tally) => Ok(tally.clone()),
            Self::Stored(None) => Ok(Tally::default()),
            Self::Stored(Some((raw, _))) => Ok(serde_json::from_str(raw)?),
        }
    }
}

struct Run {
    issue: String,
    legacy: bool,
    tally: RunTally,
}

impl Run {
    fn selected(&self) -> bool {
        matches!(self.tally, RunTally::Rebuilt(_))
    }

    fn summary(&self) -> Result<Summary> {
        let tally = self.tally.decode()?;
        let observed = tally.tokens.valid_records > 0;
        Ok(Summary {
            started_runs: 1,
            observed_runs: i64::from(observed),
            legacy_unobserved: i64::from(self.legacy && !observed),
            tally,
        })
    }
}

#[derive(Default)]
struct Snapshot {
    runs: BTreeMap<String, Run>,
    totals: BTreeMap<IssueKey, Summary>,
    ancestors: BTreeMap<String, Vec<String>>,
    direct: ScopeIssues,
    subtree: ScopeIssues,
    global: Summary,
}

impl Snapshot {
    fn keys(&self, issue: &str) -> Vec<IssueKey> {
        let mut keys = vec![(issue.to_owned(), Level::Direct)];
        if let Some(ancestors) = self.ancestors.get(issue) {
            keys.extend(
                ancestors
                    .iter()
                    .map(|ancestor| (ancestor.clone(), Level::Subtree)),
            );
        }
        keys
    }

    fn apply(&mut self, keys: &[IssueKey], summary: &Summary, sign: i64) {
        for key in keys {
            let total = self.totals.entry(key.clone()).or_default();
            total.merge_signed(summary, sign);
        }
    }

    fn replace(&mut self, id: String, run: Run) -> Result<Vec<IssueKey>> {
        let keys = self.keys(&run.issue);
        let initialized = keys
            .iter()
            .filter(|key| self.totals.contains_key(*key))
            .cloned()
            .collect::<Vec<_>>();
        if !initialized.is_empty() {
            if let Some(previous) = self.runs.get(&id) {
                self.apply(&initialized, &previous.summary()?, -1);
            }
            self.apply(&initialized, &run.summary()?, 1);
        }
        if run.selected() {
            self.repair_issue(&run.issue);
        }
        self.runs.insert(id, run);
        Ok(keys)
    }

    fn initialize_missing_totals(&mut self) -> Result<()> {
        let mut missing = self
            .direct
            .iter()
            .map(|id| (id.clone(), Level::Direct))
            .chain(self.subtree.iter().map(|id| (id.clone(), Level::Subtree)))
            .filter(|key| !self.totals.contains_key(key))
            .map(|key| (key, Summary::default()))
            .collect::<BTreeMap<_, _>>();
        if missing.is_empty() {
            return Ok(());
        }
        // A Run that arrived during a narrow rebuild can expand its repair
        // scope. Initialize those new keys from all their captured contributors.
        for run in self.runs.values() {
            let keys = self.keys(&run.issue);
            let mut summary = None;
            for key in keys {
                if let Some(total) = missing.get_mut(&key) {
                    let value = match &summary {
                        Some(value) => value,
                        None => summary.insert(run.summary()?),
                    };
                    total.merge(value);
                }
            }
        }
        self.totals.extend(missing);
        Ok(())
    }

    fn repair_issue(&mut self, issue: &str) {
        self.direct.insert(issue.to_owned());
        if let Some(ancestors) = self.ancestors.get(issue) {
            self.subtree.extend(ancestors.iter().cloned());
        }
    }

    fn repairs(&self, key: &IssueKey) -> bool {
        match key.1 {
            Level::Direct => self.direct.contains(&key.0),
            Level::Subtree => self.subtree.contains(&key.0),
        }
    }
}

fn encode(summary: &Summary) -> Result<String> {
    Ok(serde_json::to_string(summary)?)
}

fn tally(db: &Connection, run: &str) -> Result<Tally> {
    let mut requests = db
        .prepare_cached("SELECT last FROM token_requests WHERE run_id = ?1 AND conflict IS NULL")?;
    let mut rows = requests.query([run])?;
    let mut tally = Tally::default();
    while let Some(row) = rows.next()? {
        let raw = row.get_ref(0)?.as_str()?;
        let meter: RequestMeter = serde_json::from_str(raw)?;
        if meter.valid() {
            tally.count(&meter, 1);
        }
    }
    Ok(tally)
}

fn ancestors(db: &Connection, snapshot: &mut Snapshot, issue: &str) -> Result<()> {
    if !snapshot.ancestors.contains_key(issue) {
        let mut query =
            db.prepare_cached("SELECT ancestor FROM token_issue_tree WHERE descendant = ?1")?;
        let values = query
            .query_map([issue], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        snapshot.ancestors.insert(issue.to_owned(), values);
    }
    Ok(())
}

type Rows = BTreeMap<IssueKey, String>;

fn capture(db: &Connection, scope: &TokenUsageScope) -> Result<(Snapshot, Rows)> {
    let (filter, argument) = scope_filter(scope);
    let mut snapshot = Snapshot::default();
    let mut closure = db.prepare("SELECT descendant, ancestor FROM token_issue_tree")?;
    let mut edges = closure.query([])?;
    while let Some(row) = edges.next()? {
        snapshot
            .ancestors
            .entry(row.get(0)?)
            .or_default()
            .push(row.get(1)?);
    }
    if matches!(scope, TokenUsageScope::All | TokenUsageScope::Issue(_)) {
        let ids = snapshot
            .ancestors
            .iter()
            .filter(|(_, ancestors)| {
                matches!(scope, TokenUsageScope::All)
                    || argument.is_some_and(|issue| ancestors.iter().any(|id| id == issue))
            })
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for issue in ids {
            snapshot.repair_issue(&issue);
        }
    }
    let mut cursor = db.prepare(&format!(
        "SELECT r.run_id, r.issue_id, r.legacy, p.tally, p.valid_records, coalesce(({filter}), 0)
         {SCOPE_JOINS} LEFT JOIN token_run_tallies p ON p.run_id = r.run_id"
    ))?;
    let mut rows = cursor.query([argument])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let selected: bool = row.get(5)?;
        let issue: String = row.get(1)?;
        let tally = if selected {
            snapshot.repair_issue(&issue);
            RunTally::Rebuilt(tally(db, &id)?)
        } else {
            RunTally::Stored(row.get::<_, Option<String>>(3)?.zip(row.get(4)?))
        };
        snapshot.runs.insert(
            id,
            Run {
                issue,
                legacy: row.get(2)?,
                tally,
            },
        );
    }
    snapshot.initialize_missing_totals()?;
    let mut copy = Rows::new();
    if !matches!(scope, TokenUsageScope::All) {
        let mut query = db.prepare("SELECT issue_id, scope, summary FROM token_issue_rollups")?;
        let mut rows = query.query([])?;
        while let Some(row) = rows.next()? {
            let level = if row.get_ref(1)?.as_str()? == "direct" {
                Level::Direct
            } else {
                Level::Subtree
            };
            let key = (row.get(0)?, level);
            if !snapshot.repairs(&key) {
                copy.insert(key, row.get(2)?);
            }
        }
    }
    for (issues, level) in [
        (&snapshot.direct, Level::Direct),
        (&snapshot.subtree, Level::Subtree),
    ] {
        for issue in issues {
            let key = (issue.clone(), level);
            let summary = encode(snapshot.totals.get(&key).unwrap_or(&Summary::default()))?;
            copy.insert(key, summary);
        }
    }
    Ok((snapshot, copy))
}

fn stage_run(tx: &Connection, id: &str, tally: &RunTally) -> Result<()> {
    let encoded;
    let (summary, count): (&str, i64) = match tally {
        RunTally::Rebuilt(tally) => {
            encoded = serde_json::to_string(tally)?;
            (&encoded, tally.tokens.valid_records)
        }
        RunTally::Stored(Some((summary, count))) => (summary, *count),
        RunTally::Stored(None) => {
            tx.prepare_cached("DELETE FROM token_rebuild_runs WHERE run_id = ?1")?
                .execute([id])?;
            return Ok(());
        }
    };
    tx.prepare_cached("INSERT INTO token_rebuild_runs VALUES (?1, ?2, ?3)
        ON CONFLICT(run_id) DO UPDATE SET tally = excluded.tally, valid_records = excluded.valid_records")?
        .execute(params![id, summary, count])?;
    Ok(())
}

fn stage_issue(tx: &Connection, key: &IssueKey, summary: Option<&str>) -> Result<()> {
    if let Some(summary) = summary {
        tx.prepare_cached(
            "INSERT INTO token_rebuild_issues VALUES (?1, ?2, ?3)
            ON CONFLICT(issue_id, scope) DO UPDATE SET summary = excluded.summary",
        )?
        .execute(params![key.0, key.1.as_str(), summary])?;
    } else {
        tx.prepare_cached("DELETE FROM token_rebuild_issues WHERE issue_id = ?1 AND scope = ?2")?
            .execute(params![key.0, key.1.as_str()])?;
    }
    Ok(())
}

/// Let a waiting ordinary writer compete between maintenance locks.
fn yield_writer() {
    std::thread::sleep(Duration::from_millis(1));
}

fn clear_shadows(db: &mut Connection) -> Result<()> {
    for sql in [
        "DELETE FROM token_rebuild_dirty WHERE run_id IN (SELECT run_id FROM token_rebuild_dirty LIMIT ?1)",
        "DELETE FROM token_rebuild_runs WHERE run_id IN (SELECT run_id FROM token_rebuild_runs LIMIT ?1)",
        "DELETE FROM token_rebuild_issues WHERE (issue_id, scope) IN (SELECT issue_id, scope FROM token_rebuild_issues LIMIT ?1)",
    ] {
        loop {
            let (tx, clock) = timing::begin(db, "token_rebuild_clear")?;
            let removed = tx.execute(sql, [STAGE_BATCH])?;
            timing::commit(tx, clock)?;
            if removed < STAGE_BATCH {
                break;
            }
            yield_writer();
        }
    }
    Ok(())
}

fn stage_snapshot(db: &mut Connection, snapshot: &mut Snapshot, issues: Rows) -> Result<()> {
    let runs = snapshot.runs.iter().collect::<Vec<_>>();
    for batch in runs.chunks(STAGE_BATCH) {
        let (tx, clock) = timing::begin(db, "token_rebuild_stage")?;
        for (id, run) in batch {
            stage_run(&tx, id, &run.tally)?;
        }
        timing::commit(tx, clock)?;
        yield_writer();
    }
    stage_issues(
        db,
        "token_rebuild_stage",
        &issues
            .into_iter()
            .map(|(key, value)| (key, Some(value)))
            .collect::<Vec<_>>(),
        &mut snapshot.global,
    )
}

fn stage_issues(
    db: &mut Connection,
    operation: &'static str,
    modifications: &[(IssueKey, Option<String>)],
    global: &mut Summary,
) -> Result<()> {
    for batch in modifications.chunks(STAGE_BATCH) {
        let (tx, clock) = timing::begin(db, operation)?;
        let mut next = global.clone();
        for (key, summary) in batch {
            if key.1 == Level::Direct {
                // Use the exact rows being published, including untouched
                // Issues in a scoped rebuild. Subtrees would double count.
                let previous: Option<String> = tx.prepare_cached(
                    "SELECT summary FROM token_rebuild_issues WHERE issue_id = ?1 AND scope = 'direct'",
                )?.query_row([&key.0], |row| row.get(0)).optional()?;
                for (raw, sign) in [(previous.as_deref(), -1), (summary.as_deref(), 1)] {
                    if let Some(raw) = raw {
                        next.merge_signed(&serde_json::from_str(raw)?, sign);
                    }
                }
            }
            stage_issue(&tx, key, summary.as_deref())?;
        }
        timing::commit(tx, clock)?;
        *global = next;
        yield_writer();
    }
    Ok(())
}

fn catch_up(
    read: &mut Connection,
    write: &mut Connection,
    scope: &TokenUsageScope,
    snapshot: &mut Snapshot,
) -> Result<bool> {
    let tx = read.transaction()?;
    let dirty = {
        let mut query =
            tx.prepare_cached("SELECT run_id, revision FROM token_rebuild_dirty LIMIT ?1")?;
        query
            .query_map([DIRTY_BATCH], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    if dirty.is_empty() {
        return Ok(false);
    }
    let (filter, argument) = scope_filter(scope);
    let mut changed = BTreeSet::new();
    {
        let mut query = tx.prepare_cached(&format!(
            "SELECT r.issue_id, r.legacy, p.tally, p.valid_records, coalesce(({filter}), 0)
             {SCOPE_JOINS} LEFT JOIN token_run_tallies p ON p.run_id = r.run_id WHERE r.run_id = ?2"
        ))?;
        for (id, _) in &dirty {
            let run = query.query_row(params![argument, id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, bool>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, bool>(4)?,
                ))
            })?;
            ancestors(&tx, snapshot, &run.0)?;
            let tally = if run.4 {
                RunTally::Rebuilt(tally(&tx, id)?)
            } else {
                RunTally::Stored(run.2.zip(run.3))
            };
            changed.extend(snapshot.replace(
                id.clone(),
                Run {
                    issue: run.0,
                    legacy: run.1,
                    tally,
                },
            )?);
        }
    }
    snapshot.initialize_missing_totals()?;
    let mut modifications = Vec::new();
    {
        let mut query = tx.prepare_cached(
            "SELECT summary FROM token_issue_rollups WHERE issue_id = ?1 AND scope = ?2",
        )?;
        for key in changed {
            let summary = if snapshot.repairs(&key) {
                Some(encode(&snapshot.totals[&key])?)
            } else {
                query
                    .query_row(params![key.0, key.1.as_str()], |row| row.get(0))
                    .optional()?
            };
            modifications.push((key, summary));
        }
    }
    tx.commit()?;
    // These rows are not published. Bound work by rows, not by Issue
    // depth/fan-out. Finish all shadow writes before confirming any revision.
    let (tx, clock) = timing::begin(write, "token_rebuild_catchup")?;
    for (id, _) in &dirty {
        stage_run(&tx, id, &snapshot.runs[id].tally)?;
    }
    timing::commit(tx, clock)?;
    yield_writer();
    stage_issues(
        write,
        "token_rebuild_catchup_issues",
        &modifications,
        &mut snapshot.global,
    )?;

    // If a fact changed during staging, the newer revision is still dirty.
    let (tx, clock) = timing::begin(write, "token_rebuild_catchup_ack")?;
    for (id, revision) in dirty {
        tx.prepare_cached("DELETE FROM token_rebuild_dirty WHERE run_id = ?1 AND revision = ?2")?
            .execute(params![id, revision])?;
    }
    timing::commit(tx, clock)?;
    yield_writer();
    Ok(true)
}

fn publish(db: &mut Connection, global: &Summary) -> Result<bool> {
    let (tx, clock) = timing::begin(db, "token_rebuild_publish")?;
    if tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM token_rebuild_dirty)",
        [],
        |row| row.get::<_, bool>(0),
    )? {
        return Ok(false);
    }
    tx.execute_batch(
        "ALTER TABLE token_run_tallies RENAME TO token_rebuild_run_swap;
         ALTER TABLE token_rebuild_runs RENAME TO token_run_tallies;
         ALTER TABLE token_rebuild_run_swap RENAME TO token_rebuild_runs;
         ALTER TABLE token_issue_rollups RENAME TO token_rebuild_issue_swap;
         ALTER TABLE token_rebuild_issues RENAME TO token_issue_rollups;
         ALTER TABLE token_rebuild_issue_swap RENAME TO token_rebuild_issues;
         UPDATE token_rebuild_state SET collecting = 0 WHERE id = 1;",
    )?;
    overview::write_global(&tx, global)?;
    timing::commit(tx, clock)?;
    Ok(true)
}

impl Store {
    pub fn rebuild_token_usage(&mut self, scope: &TokenUsageScope) -> Result<Value> {
        // Resolve short Issue number once for membership and empty descendants.
        let resolved = match scope {
            TokenUsageScope::Issue(issue) => self
                .resolve_issue(issue)?
                .map(|id| TokenUsageScope::Issue(id.into_owned())),
            _ => Some(scope.clone()),
        };
        let Some(resolved) = resolved else {
            return Ok(json!({"scope":scope,"runs":0,"direct_issues":0,"subtree_issues":0}));
        };
        let path = self
            .db
            .path()
            .context("token rebuild needs a persisted Server database")?;
        let mut read = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        read.busy_timeout(Duration::from_secs(5))?;
        abandon(&self.db)?;
        clear_shadows(&mut self.db)?;
        let (tx, clock) = timing::begin(&mut self.db, "token_rebuild_begin")?;
        tx.execute(
            "UPDATE token_rebuild_state SET collecting = 1 WHERE id = 1",
            [],
        )?;
        timing::commit(tx, clock)?;
        let result = (|| {
            let tx = read.transaction()?;
            let (mut snapshot, issues) = capture(&tx, &resolved)?;
            tx.commit()?;
            stage_snapshot(&mut self.db, &mut snapshot, issues)?;
            loop {
                if !catch_up(&mut read, &mut self.db, &resolved, &mut snapshot)?
                    && publish(&mut self.db, &snapshot.global)?
                {
                    break;
                }
            }
            Ok(
                json!({"scope":scope,"runs":snapshot.runs.values().filter(|r| r.selected()).count(),
                "direct_issues":snapshot.direct.len(),"subtree_issues":snapshot.subtree.len()}),
            )
        })();
        if result.is_err() {
            abandon(&self.db)?;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::token_usage::tests::{
        Fixture, assignment, base, issue, native_run, observation, sync, view,
    };

    #[test]
    fn shadow_publish_catches_concurrent_writes_and_readers_keep_complete_snapshots() -> Result<()>
    {
        let file = Fixture::new()?;
        let mut writer = Store::initialize(&file.file())?;
        base(&writer.db)?;
        issue(&writer.db, "root", None)?;
        issue(&writer.db, "left", Some("root"))?;
        issue(&writer.db, "right", Some("root"))?;
        assignment(&writer.db, "a", "left", "runner")?;
        assignment(&writer.db, "b", "right", "runner")?;
        let a = native_run("run-a", "a");
        let b = native_run("run-b", "b");
        let a0 = observation(&a, 0, Some(10));
        let b0 = observation(&b, 0, Some(1));
        let a1 = observation(&a, 1, Some(6));
        let b1 = observation(&b, 1, Some(2));
        sync(&mut writer, "runner", &[a, b], &[a0, b0])?;
        // A stale root report stays published until both shadows are ready.
        writer.db.execute(
            "DELETE FROM token_issue_rollups WHERE issue_id = 'root' AND scope = 'subtree'",
            [],
        )?;
        writer
            .db
            .execute("UPDATE token_rebuild_state SET collecting = 1", [])?;
        let mut read = Connection::open_with_flags(file.file(), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut held = Connection::open_with_flags(file.file(), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let scope = TokenUsageScope::Run("run-a".into());
        let (mut snapshot, issues) = {
            let tx = read.transaction()?;
            capture(&tx, &scope)?
        };
        sync(&mut writer, "runner", &[], &[a1.clone(), b1, a1])?;
        issue(&writer.db, "leaf", Some("right"))?;
        assignment(&writer.db, "c", "leaf", "runner")?;
        let c = native_run("run-c", "c");
        let c0 = observation(&c, 0, Some(4));
        sync(&mut writer, "runner", &[c], &[c0])?;
        writer.db.execute("UPDATE token_global_rollup SET summary = (SELECT summary FROM token_issue_rollups WHERE issue_id = 'left' AND scope = 'direct')", [])?;
        stage_snapshot(&mut writer.db, &mut snapshot, issues)?;
        assert!(!publish(&mut writer.db, &snapshot.global)?);
        let before = view(&writer, "root")?;
        assert_eq!(before["subtree"]["total_tokens"]["tokens"], "12");
        let held = held.transaction()?;
        let root_summary = |db: &Connection| -> rusqlite::Result<String> {
            db.query_row("SELECT summary FROM token_issue_rollups WHERE issue_id = 'root' AND scope = 'subtree'", [], |r| r.get(0))
        };
        let old = root_summary(&held)?;
        let global_before = overview::read_global(&held)?;
        assert_eq!(global_before.tally.tokens.sums[0], 16);
        while catch_up(&mut read, &mut writer.db, &scope, &mut snapshot)? {}
        assert_eq!(view(&writer, "root")?, before);
        assert_eq!(overview::read_global(&writer.db)?.tally.tokens.sums[0], 16);
        assert!(publish(&mut writer.db, &snapshot.global)?);
        assert_eq!(root_summary(&held)?, old);
        assert_eq!(overview::read_global(&held)?.tally.tokens.sums[0], 16);
        assert_eq!(overview::read_global(&writer.db)?.tally.tokens.sums[0], 23);
        assert_ne!(root_summary(&writer.db)?, old);
        let rebuilt = view(&writer, "root")?;
        assert_eq!(rebuilt["subtree"]["reported_requests"], 5);
        assert_eq!(rebuilt["subtree"]["total_tokens"]["tokens"], "23");
        assert_eq!(
            view(&writer, "right")?["subtree"]["total_tokens"]["tokens"],
            "7"
        );
        held.commit()?;
        // A second rebuild may clear a previously published shadow, then
        // republish exactly the same counts.
        writer.rebuild_token_usage(&TokenUsageScope::All)?;
        assert_eq!(view(&writer, "root")?, rebuilt);
        assert_eq!(overview::read_global(&writer.db)?.tally.tokens.sums[0], 23);
        Ok(())
    }

    #[test]
    fn scoped_rebuild_copies_unrelated_corrupt_tallies_without_decoding() -> Result<()> {
        let file = Fixture::new()?;
        let mut writer = Store::initialize(&file.file())?;
        base(&writer.db)?;
        issue(&writer.db, "left", None)?;
        issue(&writer.db, "right", None)?;
        assignment(&writer.db, "a", "left", "runner")?;
        assignment(&writer.db, "b", "right", "runner")?;
        let a = native_run("run-a", "a");
        let b = native_run("run-b", "b");
        let usage = [observation(&a, 0, Some(10)), observation(&b, 0, Some(1))];
        sync(&mut writer, "runner", &[a, b], &usage)?;
        let expected = view(&writer, "left")?;
        let unrelated = view(&writer, "right")?;
        writer.db.execute_batch(
            "DELETE FROM token_issue_rollups WHERE issue_id = 'left';
             DELETE FROM token_run_tallies WHERE run_id = 'run-a';
             UPDATE token_run_tallies SET tally = '{}', valid_records = 19 WHERE run_id = 'run-b';",
        )?;
        let reply = writer.rebuild_token_usage(&TokenUsageScope::Run("run-a".into()))?;
        assert_eq!(reply["runs"], 1);
        assert_eq!(view(&writer, "left")?, expected);
        assert_eq!(view(&writer, "right")?, unrelated);
        assert_eq!(overview::read_global(&writer.db)?.tally.tokens.sums[0], 11);
        assert_eq!(
            writer.db.query_row(
                "SELECT tally, valid_records FROM token_run_tallies WHERE run_id = 'run-b'",
                [],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
            )?,
            ("{}".into(), 19)
        );
        Ok(())
    }

    #[test]
    fn catchup_commits_fanout_in_chunks_and_acks_only_after_all_shadow_rows() -> Result<()> {
        let file = Fixture::new()?;
        let mut writer = Store::initialize(&file.file())?;
        base(&writer.db)?;
        issue(&writer.db, "root", None)?;
        let mut runs = Vec::new();
        let mut usages = Vec::new();
        for branch in 0..DIRTY_BATCH {
            let mut parent = "root".to_owned();
            for level in 0..4 {
                let child = format!("b-{branch:02}-{level}");
                issue(&writer.db, &child, Some(&parent))?;
                parent = child;
            }
            let task = format!("task-{branch:02}");
            assignment(&writer.db, &task, &parent, "runner")?;
            let run = native_run(&format!("run-{branch:02}"), &task);
            usages.push(observation(&run, 0, Some(5)));
            runs.push(run);
        }
        sync(&mut writer, "runner", &runs, &[])?;
        writer
            .db
            .execute("UPDATE token_rebuild_state SET collecting = 1", [])?;
        let mut read = Connection::open_with_flags(file.file(), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let scope = TokenUsageScope::All;
        let (mut snapshot, issues) = capture(&read, &scope)?;
        stage_snapshot(&mut writer.db, &mut snapshot, issues)?;
        sync(&mut writer, "runner", &[], &usages)?;
        // The root sorts after all branch keys: it belongs to the second
        // Issue chunk. A failure there must not roll back the first shadow
        // chunk or acknowledge versions whose projections are still incomplete.
        writer.db.execute_batch(
            "CREATE TEMP TRIGGER reject_last_shadow BEFORE INSERT ON token_rebuild_issues
             WHEN NEW.issue_id = 'root' AND NEW.scope = 'subtree'
             BEGIN SELECT RAISE(ABORT, 'synthetic later chunk failure'); END;",
        )?;
        let error = catch_up(&mut read, &mut writer.db, &scope, &mut snapshot).unwrap_err();
        assert!(error.to_string().contains("synthetic later chunk failure"));
        assert_eq!(
            writer
                .db
                .query_row("SELECT count(*) FROM token_rebuild_dirty", [], |r| r
                    .get::<_, i64>(0))?,
            DIRTY_BATCH
        );
        let shadow: String = writer.db.query_row(
            "SELECT summary FROM token_rebuild_issues WHERE issue_id='b-00-3' AND scope='direct'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(
            serde_json::from_str::<Summary>(&shadow)?
                .tally
                .tokens
                .valid_records,
            1
        );
        assert!(!publish(&mut writer.db, &snapshot.global)?);
        writer.db.execute_batch("DROP TRIGGER reject_last_shadow")?;
        let more = observation(&runs[31], 1, Some(7));
        sync(&mut writer, "runner", &[], &[more.clone(), more])?;
        let expected = view(&writer, "root")?;
        assert!(catch_up(&mut read, &mut writer.db, &scope, &mut snapshot)?);
        assert!(publish(&mut writer.db, &snapshot.global)?);
        assert_eq!(view(&writer, "root")?, expected);
        assert_eq!(expected["subtree"]["reported_requests"], DIRTY_BATCH + 1);
        assert_eq!(expected["subtree"]["total_tokens"]["tokens"], "167");
        let total = overview::read_global(&writer.db)?;
        assert_eq!(total.tally.tokens.valid_records, DIRTY_BATCH + 1);
        assert_eq!(total.tally.tokens.sums[0], 167);
        Ok(())
    }
}
