//! The non-Issue side of the global cache. Issue data still has one owner
//! and one tally path, regardless of how many ancestors display its subtotal.
use super::{Breakdown, Summary, Tally};
use crate::store::Store;
use anyhow::{Result, ensure};
use codex_app::usage::{Response, valid_native_id};
use protocol::{RequestMeter, TokenCounts, TokenModel};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::path::Path;

mod history;
pub(crate) use history::spawn_history;
pub(in crate::store) use history::{backfill_status, enqueue};

#[derive(Clone, Copy)]
pub(crate) enum Source {
    Router,
    Discussion,
    DiscussionSummary,
    HandoffReceiver,
    Feishu,
}

impl Source {
    const ALL: [Self; 5] = [
        Self::Router,
        Self::Discussion,
        Self::DiscussionSummary,
        Self::HandoffReceiver,
        Self::Feishu,
    ];

    const fn key(self) -> &'static str {
        match self {
            Self::Router => "router",
            Self::Discussion => "discussion",
            Self::DiscussionSummary => "discussion_summary",
            Self::HandoffReceiver => "handoff_receiver",
            Self::Feishu => "feishu",
        }
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Stats {
    attempts_without_turn: i64,
    shared_turn_attempts: i64,
    shared_issue_responses: i64,
    responses_without_usage: i64,
    conflicts: i64,
    summary: Summary,
}

pub(in crate::store) fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(include_str!("system.sql"))?;
    // Infer only historical launch provenance from durable Tend records.
    // This migration never opens a native file or fabricates a response.
    db.execute_batch(
        "INSERT INTO token_agent_attempts(id, source, thread, turn, process_pid, process_start, legacy)
         SELECT a.id, CASE
             WHEN d.id IS NOT NULL AND d.manual = 0 THEN 'discussion_summary'
             WHEN discussion.chat_id IS NOT NULL THEN 'discussion'
             WHEN quick.operation_id IS NOT NULL AND json_extract(h.intent, '$.route_with_source') IS NOT 1
                 THEN 'handoff_receiver'
             ELSE 'router' END,
             a.thread_id, a.turn_id, a.process_pid, a.process_start, 1
         FROM router_attempts a JOIN router_inputs i ON a.seq = i.seq
         LEFT JOIN discussions discussion ON i.chat_id = discussion.chat_id
         LEFT JOIN chat_messages m ON m.seq = i.message_seq
         LEFT JOIN discussion_drafts d ON m.id = d.message_id
         LEFT JOIN discussion_handoffs h ON m.id = h.message_id
         LEFT JOIN discussion_quick_handoffs quick ON h.draft_id = quick.draft_id
         ORDER BY a.rowid;
         INSERT INTO token_agent_turns(thread, turn, source, attempt, legacy)
         SELECT thread, turn, source, id, 1 FROM token_agent_attempts WHERE turn IS NOT NULL
         ORDER BY rowid ON CONFLICT(thread, turn) DO NOTHING;",
    )?;
    for source in Source::ALL {
        let (unconfirmed, shared): (i64, i64) = db.query_row(
            "SELECT count(*) FILTER (WHERE a.turn IS NULL),
                 count(*) FILTER (WHERE a.turn IS NOT NULL AND t.attempt IS NOT a.id)
             FROM token_agent_attempts a LEFT JOIN token_agent_turns t
               ON t.thread=a.thread AND t.turn=a.turn WHERE a.source=?1",
            [source.key()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let started = db.query_row(
            "SELECT count(*) FROM token_agent_turns WHERE source=?1",
            [source.key()],
            |r| r.get(0),
        )?;
        write(
            db,
            source.key(),
            &Stats {
                attempts_without_turn: unconfirmed,
                shared_turn_attempts: shared,
                summary: Summary {
                    started_runs: started,
                    legacy_unobserved: started,
                    ..Summary::default()
                },
                ..Stats::default()
            },
        )?;
    }
    Ok(())
}

fn read(db: &Connection, source: &str) -> Result<Stats> {
    let raw: String = db
        .prepare_cached("SELECT stats FROM token_agent_rollups WHERE source=?1")?
        .query_row([source], |r| r.get(0))?;
    Ok(serde_json::from_str(&raw)?)
}

fn write(db: &Connection, source: &str, stats: &Stats) -> Result<()> {
    db.prepare_cached(
        "INSERT INTO token_agent_rollups(source, stats) VALUES (?1, ?2)
         ON CONFLICT(source) DO UPDATE SET stats=excluded.stats",
    )?
    .execute(params![source, serde_json::to_string(stats)?])?;
    Ok(())
}

pub(super) fn migrate_selections(
    db: &Connection,
    source: &str,
    extra: &super::models::ModelProjection,
) -> Result<()> {
    let mut stats = read(db, source)?;
    stats.summary.tally.model_usage.merge(extra, 1);
    write(db, source, &stats)
}

impl Store {
    pub(crate) fn start_feishu_metering(
        &self,
        id: &str,
        thread: &str,
        process: &codex_app::ProcessIdentity,
    ) -> Result<()> {
        let tx = self.db.unchecked_transaction()?;
        launch(&tx, id, Source::Feishu, thread, None, process)?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn bind_feishu_metering(&self, id: &str, turn: &str) -> Result<()> {
        let tx = self.db.unchecked_transaction()?;
        bind(&tx, id, turn)?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn record_agent_child_response(
        &self,
        attempt: &str,
        root: (&str, &str),
        child: (&str, &str),
        response: Response,
    ) {
        let result = (|| -> Result<()> {
            let tx =
                rusqlite::Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
            if let Some(owner) = bind_child(&tx, attempt, root, child)? {
                persist(
                    &tx,
                    &owner,
                    child.0,
                    child.1,
                    &response.id,
                    response.meter,
                    None,
                )?;
            }
            tx.commit()?;
            Ok(())
        })();
        if let Err(error) = result {
            eprintln!("Record child model response for attempt {attempt}: {error:#}");
        }
    }

    pub(crate) fn record_agent_response(
        &self,
        attempt: &str,
        thread: &str,
        turn: &str,
        response: Response,
    ) {
        let result = (|| -> Result<()> {
            let tx =
                rusqlite::Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
            persist(
                &tx,
                attempt,
                thread,
                turn,
                &response.id,
                response.meter,
                None,
            )?;
            tx.commit()?;
            Ok(())
        })();
        if let Err(error) = result {
            eprintln!("Record model response for attempt {attempt}: {error:#}");
        }
    }
}

pub(in crate::store) fn launch(
    db: &Connection,
    id: &str,
    source: Source,
    thread: &str,
    rollout: Option<&Path>,
    process: &codex_app::ProcessIdentity,
) -> Result<()> {
    let key = source.key();
    db.execute(
        "INSERT INTO token_agent_attempts(id, source, thread, rollout, process_pid, process_start)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            id,
            key,
            thread,
            rollout.filter(|p| p.is_absolute()).and_then(Path::to_str),
            process.pid,
            process.started_at
        ],
    )?;
    let mut stats = read(db, key)?;
    stats.attempts_without_turn += 1;
    write(db, key, &stats)
}

pub(in crate::store) fn bind(db: &Connection, id: &str, turn: &str) -> Result<()> {
    let row = db
        .query_row(
            "SELECT source, thread, turn, legacy FROM token_agent_attempts WHERE id=?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, bool>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((source, thread, previous, legacy)) = row else {
        return Ok(());
    };
    ensure!(
        previous.as_deref().is_none_or(|p| p == turn),
        "native metering turn changed"
    );
    if previous.is_some() {
        return Ok(());
    }
    db.execute(
        "UPDATE token_agent_attempts SET turn=?2 WHERE id=?1",
        [id, turn],
    )?;
    let mut stats = read(db, &source)?;
    stats.attempts_without_turn -= 1;
    if db.execute(
        "INSERT INTO token_agent_turns(thread, turn, source, attempt, legacy)
         VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT DO NOTHING",
        params![thread, turn, source, id, legacy],
    )? == 1
    {
        stats.summary.started_runs += 1;
        stats.summary.legacy_unobserved += i64::from(legacy);
    } else {
        stats.shared_turn_attempts += 1;
    }
    write(db, &source, &stats)
}

fn bind_child(
    db: &Connection,
    attempt: &str,
    root: (&str, &str),
    child: (&str, &str),
) -> Result<Option<String>> {
    if child.0 == root.0
        || child.1 == root.1
        || ![child.0, child.1].into_iter().all(valid_native_id)
    {
        return Ok(None);
    }
    let Some((owner, source, legacy)) = db
        .prepare_cached(
            "SELECT t.attempt, t.source, t.legacy FROM token_agent_turns t
         JOIN token_agent_attempts a ON a.thread=t.thread AND a.turn=t.turn
         WHERE a.id=?1 AND t.thread=?2 AND t.turn=?3",
        )?
        .query_row(params![attempt, root.0, root.1], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, bool>(2)?,
            ))
        })
        .optional()?
    else {
        return Ok(None);
    };
    if db
        .prepare_cached(
            "INSERT INTO token_agent_turns(thread,turn,source,attempt,legacy)
         VALUES (?1,?2,?3,?4,?5) ON CONFLICT DO NOTHING",
        )?
        .execute(params![child.0, child.1, source, owner, legacy])?
        == 1
    {
        let mut stats = read(db, &source)?;
        stats.summary.started_runs += 1;
        stats.summary.legacy_unobserved += i64::from(legacy);
        write(db, &source, &stats)?;
    }
    let matches: bool = db
        .prepare_cached("SELECT attempt=?3 FROM token_agent_turns WHERE thread=?1 AND turn=?2")?
        .query_row(params![child.0, child.1, owner], |r| r.get(0))?;
    Ok(matches.then_some(owner))
}

