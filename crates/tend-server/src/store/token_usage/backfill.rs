use super::{Tally, VerifiedRun, verified_run};
use crate::store::{Store, runner::FRESH_SECONDS, timing};
use anyhow::Result;
use protocol::{
    NativeTokenRecord, RequestMeter, StartTokenBackfill, TokenBackfillContent, TokenBackfillPage,
    TokenBackfillWork, TokenConflictPolicy, TokenCounts, TokenModel, TokenUsageScope,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const PAGE: i64 = 100;

pub(in crate::store) fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(include_str!("backfill.sql"))?;
    Ok(())
}

pub(in crate::store) fn migrate_ambiguities(db: &Connection) -> Result<()> {
    let mut sql = db.prepare(
        "SELECT thread_id, turn_id FROM token_runs GROUP BY thread_id, turn_id HAVING count(*) > 1",
    )?;
    let ambiguous = sql
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (thread, turn) in ambiguous {
        note_turn_ambiguity(db, &thread, &turn)?;
    }
    Ok(())
}

type Key<'a> = (&'a str, &'a str, &'a str);

fn request(
    db: &Connection,
    key: Key<'_>,
) -> Result<Option<(String, RequestMeter, Option<String>)>> {
    db.prepare_cached(
        "SELECT run_id, last, conflict FROM token_requests
         WHERE thread_id = ?1 AND turn_id = ?2 AND response_id = ?3",
    )?
    .query_row(key, |row| {
        Ok((row.get(0)?, row.get::<_, String>(1)?, row.get(2)?))
    })
    .optional()?
    .map(|(run, last, conflict)| Ok((run, serde_json::from_str(&last)?, conflict)))
    .transpose()
}

#[derive(Deserialize, Serialize)]
struct Evidence {
    native: NativeTokenRecord,
    prior_native: Option<NativeTokenRecord>,
    server: Option<TokenCounts>,
    server_conflict: Option<TokenCounts>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    server_model: Option<TokenModel>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    server_provider_counts: bool,
    policy: TokenConflictPolicy,
}

fn compatible(server: TokenCounts, native: TokenCounts) -> bool {
    server
        .fields()
        .into_iter()
        .zip(native.fields())
        .enumerate()
        .all(|(i, (server, native))| {
            server.is_none()
                || server == native
                || (matches!(i, 2 | 3 | 5) && server == Some(0) && native.is_none())
        })
}

fn same_usage(a: &NativeTokenRecord, b: &NativeTokenRecord) -> bool {
    let normalized = |record: &NativeTokenRecord| {
        let counts = record
            .counts
            .map(TokenCounts::without_unproven_optional_zeros);
        let variant = record
            .variant
            .map(TokenCounts::without_unproven_optional_zeros)
            .filter(|variant| Some(*variant) != counts);
        (counts, record.invalid, variant)
    };
    normalized(a) == normalized(b)
}

impl Evidence {
    fn verified_server(&self) -> Option<TokenCounts> {
        self.server
            .filter(|counts| (counts.valid() || counts.missing()) && self.server_conflict.is_none())
    }

    fn decision(&self) -> (&'static str, Option<TokenCounts>) {
        use TokenConflictPolicy::*;
        let native = self.native.counts.filter(|counts| {
            (counts.valid() || counts.missing())
                && !self.native.invalid
                && self.native.variant.is_none()
        });
        let server = self.verified_server();
        match (native, self.server, server, self.policy) {
            (Some(native), None, _, _) if self.prior_native.is_none() => {
                ("native_only", Some(native))
            }
            (Some(native), _, Some(server), _)
                if self.prior_native.is_none()
                    && self.server_provider_counts
                    && server.merge_known(native).is_some() =>
            {
                ("compatible", Some(server))
            }
            (Some(native), _, Some(server), _)
                if self.prior_native.is_none() && compatible(server, native) =>
            {
                (
                    if server.without_unproven_optional_zeros() == native {
                        "compatible"
                    } else {
                        "enriched"
                    },
                    Some(native),
                )
            }
            (Some(native), _, _, PreferNative) => ("chose_native", Some(native)),
            (_, _, Some(server), PreferServer) => ("chose_server", Some(server)),
            _ => ("quarantined", None),
        }
    }

    fn meter(&self, decision: &str, counts: TokenCounts) -> RequestMeter {
        let model = if decision == "chose_server" {
            self.server_model.clone()
        } else {
            let live = self
                .verified_server()
                .filter(|server| compatible(*server, counts))
                .and(self.server_model.clone());
            TokenModel::merge(self.native.model.clone(), live)
        };
        RequestMeter {
            counts,
            model,
            provider_counts: decision != "chose_native"
                && self.server_provider_counts
                && self.server == Some(counts),
        }
    }

