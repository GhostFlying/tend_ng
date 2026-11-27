pub(super) mod backfill;
mod models;
pub(super) mod overview;
pub(super) mod rebuild;
pub(crate) mod system;

use super::Store;
use anyhow::Result;
use protocol::{RequestMeter, RunObservation, RunPhase, RunTokenUsage, TokenCounts};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub(super) fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(include_str!("token_usage/schema.sql"))?;
    // Only accepted Starts establish coverage. A final task_runs row alone
    // cannot distinguish a pre-turn failure from an executed turn.
    db.execute(
        "INSERT INTO token_runs (run_id, assignment_id, issue_id, thread_id, turn_id, legacy, tally)
         SELECT r.id, r.assignment_id, t.issue_id,
                json_extract(e.observation, '$.thread_id'), json_extract(e.observation, '$.turn_id'), 1, ?1
         FROM runner_run_events e
         JOIN task_runs r ON r.id = json_extract(e.observation, '$.run_id')
             AND r.assignment_id = json_extract(e.observation, '$.assignment_id')
         JOIN tasks t ON t.id = r.task_id AND t.id = json_extract(e.observation, '$.task_id')
         WHERE json_extract(e.observation, '$.phase.state') = 'started'
           AND length(json_extract(e.observation, '$.thread_id')) > 0
           AND length(json_extract(e.observation, '$.turn_id')) > 0",
        [serde_json::to_string(&Tally::default())?],
    )?;
    db.execute(
        "WITH direct AS (
             SELECT issue_id, count(*) AS n FROM token_runs GROUP BY issue_id
         ), grouped AS (
             SELECT issue_id, 'direct' AS scope, n FROM direct UNION ALL
             SELECT c.ancestor, 'subtree', sum(d.n) FROM direct d
             JOIN token_issue_tree c ON c.descendant = d.issue_id GROUP BY c.ancestor
         )
         INSERT INTO token_issue_rollups (issue_id, scope, summary)
         SELECT issue_id, scope, json_object('started_runs', n, 'observed_runs', 0,
             'legacy_unobserved', n, 'tally', json(?1)) FROM grouped",
        [serde_json::to_string(&Tally::default())?],
    )?;
    Ok(())
}

#[derive(Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
struct Totals {
    valid_records: i64,
    known: [i64; 6],
    // Native values are i64; aggregate totals need not fit i64 or JS Number.
    sums: [i128; 6],
}

impl Totals {
    fn count(&mut self, counts: TokenCounts, sign: i64) {
        self.valid_records += sign;
        for (i, value) in counts.fields().into_iter().enumerate() {
            if let Some(n) = value {
                self.known[i] += sign;
                self.sums[i] += i128::from(sign) * i128::from(n);
            }
        }
    }

    fn merge(&mut self, other: Self, sign: i64) {
        self.valid_records += sign * other.valid_records;
        for i in 0..self.sums.len() {
            self.known[i] += sign * other.known[i];
            self.sums[i] += i128::from(sign) * other.sums[i];
        }
    }
}