fn issue_has_key(db: &Connection, thread: &str, turn: &str, response: &str) -> Result<bool> {
    Ok(db.prepare_cached(
        "SELECT EXISTS(SELECT 1 FROM token_requests WHERE thread_id=?1 AND turn_id=?2 AND response_id=?3)"
    )?.query_row(params![thread, turn, response], |r| r.get(0))?)
}

pub(super) fn agent_meter(
    db: &Connection,
    key: super::RequestKey<'_>,
) -> Result<Option<RequestMeter>> {
    let meter: Option<String> = db
        .prepare_cached(
            "SELECT meter FROM token_agent_requests
         WHERE thread=?1 AND turn=?2 AND response=?3 AND conflict IS NULL",
        )?
        .query_row(key, |row| row.get(0))
        .optional()?;
    meter
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(Into::into)
}

fn count(stats: &mut Stats, meter: &RequestMeter, conflict: bool, shadowed: bool, sign: i64) {
    if shadowed {
        stats.shared_issue_responses += sign;
        return;
    }
    stats.conflicts += if conflict { sign } else { 0 };
    let unknown = RequestMeter {
        counts: TokenCounts::default(),
        model: Some(TokenModel::Unresolved),
        provider_counts: false,
    };
    let meter = if conflict { &unknown } else { meter };
    stats.responses_without_usage += if meter.counts.missing() { sign } else { 0 };
    stats.summary.tally.count(meter, sign);
}