    fn audit(&self, decision: &str) -> Value {
        json!({"server":self.server,"server_conflict":self.server_conflict,
            "native":self.native.counts,"native_variant":self.native.variant,
            "native_invalid":self.native.invalid,"prior_native":self.prior_native,
            "native_model":self.native.model,"server_model":self.server_model,
            "server_provider_counts":self.server_provider_counts,
            "policy":self.policy,"decision":decision})
    }
}

fn binding(db: &Connection, key: Key<'_>) -> Result<Option<(String, String, Evidence)>> {
    db.prepare_cached(
        "SELECT run_id, job_id, evidence FROM token_backfill_keys
         WHERE thread_id = ?1 AND turn_id = ?2 AND response_id = ?3",
    )?
    .query_row(key, |row| {
        Ok((row.get(0)?, row.get(1)?, row.get::<_, String>(2)?))
    })
    .optional()?
    .map(|(run, job, text)| Ok((run, job, serde_json::from_str(&text)?)))
    .transpose()
}

fn save_binding(
    db: &Connection,
    key: Key<'_>,
    run: &str,
    job: &str,
    evidence: &Evidence,
) -> Result<()> {
    db.prepare_cached(
        "INSERT INTO token_backfill_keys (thread_id, turn_id, response_id, run_id, job_id, evidence)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(thread_id, turn_id, response_id) DO UPDATE SET job_id = excluded.job_id, evidence = excluded.evidence",
    )?.execute(params![key.0, key.1, key.2, run, job, serde_json::to_string(evidence)?])?;
    Ok(())
}

fn audit(
    db: &Connection,
    job: &str,
    run: &str,
    key: Key<'_>,
    root: (&str, &str),
    source: &str,
    evidence: Value,
) -> Result<()> {
    db.prepare_cached(
        "INSERT INTO token_backfill_audit (job_id, run_id, response_id, source, evidence)
         VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT DO NOTHING",
    )?
    .execute(params![
        job,
        run,
        protocol::native_request_cursor(((key.0, key.1) != root).then_some((key.0, key.1)), key.2),
        source,
        serde_json::to_string(&evidence)?
    ])?;
    Ok(())
}

/// For managed keys both source variants are retained, so choosing authority
/// gives the same result whether a live observation arrives first or last.
fn project(
    db: &Connection,
    key: Key<'_>,
    run: &str,
    previous: Option<(String, RequestMeter, Option<String>)>,
    chosen: Option<RequestMeter>,
    evidence: &Evidence,
    tally: &mut Tally,
) -> Result<()> {
    let before = previous
        .as_ref()
        .and_then(|(_, last, conflict)| conflict.is_none().then_some(last));
    if before == chosen.as_ref() {
        return Ok(());
    }
    if let Some(before) = before {
        tally.count(before, -1);
    }
    if let Some(chosen) = chosen {
        tally.count(&chosen, 1);
        db.prepare_cached(
            "INSERT INTO token_requests (thread_id, turn_id, response_id, run_id, last, observed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, unixepoch())
             ON CONFLICT(thread_id, turn_id, response_id) DO UPDATE SET last = excluded.last, conflict = NULL",
        )?.execute(params![key.0, key.1, key.2, run, serde_json::to_string(&chosen)?])?;
    } else if let Some((_, last, _)) = previous {
        db.prepare_cached(
            "UPDATE token_requests SET conflict = ?4
             WHERE thread_id = ?1 AND turn_id = ?2 AND response_id = ?3",
        )?
        .execute(params![
            key.0,
            key.1,
            key.2,
            serde_json::to_string(&evidence.native.counts.unwrap_or(last.counts))?
        ])?;
    }
    Ok(())
}