impl Totals {
    fn fields(self) -> [Field; 6] {
        std::array::from_fn(|i| Field {
            tokens: (self.known[i] > 0).then(|| self.sums[i].to_string()),
            reported_requests: self.known[i],
        })
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
struct Tally {
    tokens: Totals,
    model_usage: models::ModelProjection,
}

// Keep the original tally fields at their original location. Avoid serde
// flatten here: its intermediate JSON values cannot represent large i128s.
#[derive(Deserialize, Serialize)]
struct TallyWire<M> {
    valid_records: i64,
    known: [i64; 6],
    sums: [i128; 6],
    #[serde(default = "Default::default", skip_serializing_if = "Option::is_none")]
    model_usage: Option<M>,
}

impl Serialize for Tally {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        TallyWire {
            valid_records: self.tokens.valid_records,
            known: self.tokens.known,
            sums: self.tokens.sums,
            model_usage: (self.model_usage != models::ModelProjection::default())
                .then_some(&self.model_usage),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Tally {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let TallyWire {
            valid_records,
            known,
            sums,
            model_usage,
        } = TallyWire::deserialize(d)?;
        Ok(Self {
            tokens: Totals {
                valid_records,
                known,
                sums,
            },
            model_usage: model_usage.unwrap_or_default(),
        })
    }
}

impl Tally {
    fn count(&mut self, meter: &RequestMeter, sign: i64) {
        self.tokens.count(meter.counts, sign);
        self.model_usage.count(meter, sign);
    }

    fn merge(&mut self, other: &Self, sign: i64) {
        self.tokens.merge(other.tokens, sign);
        self.model_usage.merge(&other.model_usage, sign);
    }
}

#[derive(Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
struct Summary {
    started_runs: i64,
    observed_runs: i64,
    legacy_unobserved: i64,
    tally: Tally,
}

impl Summary {
    fn merge(&mut self, other: &Self) {
        self.merge_signed(other, 1);
    }

    fn merge_signed(&mut self, other: &Self, sign: i64) {
        self.started_runs += sign * other.started_runs;
        self.observed_runs += sign * other.observed_runs;
        self.legacy_unobserved += sign * other.legacy_unobserved;
        self.tally.merge(&other.tally, sign);
    }
}

struct VerifiedRun {
    assignment: String,
    task: String,
    issue: String,
    runner: String,
    thread: String,
    turn: String,
    legacy: bool,
    tally: Tally,
}

fn verified_run(db: &Connection, id: &str) -> Result<Option<VerifiedRun>> {
    let row = db.prepare_cached(
        "SELECT r.assignment_id, a.task_id, r.issue_id, a.runner_id, r.thread_id, r.turn_id, r.legacy, p.tally
         FROM token_runs r JOIN runner_assignments a ON a.id = r.assignment_id
         LEFT JOIN token_run_tallies p ON p.run_id = r.run_id WHERE r.run_id = ?1",
    )?.query_row([id], |row| Ok((
        row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get::<_, Option<String>>(7)?,
    ))).optional()?;
    row.map(
        |(assignment, task, issue, runner, thread, turn, legacy, tally)| {
            Ok(VerifiedRun {
                assignment,
                task,
                issue,
                runner,
                thread,
                turn,
                legacy,
                tally: tally
                    .as_deref()
                    .map(serde_json::from_str)
                    .transpose()?
                    .unwrap_or_default(),
            })
        },
    )
    .transpose()
}

fn start(db: &Connection, observation: &RunObservation) -> Result<bool> {
    let (Some(thread), Some(turn)) = (&observation.thread_id, &observation.turn_id) else {
        return Ok(false);
    };
    if thread.is_empty() || turn.is_empty() {
        return Ok(false);
    }
    let created = db.prepare_cached(
        "INSERT INTO token_runs (run_id, assignment_id, issue_id, thread_id, turn_id, legacy, tally)
         SELECT ?1, ?2, issue_id, ?3, ?4, 0, ?5 FROM tasks WHERE id = ?6
         ON CONFLICT(run_id) DO NOTHING",
    )?.execute(params![observation.run_id, observation.assignment_id, thread, turn,
        serde_json::to_string(&Tally::default())?, observation.task_id])? == 1;
    if created {
        save_run_tally(db, &observation.run_id, &Tally::default())?;
    }
    Ok(created)
}

fn stage(db: &Connection, runner: &str, usage: &RunTokenUsage) -> Result<()> {
    db.prepare_cached(
        "INSERT INTO token_usage_pending (event_id, run_id, runner_id, payload)
         SELECT ?1, ?2, ?3, ?4 FROM runner_assignments a
         WHERE a.id = ?5 AND a.runner_id = ?3 AND a.task_id = ?6 AND a.state = 'accepted'
           AND NOT EXISTS(SELECT 1 FROM task_runs WHERE id = ?2)
         ON CONFLICT(event_id) DO NOTHING",
    )?
    .execute(params![
        usage.event_id,
        usage.run_id,
        runner,
        serde_json::to_string(usage)?,
        usage.assignment_id,
        usage.task_id
    ])?;
    Ok(())
}

fn persist_request(
    db: &Connection,
    run: &VerifiedRun,
    runner: &str,
    usage: &RunTokenUsage,
    tally: &mut Tally,
) -> Result<()> {
    let (thread, turn) = usage
        .root
        .as_ref()
        .map_or((usage.thread_id.as_str(), usage.turn_id.as_str()), |root| {
            (root.thread_id.as_str(), root.turn_id.as_str())
        });
    if runner != run.runner
        || usage.assignment_id != run.assignment
        || usage.task_id != run.task
        || thread != run.thread
        || turn != run.turn
    {
        return Ok(());
    }
    let child = usage.root.is_some();
    let result = persist_verified_request(db, usage, tally);
    if result.is_ok() && child {
        system::sync_overlap(db, &usage.thread_id, &usage.turn_id)?;
    }
    result
}

fn persist_verified_request(
    db: &Connection,
    usage: &RunTokenUsage,
    tally: &mut Tally,
) -> Result<()> {
    if backfill::live(db, usage, tally)? {
        return Ok(());
    }
    let meter = RequestMeter {
        counts: usage.last,
        model: usage.model.clone(),
        provider_counts: usage.provider_counts,
    };
    let last = serde_json::to_string(&meter)?;
    let key = params![usage.thread_id, usage.turn_id, usage.response_id];
    if db.prepare_cached(
        "INSERT INTO token_requests (thread_id, turn_id, response_id, run_id, last, observed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT DO NOTHING",
    )?.execute(params![usage.thread_id, usage.turn_id, usage.response_id, usage.run_id, last, usage.observed_at])? == 1 {
        tally.count(&meter, 1);
        return Ok(());
    }
    let previous = db.prepare_cached(
        "SELECT run_id, last, conflict FROM token_requests WHERE thread_id = ?1 AND turn_id = ?2 AND response_id = ?3",
    )?.query_row(key, |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?)))?;
    // A record asserting a different Run cannot invalidate the original Run's
    // evidence. For an identity conflict within the same Run, neither counts.
    if previous.0 == usage.run_id && previous.1 != last && previous.2.is_none() {
        let old: RequestMeter = serde_json::from_str(&previous.1)?;
        if let Some(merged) = old.merge(&meter) {
            if old != merged {
                db.prepare_cached(
                    "UPDATE token_requests SET last = ?4 WHERE thread_id = ?1 AND turn_id = ?2 AND response_id = ?3",
                )?.execute(params![usage.thread_id, usage.turn_id, usage.response_id, serde_json::to_string(&merged)?])?;
                tally.count(&old, -1);
                tally.count(&merged, 1);
            }
        } else {
            db.prepare_cached(
                "UPDATE token_requests SET conflict = ?4 WHERE thread_id = ?1 AND turn_id = ?2 AND response_id = ?3",
            )?.execute(params![usage.thread_id, usage.turn_id, usage.response_id, last])?;
            tally.count(&old, -1);
        }
    }
    Ok(())
}

#[derive(Default)]
struct RunBatch<'a> {
    observation: Option<&'a RunObservation>,
    started: Option<&'a RunObservation>,
    new_start: bool,
    usage: Vec<&'a RunTokenUsage>,
}