fn persist(
    db: &Connection,
    attempt: &str,
    thread: &str,
    turn: &str,
    response: &str,
    meter: RequestMeter,
    native_policy: Option<protocol::TokenConflictPolicy>,
) -> Result<bool> {
    if ![thread, turn, response].into_iter().all(valid_native_id) {
        return Ok(false);
    }
    let Some((source, observed, legacy)) = db
        .prepare_cached(
            "SELECT t.source, t.observed, t.legacy FROM token_agent_turns t
         JOIN token_agent_attempts a ON a.id=t.attempt OR (a.thread=t.thread AND a.turn=t.turn)
         WHERE a.id=?1 AND t.thread=?2 AND t.turn=?3",
        )?
        .query_row(params![attempt, thread, turn], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, bool>(1)?,
                r.get::<_, bool>(2)?,
            ))
        })
        .optional()?
    else {
        return Ok(false);
    };
    let previous: Option<(String, Option<String>, bool)> = db.prepare_cached(
        "SELECT meter, conflict, shadowed FROM token_agent_requests WHERE thread=?1 AND turn=?2 AND response=?3"
    )?.query_row(params![thread, turn, response], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?;
    let mut stats = read(db, &source)?;
    let shadowed = issue_has_key(db, thread, turn, response)?;
    let mut conflict = None;
    let mut previous_agent = None;
    let meter = if let Some((old_raw, old_conflict, old_shadowed)) = previous {
        use protocol::TokenConflictPolicy::{PreferNative, PreferServer};
        let old: RequestMeter = serde_json::from_str(&old_raw)?;
        let merged = old.merge(&meter);
        let (next, disagreement) = match (merged, old_conflict, native_policy) {
            (_, Some(_), Some(PreferNative)) => (meter, None),
            (_, Some(_), Some(PreferServer)) => (old.clone(), None),
            (_, Some(conflict), _) => (old.clone(), Some(conflict)),
            (Some(meter), _, _) => (meter, None),
            (None, _, Some(PreferNative)) => (meter, None),
            (None, _, Some(PreferServer)) => (old.clone(), None),
            (None, _, _) => (old.clone(), Some(serde_json::to_string(&meter)?)),
        };
        let old_had_conflict = db.prepare_cached(
            "SELECT conflict IS NOT NULL FROM token_agent_requests WHERE thread=?1 AND turn=?2 AND response=?3"
        )?.query_row(params![thread, turn, response], |r| r.get::<_, bool>(0))?;
        if old == next && old_had_conflict == disagreement.is_some() && old_shadowed == shadowed {
            return Ok(disagreement.is_some());
        }
        count(&mut stats, &old, old_had_conflict, old_shadowed, -1);
        if !old_had_conflict {
            previous_agent = Some(old);
        }
        conflict = disagreement;
        next
    } else {
        meter
    };
    db.prepare_cached(
        "INSERT INTO token_agent_requests(thread, turn, response, meter, conflict, shadowed)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(thread, turn, response) DO UPDATE
         SET meter=excluded.meter, conflict=excluded.conflict, shadowed=excluded.shadowed",
    )?
    .execute(params![
        thread,
        turn,
        response,
        serde_json::to_string(&meter)?,
        conflict,
        shadowed
    ])?;
    count(&mut stats, &meter, conflict.is_some(), shadowed, 1);
    if !observed {
        db.execute(
            "UPDATE token_agent_turns SET observed=1 WHERE thread=?1 AND turn=?2",
            [thread, turn],
        )?;
        stats.summary.observed_runs += 1;
        stats.summary.legacy_unobserved -= i64::from(legacy);
    }
    write(db, &source, &stats)?;
    if shadowed {
        super::agent_model_changed(
            db,
            (thread, turn, response),
            previous_agent.as_ref(),
            conflict.is_none().then_some(&meter),
        )?;
    }
    Ok(conflict.is_some())
}