/// Called only after ordinary sync verified the original assignment and turn.
/// A successor never submits observations on the original Runner's behalf.
pub(super) fn live(
    db: &Connection,
    usage: &protocol::RunTokenUsage,
    tally: &mut Tally,
) -> Result<bool> {
    let key = (&*usage.thread_id, &*usage.turn_id, &*usage.response_id);
    let Some((run, job, mut evidence)) = binding(db, key)? else {
        return Ok(false);
    };
    // Revoked historical bindings are evidence, not realtime ownership.
    // The canonical request was restored (or removed) when ambiguity was
    // recorded, so just use the ordinary first verified realtime copy.
    let root = usage.root.as_ref().map_or((key.0, key.1), |root| {
        (root.thread_id.as_str(), root.turn_id.as_str())
    });
    if ambiguous(db, root.0, root.1)? {
        return Ok(false);
    }
    if run != usage.run_id {
        audit(
            db,
            &job,
            &usage.run_id,
            key,
            root,
            "live",
            json!({
                "decision":"wrong_run","owner_run_id":run,"server":usage.last
            }),
        )?;
        return Ok(true);
    }
    let previous = (
        evidence.server,
        evidence.server_conflict,
        evidence.server_model.clone(),
        evidence.server_provider_counts,
    );
    match evidence.server {
        None => {
            evidence.server = Some(usage.last);
            evidence.server_model = usage.model.clone();
            evidence.server_provider_counts = usage.provider_counts;
        }
        Some(first) => {
            if evidence.server_conflict.is_none()
                && let Some(merged) = first.merge_reported(
                    evidence.server_provider_counts,
                    usage.last,
                    usage.provider_counts,
                )
            {
                evidence.server = Some(merged);
                evidence.server_provider_counts |= usage.provider_counts;
                evidence.server_model =
                    TokenModel::merge(evidence.server_model, usage.model.clone());
            } else {
                evidence.server_conflict.get_or_insert(usage.last);
            }
        }
    }
    if previous
        == (
            evidence.server,
            evidence.server_conflict,
            evidence.server_model.clone(),
            evidence.server_provider_counts,
        )
    {
        return Ok(true);
    }
    let (decision, chosen) = evidence.decision();
    if decision != "compatible" {
        let mut detail = evidence.audit(decision);
        detail["incoming"] = json!(usage.last);
        audit(db, &job, &run, key, root, "live", detail)?;
    }
    let chosen = chosen.map(|counts| evidence.meter(decision, counts));
    project(db, key, &run, request(db, key)?, chosen, &evidence, tally)?;
    save_binding(db, key, &run, &job, &evidence)?;
    Ok(true)
}

fn historical(
    db: &Connection,
    run: &VerifiedRun,
    run_id: &str,
    job: &StartTokenBackfill,
    reader: &str,
    native: &NativeTokenRecord,
    tally: &mut Tally,
) -> Result<&'static str> {
    // Old Runner readers sent Codex's optional defaults directly. Keep the
    // compatibility decision independent of which generation read the file.
    let mut normalized = native.clone();
    normalized.counts = native
        .counts
        .map(TokenCounts::without_unproven_optional_zeros);
    normalized.variant = native
        .variant
        .map(TokenCounts::without_unproven_optional_zeros)
        .filter(|variant| Some(*variant) != normalized.counts);
    if normalized.counts.is_some_and(TokenCounts::missing) && normalized.model.is_none() {
        normalized.model = Some(TokenModel::Unresolved);
    }
    let native = &normalized;
    let key = native.child.as_ref().map_or(
        (
            run.thread.as_str(),
            run.turn.as_str(),
            native.response_id.as_str(),
        ),
        |child| {
            (
                child.thread_id.as_str(),
                child.turn_id.as_str(),
                native.response_id.as_str(),
            )
        },
    );
    if native
        .child
        .as_ref()
        .is_some_and(|child| child.thread_id == run.thread || child.turn_id == run.turn)
    {
        return Ok("invalid_child_identity");
    }
    let root = (run.thread.as_str(), run.turn.as_str());
    let original = request(db, key)?;
    let binding = binding(db, key)?;
    if let Some(owner) = binding
        .as_ref()
        .map(|(id, _, _)| id)
        .or(original.as_ref().map(|row| &row.0))
        && owner != run_id
    {
        audit(
            db,
            &job.job_id,
            run_id,
            key,
            root,
            "rollout",
            json!({
                "decision":"wrong_run","owner_run_id":owner,"reader_id":reader,"native":native
            }),
        )?;
        return Ok("wrong_run");
    }
    let old = binding.as_ref().map(|(_, _, old)| old);
    let mut evidence = match old {
        Some(old) => Evidence {
            native: native.clone(),
            prior_native: if same_usage(&old.native, native) {
                old.prior_native.clone()
            } else {
                Some(old.native.clone())
            },
            server: old.server,
            server_conflict: old.server_conflict,
            server_model: old.server_model.clone(),
            server_provider_counts: old.server_provider_counts,
            policy: if job.policy == TokenConflictPolicy::Quarantine
                && same_usage(&old.native, native)
            {
                old.policy
            } else {
                job.policy
            },
        },
        None => Evidence {
            native: native.clone(),
            prior_native: None,
            server: original.as_ref().map(|row| row.1.counts),
            server_model: original.as_ref().and_then(|row| row.1.model.clone()),
            server_provider_counts: original.as_ref().is_some_and(|row| row.1.provider_counts),
            server_conflict: original
                .as_ref()
                .and_then(|row| row.2.as_ref())
                .map(|raw| serde_json::from_str(raw))
                .transpose()?,
            policy: job.policy,
        },
    };
    if let Some(old) = old
        && same_usage(&old.native, native)
    {
        evidence.native.model = TokenModel::merge(old.native.model.clone(), native.model.clone());
    }
    let (decision, chosen) = evidence.decision();
    if decision != "compatible" && decision != "native_only"
        || old.is_some_and(|old| old.native != *native)
    {
        let mut detail = evidence.audit(decision);
        detail["reader_id"] = json!(reader);
        detail["previous_selected"] = json!(original.as_ref().map(|row| &row.1));
        detail["previous_native"] = json!(old.map(|old| &old.native));
        audit(db, &job.job_id, run_id, key, root, "rollout", detail)?;
    }
    let chosen = chosen.map(|counts| evidence.meter(decision, counts));
    project(db, key, run_id, original, chosen, &evidence, tally)?;
    save_binding(db, key, run_id, &job.job_id, &evidence)?;
    if native.child.is_some() {
        super::system::sync_overlap(db, key.0, key.1)?;
    }
    Ok(decision)
}