/// Runs are lifecycle facts already accepted by sync; usage cannot start or
/// stop a Run, even when the records share a batch or arrive out of order.
pub(super) fn apply_batch(
    db: &Connection,
    runner: &str,
    observations: &[RunObservation],
    usage: &[RunTokenUsage],
) -> Result<Vec<String>> {
    let mut runs = BTreeMap::<&str, RunBatch<'_>>::new();
    for observation in observations {
        let run = runs.entry(&observation.run_id).or_default();
        run.observation = Some(observation);
        if observation.phase == RunPhase::Started {
            run.started = Some(observation);
        }
    }
    for record in usage.iter().filter(|usage| usage.valid()) {
        runs.entry(&record.run_id).or_default().usage.push(record);
    }

    for batch in runs.values_mut() {
        if let Some(observation) = batch.started {
            batch.new_start = start(db, observation)?;
            if batch.new_start
                && let (Some(thread), Some(turn)) = (&observation.thread_id, &observation.turn_id)
            {
                backfill::note_turn_ambiguity(db, thread, turn)?;
            }
        }
    }
    let mut direct: BTreeMap<String, Summary> = BTreeMap::new();
    for (id, records) in runs {
        let new_start = records.new_start;
        let Some(run) = verified_run(db, id)? else {
            if records.observation.is_some() {
                db.prepare_cached("DELETE FROM token_usage_pending WHERE run_id = ?1")?
                    .execute([id])?;
            } else {
                for record in records.usage {
                    stage(db, runner, record)?;
                }
            }
            continue;
        };
        let before = &run.tally;
        let mut after = before.clone();
        if new_start {
            // Drain across sync batches before applying current-batch records.
            let mut query = db.prepare_cached(
                "SELECT runner_id, payload FROM token_usage_pending WHERE run_id = ?1 ORDER BY rowid",
            )?;
            let mut pending = query.query([id])?;
            while let Some(row) = pending.next()? {
                let source: String = row.get(0)?;
                let payload: String = row.get(1)?;
                persist_request(
                    db,
                    &run,
                    &source,
                    &serde_json::from_str(&payload)?,
                    &mut after,
                )?;
            }
            db.prepare_cached("DELETE FROM token_usage_pending WHERE run_id = ?1")?
                .execute([id])?;
        }
        let had_usage = !records.usage.is_empty();
        for record in records.usage {
            persist_request(db, &run, runner, record, &mut after)?;
        }
        if had_usage || new_start {
            system::sync_overlap(db, &run.thread, &run.turn)?;
        }
        if &after != before {
            save_run_tally(db, id, &after)?;
        }
        if new_start || &after != before {
            direct
                .entry(run.issue.clone())
                .or_default()
                .merge(&run_delta(&run, after, new_start));
        }
    }
    update_rollups(db, direct)?;
    // Confirm receipt independently from attribution. Malformed, conflicted
    // and misattributed observations must not jam the Runner outbox.
    Ok(usage.iter().map(|usage| usage.event_id.clone()).collect())
}