// The Issue ledger owns overlapping request keys. Recheck only explicitly
// touched turns, including a historical correction that removes such a key.
pub(super) fn sync_overlap(db: &Connection, thread: &str, turn: &str) -> Result<()> {
    let mut query = db.prepare_cached(
        "SELECT q.response, q.meter, q.conflict IS NOT NULL, q.shadowed, t.source
         FROM token_agent_requests q JOIN token_agent_turns t USING(thread,turn)
         WHERE q.thread=?1 AND q.turn=?2 AND q.shadowed != EXISTS(
             SELECT 1 FROM token_requests i
             WHERE i.thread_id=q.thread AND i.turn_id=q.turn AND i.response_id=q.response
         )",
    )?;
    let mut rows = query.query(params![thread, turn])?;
    let mut changed: Option<(String, Stats)> = None;
    while let Some(row) = rows.next()? {
        let response: String = row.get(0)?;
        let meter = serde_json::from_str(row.get_ref(1)?.as_str()?)?;
        let conflict = row.get(2)?;
        let shadowed: bool = row.get(3)?;
        if changed.is_none() {
            let source: String = row.get(4)?;
            changed = Some((source.clone(), read(db, &source)?));
        }
        let stats = &mut changed.as_mut().expect("stats loaded").1;
        count(stats, &meter, conflict, shadowed, -1);
        count(stats, &meter, conflict, !shadowed, 1);
        db.execute(
            "UPDATE token_agent_requests SET shadowed=?4 WHERE thread=?1 AND turn=?2 AND response=?3",
            params![thread, turn, response, !shadowed],
        )?;
    }
    if let Some((source, stats)) = changed {
        write(db, &source, &stats)?;
    }
    Ok(())
}