fn ambiguous(db: &Connection, thread: &str, turn: &str) -> Result<bool> {
    Ok(db.prepare_cached(
        "SELECT EXISTS(SELECT 1 FROM token_backfill_ambiguous WHERE thread_id = ?1 AND turn_id = ?2)"
    )?.query_row(params![thread, turn], |row| row.get(0))?)
}

fn restore_server_copy(
    db: &Connection,
    key: Key<'_>,
    run: &str,
    original: Option<(String, RequestMeter, Option<String>)>,
    evidence: &Evidence,
    tally: &mut Tally,
) -> Result<()> {
    if let Some((owner, last, conflict)) = original {
        if owner != run {
            return Ok(());
        }
        if conflict.is_none() {
            tally.count(&last, -1);
        }
    }
    if let Some(counts) = evidence.server {
        let server = RequestMeter {
            counts,
            model: evidence.server_model.clone(),
            provider_counts: evidence.server_provider_counts,
        };
        // Keep even a conflicted realtime owner and its exact old evidence.
        // Do not leave a rollout-origin conflict marker on an ordinary copy.
        let conflict = evidence
            .server_conflict
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        db.prepare_cached(
            "INSERT INTO token_requests (thread_id, turn_id, response_id, run_id, last, conflict, observed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, unixepoch())
             ON CONFLICT(thread_id, turn_id, response_id) DO UPDATE SET last = excluded.last, conflict = excluded.conflict",
        )?.execute(params![key.0, key.1, key.2, run, serde_json::to_string(&server)?, conflict])?;
        if evidence.verified_server().is_some() {
            tally.count(&server, 1);
        }
    } else {
        // The job audit and original binding retain the retracted evidence.
        // A rollout-only row must not reserve the native key against a
        // verified live receipt, even if the row had already been isolated.
        db.prepare_cached(
            "DELETE FROM token_requests
             WHERE thread_id = ?1 AND turn_id = ?2 AND response_id = ?3 AND run_id = ?4",
        )?
        .execute(params![key.0, key.1, key.2, run])?;
    }
    Ok(())
}