fn save_run_tally(db: &Connection, id: &str, tally: &Tally) -> Result<()> {
    db.prepare_cached(
        "INSERT INTO token_run_tallies (run_id, tally, valid_records) VALUES (?1, ?2, ?3)
         ON CONFLICT(run_id) DO UPDATE SET tally = excluded.tally, valid_records = excluded.valid_records"
    )?
        .execute(params![
            id,
            serde_json::to_string(&tally)?,
            tally.tokens.valid_records
        ])?;
    Ok(())
}

fn save_delta(db: &Connection, id: &str, run: &VerifiedRun, after: Tally) -> Result<()> {
    system::sync_overlap(db, &run.thread, &run.turn)?;
    if after != run.tally {
        save_run_tally(db, id, &after)?;
        update_rollups(
            db,
            BTreeMap::from([(run.issue.clone(), run_delta(run, after, false))]),
        )?;
    }
    Ok(())
}

fn run_delta(run: &VerifiedRun, after: Tally, new_start: bool) -> Summary {
    let was_observed = i64::from(run.tally.tokens.valid_records > 0);
    let observed = i64::from(after.tokens.valid_records > 0);
    let mut delta = Summary {
        started_runs: i64::from(new_start),
        observed_runs: observed - was_observed,
        legacy_unobserved: if run.legacy {
            was_observed - observed
        } else {
            0
        },
        tally: after,
    };
    delta.tally.merge(&run.tally, -1);
    delta
}

fn update_rollups(db: &Connection, direct: BTreeMap<String, Summary>) -> Result<()> {
    let mut subtree: BTreeMap<String, Summary> = BTreeMap::new();
    let mut global = Summary::default();
    for (issue, delta) in direct {
        if delta == Summary::default() {
            continue;
        }
        update_issue(db, &issue, "direct", &delta)?;
        global.merge(&delta);
        let mut query =
            db.prepare_cached("SELECT ancestor FROM token_issue_tree WHERE descendant = ?1")?;
        for ancestor in query.query_map([&issue], |row| row.get::<_, String>(0))? {
            subtree.entry(ancestor?).or_default().merge(&delta);
        }
    }
    for (issue, delta) in subtree {
        if delta != Summary::default() {
            update_issue(db, &issue, "subtree", &delta)?;
        }
    }
    if global != Summary::default() {
        let mut total = overview::read_global(db)?;
        total.merge(&global);
        overview::write_global(db, &total)?;
    }
    Ok(())
}

fn update_issue(db: &Connection, issue: &str, scope: &str, delta: &Summary) -> Result<()> {
    let existing = db
        .prepare_cached(
            "SELECT summary FROM token_issue_rollups WHERE issue_id = ?1 AND scope = ?2",
        )?
        .query_row(params![issue, scope], |row| row.get::<_, String>(0))
        .optional()?;
    let mut summary = existing
        .as_deref()
        .map(serde_json::from_str)
        .transpose()?
        .unwrap_or_default();
    Summary::merge(&mut summary, delta);
    write_issue(db, issue, scope, summary)
}