#[derive(Serialize)]
pub(super) struct SourceUsage {
    source: &'static str,
    usage: Breakdown,
    attempts_without_turn: i64,
    shared_turn_attempts: i64,
    shared_issue_responses: i64,
    responses_without_usage: Option<i64>,
    conflicts: i64,
}

pub(super) fn read_overview(
    db: &Connection,
    issue: Summary,
) -> Result<(Breakdown, Vec<SourceUsage>)> {
    let mut tally: Tally = issue.tally.clone();
    let mut rows = vec![SourceUsage {
        source: "runner",
        usage: issue.clone().into(),
        attempts_without_turn: 0,
        shared_turn_attempts: 0,
        shared_issue_responses: 0,
        responses_without_usage: None,
        conflicts: 0,
    }];
    for source in Source::ALL {
        let stats = read(db, source.key())?;
        tally.merge(&stats.summary.tally, 1);
        rows.push(SourceUsage {
            source: source.key(),
            usage: stats.summary.into(),
            attempts_without_turn: stats.attempts_without_turn,
            shared_turn_attempts: stats.shared_turn_attempts,
            shared_issue_responses: stats.shared_issue_responses,
            responses_without_usage: Some(stats.responses_without_usage),
            conflicts: stats.conflicts,
        });
    }
    // Keep the global run fields backward-compatible: they describe the Issue
    // Runs only. Each server source has its own native turn denominator.
    Ok((Summary { tally, ..issue }.into(), rows))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::token_usage::tests::{
        Fixture, assignment, base, issue, native_run, observation, sync,
    };
    use serde_json::{Value, json, to_value};

    fn response(id: &str, total: Option<i64>) -> Response {
        Response {
            id: id.into(),
            meter: RequestMeter {
                counts: TokenCounts {
                    total_tokens: total,
                    ..TokenCounts::default()
                },
                model: Some(TokenModel::Unresolved),
                provider_counts: false,
            },
        }
    }

    fn attempt(store: &Store, id: &str, source: Source, thread: &str, turn: &str) -> Result<()> {
        let process = codex_app::ProcessIdentity {
            pid: 1,
            started_at: "fixture".into(),
        };
        launch(&store.db, id, source, thread, None, &process)?;
        bind(&store.db, id, turn)
    }

    fn root_page(store: &Store) -> Result<Value> {
        Ok(to_value(store.token_overview(None, None)?.unwrap())?)
    }

    fn row(page: &Value, key: &str) -> Value {
        page["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["source"] == key)
            .unwrap()
            .clone()
    }

    #[test]
    fn canonical_turns_cross_source_and_issue_keys_do_not_double_count() -> Result<()> {
        let file = Fixture::new()?;
        let mut store = Store::initialize(&file.file())?;
        base(&store.db)?;
        issue(&store.db, "issue", None)?;
        assignment(&store.db, "task", "issue", "runner")?;
        let run = native_run("run", "task");
        let thread = run.thread_id.as_deref().unwrap();
        let turn = run.turn_id.as_deref().unwrap();
        let copy = observation(&run, 0, Some(11));
        let mut unknown = observation(&run, 1, None);
        unknown.last = TokenCounts::default();
        unknown.model = Some(TokenModel::Unresolved);
        attempt(&store, "a", Source::Router, thread, turn)?;
        store.record_agent_response("a", thread, turn, response(&unknown.response_id, None));

        attempt(&store, "duplicate", Source::Discussion, thread, turn)?;
        attempt(&store, "b", Source::Discussion, thread, "different-turn")?;
        store.record_agent_response("a", thread, turn, response(&copy.response_id, Some(11)));
        store.record_agent_response(
            "duplicate",
            thread,
            turn,
            response(&copy.response_id, Some(11)),
        );
        store.record_agent_response("duplicate", thread, turn, response("unique", Some(4)));
        store.record_agent_response(
            "b",
            thread,
            "different-turn",
            response(&copy.response_id, None),
        );
        store.record_agent_response("b", thread, turn, response("wrong-binding", Some(900)));
        store.record_agent_child_response(
            "a",
            (thread, turn),
            ("child", "child-turn"),
            response("unique", Some(3)),
        );

        let first = root_page(&store)?;
        assert_eq!(
            first["total"]["total_tokens"],
            json!({"tokens":"18","reported_requests":3})
        );
        assert_eq!(first["total"]["reported_requests"], 5);
        assert_eq!(row(&first, "router")["usage"]["started_runs"], 2);
        assert_eq!(row(&first, "discussion")["shared_turn_attempts"], 1);
        assert_eq!(row(&first, "discussion")["responses_without_usage"], 1);

        // Sync arrives after the Server copy. Only the exact shared response
        // is shadowed, even though another Server response shares the turn.
        sync(
            &mut store,
            "runner",
            std::slice::from_ref(&run),
            &[copy.clone(), unknown.clone()],
        )?;
        let second = root_page(&store)?;
        assert_eq!(
            second["total"]["total_tokens"],
            first["total"]["total_tokens"]
        );
        assert_eq!(row(&second, "router")["shared_issue_responses"], 2);
        assert_eq!(second["total"]["reported_requests"], 5);
        assert_eq!(second["issues"][0]["direct"]["reported_requests"], 2);
        assert_eq!(
            row(&second, "router")["usage"]["total_tokens"]["tokens"],
            "7"
        );
        assert_eq!(
            row(&second, "runner")["usage"]["total_tokens"]["tokens"],
            "11"
        );
        assert_eq!(
            second["issues"][0]["direct"]["total_tokens"]["tokens"],
            "11"
        );
        sync(&mut store, "runner", &[run], &[copy, unknown])?;
        drop(store);
        let mut reopened = Store::open(&file.file())?;
        assert_eq!(root_page(&reopened)?, second);
        reopened.rebuild_token_usage(&protocol::TokenUsageScope::All)?;
        assert_eq!(root_page(&reopened)?, second);
        Ok(())
    }

    #[test]
    fn conflicting_values_leave_one_identity_with_unknown_usage() -> Result<()> {
        let file = Fixture::new()?;
        let store = Store::initialize(&file.file())?;
        attempt(&store, "a", Source::Feishu, "thread", "turn")?;
        for value in [Some(0), Some(500), Some(0), None] {
            store.record_agent_response("a", "thread", "turn", response("one", value));
        }
        let page = root_page(&store)?;
        assert_eq!(page["total"]["reported_requests"], 1);
        assert_eq!(page["total"]["total_tokens"]["tokens"], Value::Null);
        let feishu = row(&page, "feishu");
        assert_eq!(feishu["responses_without_usage"], 1);
        assert_eq!(feishu["conflicts"], 1);
        assert_eq!(feishu["usage"]["estimate"]["usd"], Value::Null);
        Ok(())
    }

    #[test]
    fn selection_upgrade_does_not_readd_shadowed_or_conflicting_server_requests() -> Result<()> {
        let file = Fixture::new()?;
        let mut store = Store::initialize(&file.file())?;
        base(&store.db)?;
        issue(&store.db, "issue", None)?;
        assignment(&store.db, "task", "issue", "runner")?;
        let run = native_run("run", "task");
        let thread = run.thread_id.as_deref().unwrap();
        let turn = run.turn_id.as_deref().unwrap();
        let mut copy = observation(&run, 0, Some(11));
        copy.last.input_tokens = Some(8); // The shadowed Server meter cannot supply its model.
        attempt(&store, "a", Source::Router, thread, turn)?;
        for (id, total) in [
            ("unique", 21),
            ("conflict", 22),
            ("conflict", 23),
            (&copy.response_id, 11),
        ] {
            let mut response = response(id, Some(total));
            response.meter.model = Some(TokenModel::TurnContext("gpt-6.2-sol".into()));
            response.meter.counts.input_tokens = Some(9);
            response.meter.counts.output_tokens = Some(2);
            store.record_agent_response("a", thread, turn, response);
        }
        sync(&mut store, "runner", std::slice::from_ref(&run), &[copy])?;
        let expected = root_page(&store)?;
        let router = row(&expected, "router");
        assert_eq!(router["shared_issue_responses"], 1);
        assert_eq!(router["conflicts"], 1);
        assert_eq!(router["usage"]["models"][0]["selected_model_requests"], 1);
        assert_eq!(router["usage"]["estimate"]["usd"], "0.00002");
        store.db.execute_batch(
            "UPDATE token_agent_rollups SET stats=json_remove(stats,'$.summary.tally.model_usage.selected')
             WHERE source='router';
             PRAGMA user_version=48;",
        )?;
        drop(store);
        assert_eq!(root_page(&Store::initialize(&file.file())?)?, expected);
        Ok(())
    }

    #[test]
    fn shared_request_uses_compatible_model_in_both_arrival_orders_and_rebuild() -> Result<()> {
        for agent_first in [true, false] {
            let file = Fixture::new()?;
            let mut store = Store::initialize(&file.file())?;
            base(&store.db)?;
            issue(&store.db, "root", None)?;
            issue(&store.db, "issue", Some("root"))?;
            assignment(&store.db, "task", "issue", "runner")?;
            let run = native_run("run", "task");
            let thread = run.thread_id.as_deref().unwrap();
            let turn = run.turn_id.as_deref().unwrap();
            let mut selected = observation(&run, 0, None);
            selected.last =
                TokenCounts::from_fields([Some(863), Some(831), Some(180), None, Some(32), None]);
            selected.model = Some(TokenModel::TurnContext("gpt-6.2-sol".into()));
            let observed = |total| Response {
                id: selected.response_id.clone(),
                meter: RequestMeter {
                    counts: TokenCounts {
                        total_tokens: Some(total),
                        ..selected.last
                    },
                    model: Some(TokenModel::Response("gpt-6.2-astra".into())),
                    provider_counts: false,
                },
            };
            attempt(&store, "a", Source::Router, thread, turn)?;
            if agent_first {
                store.record_agent_response("a", thread, turn, observed(863));
            }
            sync(
                &mut store,
                "runner",
                std::slice::from_ref(&run),
                std::slice::from_ref(&selected),
            )?;
            if !agent_first {
                assert_eq!(root_page(&store)?["total"]["estimate"]["usd"], "0.000338");
                store
                    .db
                    .execute("UPDATE token_rebuild_state SET collecting=1", [])?;
                store.record_agent_response("a", thread, turn, observed(863));
                assert_eq!(
                    store.db.query_row(
                        "SELECT count(*) FROM token_rebuild_dirty WHERE run_id='run'",
                        [],
                        |r| r.get::<_, i64>(0)
                    )?,
                    1
                );
            }
            let page = root_page(&store)?;
            assert_eq!(row(&page, "router")["shared_issue_responses"], 1);
            assert_eq!(row(&page, "router")["usage"]["reported_requests"], 0);
            assert_eq!(page["total"]["reported_requests"], 1);
            assert_eq!(page["total"]["models"][0]["model"], "gpt-6.2-astra");
            assert_eq!(page["total"]["estimate"]["usd"], "0.001352");
            assert_eq!(page["issues"][0]["subtree"], page["total"]);
            assert_eq!(
                store.db.query_row(
                    "SELECT json_extract(last,'$.model.id') FROM token_requests",
                    [],
                    |r| r.get::<_, String>(0)
                )?,
                "gpt-6.2-sol"
            );
            store.rebuild_token_usage(&protocol::TokenUsageScope::All)?;
            assert_eq!(root_page(&store)?, page);
            if agent_first {
                // Schema 48 recorded the Issue's base selection as a count,
                // without per-model quantities or borrowed response evidence.
                let old = "{\"confirmed\":{},\"requested\":{\"gpt-6.2-sol\":1},\"fresh\":{\"known\":0,\"zero\":0,\"units\":0}}";
                store.db.execute(
                    "UPDATE token_run_tallies SET tally=json_set(tally,'$.model_usage',json(?1))",
                    [old],
                )?;
                store.db.execute("UPDATE token_issue_rollups SET summary=json_set(summary,'$.tally.model_usage',json(?1)) WHERE json_extract(summary,'$.tally.valid_records')>0", [old])?;
                store.db.execute("UPDATE token_global_rollup SET summary=json_set(summary,'$.tally.model_usage',json(?1))", [old])?;
                store.db.pragma_update(None, "user_version", 48)?;
                drop(store);
                store = Store::initialize(&file.file())?;
                assert_eq!(root_page(&store)?, page);
            }
            store.record_agent_response("a", thread, turn, observed(900));
            let after = root_page(&store)?;
            assert_eq!(after["total"]["models"][0]["model"], "gpt-6.2-sol");
            assert_eq!(after["total"]["estimate"]["usd"], "0.000338");
            assert_eq!(after["total"]["reported_requests"], 1);
            store.rebuild_token_usage(&protocol::TokenUsageScope::Issue("root".into()))?;
            assert_eq!(root_page(&store)?, after);
        }
        Ok(())
    }

    #[test]
    fn native_server_rescan_replaces_only_a_reproduced_legacy_placeholder() -> Result<()> {
        let file = Fixture::new()?;
        let store = Store::initialize(&file.file())?;
        attempt(&store, "a", Source::Router, "thread", "turn")?;
        let rescan = |model| {
            let counts =
                TokenCounts::from_fields([Some(863), Some(831), Some(180), None, Some(32), None]);
            let meter = RequestMeter {
                counts,
                model: Some(model),
                provider_counts: false,
            };
            persist(
                &store.db,
                "a",
                "thread",
                "turn",
                "response",
                meter,
                Some(protocol::TokenConflictPolicy::Quarantine),
            )?;
            root_page(&store)
        };
        assert!(rescan(TokenModel::Ambiguous)?["total"]["estimate"]["usd"].is_null());
        let corrected = TokenModel::TurnContext("gpt-6.2-sol".into())
            .rechecked(protocol::ModelRepair::native(true, false));
        let page = rescan(corrected)?;
        assert_eq!(page["total"]["estimate"]["usd"], "0.000338");
        assert_eq!(rescan(TokenModel::Ambiguous)?, page);
        assert!(rescan(TokenModel::TurnConflict)?["total"]["estimate"]["usd"].is_null());
        Ok(())
    }
}