/// The accepted Start can race a page or arrive after a completed job.
/// Retract only rollout-derived contributions and preserve ordinary copies.
pub(super) fn note_turn_ambiguity(db: &Connection, thread: &str, turn: &str) -> Result<bool> {
    if !ambiguous(db, thread, turn)? {
        let count = db.prepare_cached(
            "SELECT count(*) FROM (SELECT run_id FROM token_runs WHERE thread_id = ?1 AND turn_id = ?2 LIMIT 2)"
        )?.query_row(params![thread, turn], |row| row.get::<_, i64>(0))?;
        if count < 2 {
            return Ok(false);
        }
        db.execute(
            "INSERT INTO token_backfill_ambiguous VALUES (?1, ?2)",
            params![thread, turn],
        )?;
        let mut keys = db.prepare_cached(
            "SELECT k.run_id, k.job_id, k.response_id, k.evidence, k.thread_id, k.turn_id
             FROM token_runs r JOIN token_backfill_keys k ON k.run_id=r.run_id
             WHERE r.thread_id = ?1 AND r.turn_id = ?2",
        )?;
        let grouped = keys
            .query_map(params![thread, turn], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut affected = std::collections::BTreeMap::new();
        for (run_id, job, response, text, native_thread, native_turn) in grouped {
            let (_, tally) = match affected.entry(run_id.clone()) {
                std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::btree_map::Entry::Vacant(entry) => {
                    let run = verified_run(db, &run_id)?.expect("backfill key references a Run");
                    let tally = run.tally.clone();
                    entry.insert((run, tally))
                }
            };
            let evidence: Evidence = serde_json::from_str(&text)?;
            let key = (
                native_thread.as_str(),
                native_turn.as_str(),
                response.as_str(),
            );
            let original = request(db, key)?;
            let mut detail = evidence.audit("ambiguous_turn");
            detail["previous_selected"] = json!(original.as_ref().map(|row| &row.1));
            audit(db, &job, &run_id, key, (thread, turn), "assignment", detail)?;
            restore_server_copy(db, key, &run_id, original, &evidence, tally)?;
            if db
                .prepare_cached(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='token_agent_requests')",
                )?
                .query_row([], |row| row.get::<_, bool>(0))?
            {
                super::system::sync_overlap(db, key.0, key.1)?;
            }
        }
        for (id, (run, tally)) in affected {
            super::save_delta(db, &id, &run, tally)?;
        }
    }
    db.prepare_cached(
        "UPDATE token_backfill_items SET state = 'gap', gap = 'ambiguous_turn'
         WHERE run_id IN (SELECT run_id FROM token_runs WHERE thread_id = ?1 AND turn_id = ?2)
           AND gap IS NOT 'ambiguous_turn'",
    )?
    .execute(params![thread, turn])?;
    Ok(true)
}

pub(in crate::store) fn pending(
    db: &Connection,
    reader: &str,
) -> Result<Option<TokenBackfillWork>> {
    Ok(db
        .prepare_cached(
            "SELECT i.id, i.job_id, a.runner_id, r.run_id, r.thread_id, r.turn_id, i.offset
         FROM token_backfill_items i JOIN token_runs r ON r.run_id = i.run_id
         JOIN runner_assignments a ON a.id = r.assignment_id
         WHERE i.reader_id = ?1 AND i.state = 'pending' ORDER BY i.id LIMIT 1",
        )?
        .query_row([reader], |row| {
            Ok(TokenBackfillWork {
                item: row.get(0)?,
                job_id: row.get(1)?,
                runner_id: row.get(2)?,
                run_id: row.get(3)?,
                thread_id: row.get(4)?,
                turn_id: row.get(5)?,
                offset: row.get(6)?,
                include_children: true,
            })
        })
        .optional()?)
}