fn write_issue(db: &Connection, issue: &str, scope: &str, summary: Summary) -> Result<()> {
    db.prepare_cached(
        "INSERT INTO token_issue_rollups (issue_id, scope, summary) VALUES (?1, ?2, ?3)
         ON CONFLICT(issue_id, scope) DO UPDATE SET summary = excluded.summary",
    )?
    .execute(params![issue, scope, serde_json::to_string(&summary)?])?;
    Ok(())
}

const ISSUE_USAGE_QUERY: &str = "SELECT e.descendant, n.number, i.title, d.summary, s.summary
    FROM token_issue_tree e
    JOIN issues i ON i.id = e.descendant
    JOIN issue_short_numbers n ON n.issue_id = e.descendant
    LEFT JOIN token_issue_rollups d ON d.issue_id = e.descendant AND d.scope = 'direct'
    LEFT JOIN token_issue_rollups s ON s.issue_id = e.ancestor AND s.scope = 'subtree' AND e.ancestor = e.descendant
    WHERE e.ancestor = ?1 ORDER BY n.number";

#[derive(Serialize)]
pub struct IssueTokenUsage {
    direct: Breakdown,
    subtree: Breakdown,
    nodes: Vec<Node>,
}

#[derive(Serialize)]
struct Node {
    issue_id: String,
    number: i64,
    title: String,
    direct: Breakdown,
}

#[derive(Clone, Serialize)]
struct Breakdown {
    models: Vec<models::ModelRow>,
    estimate: models::Estimate,
    started_runs: i64,
    observed_runs: i64,
    unobserved_before_collection: i64,
    unobserved_no_usage: i64,
    reported_requests: i64,
    total_tokens: Field,
    input_tokens: Field,
    cached_input_tokens: Field,
    cache_write_input_tokens: Field,
    output_tokens: Field,
    reasoning_output_tokens: Field,
}

#[derive(Clone, Serialize)]
struct Field {
    tokens: Option<String>,
    reported_requests: i64,
}

impl From<Summary> for Breakdown {
    fn from(summary: Summary) -> Self {
        let [
            total_tokens,
            input_tokens,
            cached_input_tokens,
            cache_write_input_tokens,
            output_tokens,
            reasoning_output_tokens,
        ] = summary.tally.tokens.fields();
        let (models, estimate) = models::rows(summary.tally.model_usage, summary.tally.tokens);
        Self {
            started_runs: summary.started_runs,
            observed_runs: summary.observed_runs,
            unobserved_before_collection: summary.legacy_unobserved,
            unobserved_no_usage: summary.started_runs
                - summary.observed_runs
                - summary.legacy_unobserved,
            reported_requests: summary.tally.tokens.valid_records,
            models,
            estimate,
            total_tokens,
            input_tokens,
            cached_input_tokens,
            cache_write_input_tokens,
            output_tokens,
            reasoning_output_tokens,
        }
    }
}

fn breakdown(value: Option<String>) -> Result<Breakdown> {
    Ok(value
        .as_deref()
        .map(serde_json::from_str::<Summary>)
        .transpose()?
        .unwrap_or_default()
        .into())
}

impl Store {
    pub fn issue_token_usage(&self, issue: &str) -> Result<Option<IssueTokenUsage>> {
        let mut query = self.db.prepare_cached(ISSUE_USAGE_QUERY)?;
        let mut rows = query.query([issue])?;
        let mut summary = None;
        let mut nodes = Vec::new();
        while let Some(row) = rows.next()? {
            let issue_id: String = row.get(0)?;
            let direct = breakdown(row.get(3)?)?;
            if issue_id == issue {
                summary = Some((direct.clone(), breakdown(row.get(4)?)?));
            }
            nodes.push(Node {
                issue_id,
                number: row.get(1)?,
                title: row.get(2)?,
                direct,
            });
        }
        Ok(summary.map(|(direct, subtree)| IssueTokenUsage {
            direct,
            subtree,
            nodes,
        }))
    }
}

#[cfg(test)]
mod tests;