pub(in crate::store) fn receive(
    db: &Connection,
    reader: &str,
    page: &TokenBackfillPage,
) -> Result<()> {
    // The current pending item is the bound instruction. Delayed/duplicate
    // pages, an old reader after retirement and out-of-order pages are no-ops.
    let Some(work) =
        pending(db, reader)?.filter(|work| work.item == page.item && work.offset == page.offset)
    else {
        return Ok(());
    };
    let Some(run) = verified_run(db, &work.run_id)? else {
        return Ok(());
    };
    if run.runner != work.runner_id || run.thread != work.thread_id || run.turn != work.turn_id {
        return Ok(());
    }
    if note_turn_ambiguity(db, &run.thread, &run.turn)? {
        return Ok(());
    }
    if let TokenBackfillContent::Gap { reason } = page.content {
        db.execute(
            "UPDATE token_backfill_items SET state = 'gap', gap = ?2 WHERE id = ?1",
            params![work.item, serde_json::to_value(reason)?.as_str()],
        )?;
        return Ok(());
    }
    let TokenBackfillContent::Records {
        snapshot,
        records,
        done,
        prefix,
        child_gap,
        child_checked,
    } = &page.content
    else {
        unreachable!();
    };
    let (previous, last, raw) = db.query_row(
        "SELECT i.snapshot, i.last_response, j.request FROM token_backfill_items i
         JOIN token_backfill_jobs j ON j.id = i.job_id WHERE i.id = ?1",
        [work.item],
        |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
            ))
        },
    )?;
    if previous.as_ref().is_some_and(|old| old != snapshot)
        || last
            .as_ref()
            .is_some_and(|old| records.first().is_some_and(|first| *old >= first.cursor()))
    {
        db.execute(
            "UPDATE token_backfill_items SET state = 'gap', gap = 'source_changed' WHERE id = ?1",
            [work.item],
        )?;
        return Ok(());
    }
    let job: StartTokenBackfill = serde_json::from_str(&raw)?;
    let mut tally = run.tally.clone();
    for native in records {
        let decision = historical(db, &run, &work.run_id, &job, reader, native, &mut tally)?;
        db.prepare_cached(
            "INSERT INTO token_backfill_seen (item, response_id, result) VALUES (?1, ?2, ?3)",
        )?
        .execute(params![work.item, native.cursor(), decision])?;
    }
    super::save_delta(db, &work.run_id, &run, tally)?;
    if *done {
        // An unmatched copied fact is reported but never subtracted. Even a
        // sealed scan does not certify absence (e.g. pre-record Codex versions).
        db.execute(
            "INSERT INTO token_backfill_audit (job_id, run_id, response_id, source, evidence)
             SELECT ?2, q.run_id, CASE WHEN q.thread_id=?4 AND q.turn_id=?5 THEN q.response_id
                    ELSE char(31)||q.thread_id||char(31)||q.turn_id||char(31)||q.response_id END, 'server',
                    json_object('decision', 'not_seen_in_rollout', 'server', json(q.last), 'server_conflict', json(q.conflict))
             FROM token_requests q WHERE q.run_id = ?3
             AND (?6 OR (q.thread_id=?4 AND q.turn_id=?5))
             AND NOT EXISTS (SELECT 1 FROM token_backfill_seen s WHERE s.item = ?1
                 AND s.response_id=CASE WHEN q.thread_id=?4 AND q.turn_id=?5 THEN q.response_id
                    ELSE char(31)||q.thread_id||char(31)||q.turn_id||char(31)||q.response_id END)
             ON CONFLICT DO NOTHING",
            params![work.item, work.job_id, work.run_id, run.thread, run.turn, *child_checked && child_gap.is_none()],
        )?;
    }
    let child_gap = child_gap.map(serde_json::to_value).transpose()?;
    let gap = if !child_checked {
        Some("child_reader_unsupported")
    } else {
        child_gap.as_ref().and_then(Value::as_str)
    };
    db.execute(
        "UPDATE token_backfill_items SET snapshot = ?2, offset = offset + ?3, last_response = ?4,
         state = CASE WHEN NOT ?5 THEN 'pending' WHEN ?6 THEN 'active_prefix'
                      WHEN ?7 IS NOT NULL THEN 'gap' ELSE 'scanned' END, gap=?7 WHERE id = ?1",
        params![
            work.item,
            snapshot,
            records.len() as i64,
            records.last().map(NativeTokenRecord::cursor),
            done,
            prefix,
            gap
        ],
    )?;
    Ok(())
}

/// Returns predicates made only of static SQL. The one selector argument is
/// separately bound. The Run membership is captured in the job transaction.
pub(super) fn scope_filter(scope: &TokenUsageScope) -> (&'static str, Option<&str>) {
    match scope {
        TokenUsageScope::All => ("?1 IS NULL", None),
        TokenUsageScope::Run(id) => ("r.run_id = ?1", Some(id)),
        TokenUsageScope::Task(id) => ("a.task_id = ?1", Some(id)),
        TokenUsageScope::Issue(id) => (
            "r.issue_id IN (SELECT descendant FROM token_issue_tree WHERE ancestor =
                coalesce((SELECT issue_id FROM issue_short_numbers WHERE number = ?1), ?1))",
            Some(id.strip_prefix('#').unwrap_or(id)),
        ),
        TokenUsageScope::Runner(id) => ("a.runner_id = ?1", Some(id)),
        TokenUsageScope::Host(id) => ("o.host_id = ?1", Some(id)),
    }
}

pub(super) const SCOPE_JOINS: &str =
    "FROM token_runs r JOIN runner_assignments a ON a.id = r.assignment_id
    JOIN runners o ON o.id = a.runner_id";

impl Store {
    pub fn start_token_backfill(&mut self, job: &StartTokenBackfill) -> Result<Option<Value>> {
        let (tx, timing) = timing::begin(&mut self.db, "token_backfill_start")?;
        let raw = serde_json::to_string(job)?;
        let created = tx.execute(
            "INSERT INTO token_backfill_jobs (id, request) VALUES (?1, ?2) ON CONFLICT DO NOTHING",
            params![job.job_id, raw],
        )? == 1;
        if created {
            if job.scope == TokenUsageScope::All {
                super::system::enqueue(&tx, &job.job_id)?;
            }
            let (filter, argument) = scope_filter(&job.scope);
            tx.execute(&format!(
                "WITH RECURSIVE readers(origin, reader, host) AS (
                    SELECT id, id, host_id FROM runners UNION
                    SELECT d.origin, s.id, d.host FROM readers d JOIN runner_replacements p
                      ON p.predecessor = d.reader AND p.retired_at IS NOT NULL
                    JOIN runners s ON s.id = p.successor AND s.host_id = d.host
                 ), current_reader AS (
                    SELECT origin, reader FROM readers
                    WHERE NOT EXISTS(SELECT 1 FROM runner_replacements
                        WHERE predecessor = reader AND retired_at IS NOT NULL)
                 )
                 INSERT INTO token_backfill_items (job_id, run_id, reader_id, state, gap)
                 SELECT ?2, r.run_id, rd.reader,
                    CASE WHEN am.thread_id IS NOT NULL OR rd.reader IS NULL THEN 'gap' ELSE 'pending' END,
                    CASE WHEN am.thread_id IS NOT NULL THEN 'ambiguous_turn'
                         WHEN rd.reader IS NULL THEN 'reader_missing' END
                 {SCOPE_JOINS} LEFT JOIN current_reader rd ON rd.origin = a.runner_id
                 LEFT JOIN token_backfill_ambiguous am ON am.thread_id = r.thread_id AND am.turn_id = r.turn_id
                 WHERE {filter}"
            ), params![argument, job.job_id])?;
        } else if !tx.query_row(
            "SELECT request = ?2 FROM token_backfill_jobs WHERE id = ?1",
            params![job.job_id, raw],
            |row| row.get::<_, bool>(0),
        )? {
            return Ok(None);
        }
        timing::commit(tx, timing)?;
        self.token_backfill_status(&job.job_id, 0)
    }

    pub fn token_backfill_status(&self, job: &str, after: i64) -> Result<Option<Value>> {
        let tx = self.db.unchecked_transaction()?;
        let Some((request, created_at)) = tx
            .query_row(
                "SELECT request, created_at FROM token_backfill_jobs WHERE id = ?1",
                [job],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?
        else {
            return Ok(None);
        };
        let mut counts = serde_json::Map::new();
        let mut count = tx.prepare(
            "SELECT state, count(*) FROM (
               SELECT CASE WHEN i.state != 'pending' THEN i.state
                 WHEN r.last_seen <= unixepoch() - ?2 THEN 'reader_offline'
                 WHEN b.runner_id IS NULL THEN 'reader_unsupported'
                 ELSE 'pending' END AS state
               FROM token_backfill_items i LEFT JOIN runners r ON r.id = i.reader_id
               LEFT JOIN token_backfill_readers b ON b.runner_id = i.reader_id WHERE i.job_id = ?1
             ) GROUP BY state",
        )?;
        for data in count.query_map(params![job, FRESH_SECONDS], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })? {
            let (state, n) = data?;
            counts.insert(state, json!(n));
        }
        let mut items = tx.prepare(
            "SELECT i.id, i.run_id, r.issue_id, a.task_id, a.runner_id, o.host_id, i.reader_id,
                CASE WHEN i.state != 'pending' THEN i.state
                     WHEN rr.last_seen <= unixepoch() - ?3 THEN 'reader_offline'
                     WHEN b.runner_id IS NULL THEN 'reader_unsupported'
                     ELSE 'pending' END,
                i.gap, i.offset,
                (SELECT count(*) FROM token_backfill_audit WHERE job_id = i.job_id AND run_id = i.run_id AND source = 'server')
             FROM token_backfill_items i JOIN token_runs r ON r.run_id = i.run_id
             JOIN runner_assignments a ON a.id = r.assignment_id JOIN runners o ON o.id = a.runner_id
             LEFT JOIN runners rr ON rr.id = i.reader_id LEFT JOIN token_backfill_readers b ON b.runner_id = i.reader_id
             WHERE i.job_id = ?1 AND i.id > ?2 ORDER BY i.id LIMIT ?4",
        )?;
        let mut seen = tx.prepare(
            "SELECT result, count(*) FROM token_backfill_seen WHERE item = ?1 GROUP BY result",
        )?;
        let mut rows = items.query(params![job, after, FRESH_SECONDS, PAGE + 1])?;
        let mut runs = Vec::new();
        let mut next_after = None;
        while let Some(row) = rows.next()? {
            if runs.len() == PAGE as usize {
                next_after = runs
                    .last()
                    .and_then(|value: &Value| value["cursor"].as_i64());
                break;
            }
            let id: i64 = row.get(0)?;
            let mut results = serde_json::Map::new();
            for data in seen.query_map([id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })? {
                let (result, n) = data?;
                results.insert(result, json!(n));
            }
            runs.push(json!({
                "cursor": id, "run_id": row.get::<_, String>(1)?, "issue_id": row.get::<_, String>(2)?,
                "task_id": row.get::<_, String>(3)?, "runner_id": row.get::<_, String>(4)?,
                "host_id": row.get::<_, String>(5)?, "reader_id": row.get::<_, Option<String>>(6)?,
                "state": row.get::<_, String>(7)?, "gap": row.get::<_, Option<String>>(8)?,
                "native_keys": row.get::<_, i64>(9)?, "server_only_at_scan": row.get::<_, i64>(10)?, "results_at_scan": results,
            }));
        }
        let total: i64 = counts.values().filter_map(Value::as_i64).sum();
        let waiting = ["pending", "reader_offline", "reader_unsupported"]
            .into_iter()
            .any(|status| counts.contains_key(status));
        let anomalies = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM token_backfill_audit WHERE job_id = ?1)",
            [job],
            |row| row.get::<_, bool>(0),
        )?;
        let server = super::system::backfill_status(&tx, job)?;
        let server_rows = server.as_array().expect("grouped Server backfill status");
        let state = if waiting || server_rows.iter().any(|row| row["state"] == "pending") {
            "waiting"
        } else if counts.contains_key("gap")
            || counts.contains_key("active_prefix")
            || anomalies
            || server_rows.iter().any(|row| row["state"] != "scanned")
        {
            "finished_with_findings"
        } else {
            "finished"
        };
        Ok(Some(
            json!({"job": serde_json::from_str::<StartTokenBackfill>(&request)?,
            "created_at":created_at,"state": state,"selected_runs":total,"counts":counts,"runs":runs,"next_after":next_after,"server":server}),
        ))
    }

    pub fn token_backfill_audit(&self, job: &str, after: i64) -> Result<Option<Value>> {
        if !self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM token_backfill_jobs WHERE id = ?1)",
            [job],
            |r| r.get::<_, bool>(0),
        )? {
            return Ok(None);
        }
        let mut sql = self.db.prepare(
            "SELECT l.id, l.run_id, r.thread_id, r.turn_id, a.runner_id, l.response_id, l.source, l.evidence
             FROM token_backfill_audit l JOIN token_runs r ON r.run_id = l.run_id
             JOIN runner_assignments a ON a.id = r.assignment_id
             WHERE l.job_id = ?1 AND l.id > ?2 ORDER BY l.id LIMIT ?3",
        )?;
        let mut rows = sql.query(params![job, after, PAGE + 1])?;
        let mut entries = Vec::new();
        let mut next_after = None;
        while let Some(row) = rows.next()? {
            if entries.len() == PAGE as usize {
                next_after = entries
                    .last()
                    .and_then(|value: &Value| value["cursor"].as_i64());
                break;
            }
            let root_thread: String = row.get(2)?;
            let root_turn: String = row.get(3)?;
            let stored: String = row.get(5)?;
            let mut descendant = stored.strip_prefix('\x1f').unwrap_or("").splitn(3, '\x1f');
            let (thread, turn, response) =
                match (descendant.next(), descendant.next(), descendant.next()) {
                    (Some(thread), Some(turn), Some(response)) => (thread, turn, response),
                    _ => (root_thread.as_str(), root_turn.as_str(), stored.as_str()),
                };
            entries.push(json!({"cursor":row.get::<_, i64>(0)?, "run_id":row.get::<_, String>(1)?,
                "thread_id":thread, "turn_id":turn, "response_id":response,
                "root_thread_id":root_thread,"root_turn_id":root_turn,
                "runner_id":row.get::<_, String>(4)?,
                "source":row.get::<_, String>(6)?, "evidence":serde_json::from_str::<Value>(&row.get::<_, String>(7)?)?,
            }));
        }
        Ok(Some(
            json!({"job_id":job,"entries":entries,"next_after":next_after}),
        ))
    }
}
