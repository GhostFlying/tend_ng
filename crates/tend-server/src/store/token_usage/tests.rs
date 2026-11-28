use super::*;
use protocol::{RUNNER_SYNC_BATCH, RunOutcome, TokenModel};
use serde_json::{Value, json, to_value};
use std::{fs, path::PathBuf, time::Instant};

pub(super) struct Fixture(PathBuf);

impl Fixture {
    pub(super) fn new() -> Result<Self> {
        let dir = std::env::temp_dir().join(format!("tend-usage-{}", protocol::new_id()));
        fs::create_dir(&dir)?;
        Ok(Self(dir))
    }
    pub(super) fn file(&self) -> PathBuf {
        self.0.join("fixture.sqlite")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub(super) fn base(db: &Connection) -> Result<()> {
    db.execute_batch(
        "INSERT INTO chat_messages (seq, id, chat_id, text) VALUES (1, 'source', 'fixture', 'Synthetic');
         INSERT INTO runners (id, host_id, profile, last_seen) VALUES ('runner', 'local', '{}', 0), ('other', 'other', '{}', 0);",
    )?;
    Ok(())
}

pub(super) fn issue(db: &Connection, id: &str, parent: Option<&str>) -> Result<()> {
    db.prepare_cached("INSERT INTO issues(id, source_seq, title, objective, parent_issue_id) VALUES(?1, 1, ?1, '', ?2)")?
        .execute(params![id, parent])?;
    Ok(())
}

pub(super) fn assignment(db: &Connection, id: &str, issue: &str, runner: &str) -> Result<()> {
    db.prepare_cached(
        "INSERT INTO tasks (id, issue_id, category, status, repo_ids, required_capabilities)
         VALUES (?1, ?2, 'implementation', 'accepted', '[]', '[]')",
    )?
    .execute(params![id, issue])?;
    db.prepare_cached("INSERT INTO runner_assignments (id, task_id, runner_id, contract, state, expires_at) VALUES (?1, ?1, ?2, ?3, 'accepted', 0)")?
        .execute(params![id, runner, r#"{"placement":{"agent":{"id":"work"}}}"#])?;
    db.prepare_cached("UPDATE tasks SET current_assignment = ?1 WHERE id = ?1")?
        .execute([id])?;
    Ok(())
}

pub(super) fn native_run(run: &str, task: &str) -> RunObservation {
    RunObservation {
        event_id: format!("start-{run}"),
        assignment_id: task.into(),
        task_id: task.into(),
        run_id: run.into(),
        thread_id: Some(format!("thread-{run}")),
        turn_id: Some(format!("turn-{run}")),
        input_ids: vec![],
        branches: vec![],
        started_at: None,
        phase: RunPhase::Started,
    }
}

pub(super) fn observation(run: &RunObservation, cursor: i64, total: Option<i64>) -> RunTokenUsage {
    RunTokenUsage {
        event_id: format!("{}-request-{cursor}", run.run_id),
        assignment_id: run.assignment_id.clone(),
        task_id: run.task_id.clone(),
        run_id: run.run_id.clone(),
        thread_id: run.thread_id.clone().unwrap(),
        turn_id: run.turn_id.clone().unwrap(),
        response_id: format!("resp-{cursor}"),
        root: None,
        provider_counts: false,
        last: TokenCounts {
            total_tokens: total,
            cached_input_tokens: Some(0),
            ..TokenCounts::default()
        },
        observed_at: 42,
        model: None,
    }
}

pub(super) fn sync(
    store: &mut Store,
    runner: &str,
    runs: &[RunObservation],
    usage: &[RunTokenUsage],
) -> Result<()> {
    let tx = store.db.transaction()?;
    for run in runs {
        assert!(crate::store::run::apply(&tx, runner, run)?);
    }
    let receipts = apply_batch(&tx, runner, runs, usage)?;
    assert_eq!(
        receipts,
        usage
            .iter()
            .map(|u| u.event_id.as_str())
            .collect::<Vec<_>>()
    );
    tx.commit()?;
    Ok(())
}

pub(super) fn view(store: &Store, id: &str) -> Result<Value> {
    Ok(to_value(store.issue_token_usage(id)?.unwrap())?)
}

#[test]
fn model_evidence_enriches_and_retracts_without_quarantining_valid_counts() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut store = Store::initialize(&fixture.file())?;
    base(&store.db)?;
    issue(&store.db, "root", None)?;
    issue(&store.db, "child", Some("root"))?;
    assignment(&store.db, "job", "child", "runner")?;
    let run = native_run("test-run", "job");
    let mut record = observation(&run, 1, Some(101));
    record.last = TokenCounts::from_fields([101, 100, 20, 10, 1, 0].map(Some));
    sync(&mut store, "runner", &[run], std::slice::from_ref(&record))?;
    let read = |s: &Store| -> Result<Value> {
        let global = to_value(s.token_overview(None, None)?)?["total"].clone();
        assert_eq!(global, view(s, "root")?["subtree"]);
        assert_eq!(global, view(s, "child")?["direct"]);
        assert_eq!(global["reported_requests"], 1);
        Ok(global)
    };
    let legacy = read(&store)?;
    assert_eq!(legacy["models"][0]["model"], Value::Null);
    assert_eq!(
        legacy["models"][0]["uncached_input_tokens"]["tokens"],
        Value::Null
    );
    let mut change = |n, model| -> Result<Value> {
        record.event_id = format!("new-{n}");
        record.model = model;
        sync(&mut store, "runner", &[], std::slice::from_ref(&record))?;
        read(&store)
    };
    let sol = TokenModel::TurnContext("gpt-6.2-sol".into());
    let astra = TokenModel::Response("gpt-6.2-astra".into());
    let unresolved = change(0, Some(TokenModel::Unresolved))?;
    assert_eq!(
        unresolved["models"][0]["uncached_input_tokens"]["tokens"],
        "70"
    );
    assert_eq!(unresolved["models"][0]["model"], Value::Null);
    let projected = change(1, Some(sol.clone()))?;
    assert_eq!(projected["models"][0]["model"], "gpt-6.2-sol");
    assert_eq!(projected["models"][0]["selected_model_requests"], 1);
    assert_eq!(
        projected["models"][0]["uncached_input_tokens"]["tokens"],
        "70"
    );
    assert_eq!(projected["estimate"]["usd"], "0.000177");
    assert_eq!(projected, change(2, None)?);
    let inferred = change(3, Some(TokenModel::TurnContext("conflict".into())))?;
    assert_eq!(inferred["models"][0]["model"], Value::Null);
    let confirmed = change(4, Some(astra))?;
    assert_eq!(confirmed["models"][0]["model"], "gpt-6.2-astra");
    assert_eq!(confirmed["models"][0]["response_model_requests"], 1);
    assert_eq!(confirmed["models"][0]["requested_models"], json!({}));
    assert_eq!(confirmed["estimate"]["usd"], "0.000708");
    let contradicted = change(5, Some(TokenModel::Response("gpt-6.2-sol".into())))?;
    assert_eq!(contradicted["models"][0]["model"], Value::Null);
    assert_eq!(contradicted["total_tokens"]["tokens"], "101");
    assert_eq!(contradicted, change(6, Some(sol))?);
    store.rebuild_token_usage(&protocol::TokenUsageScope::All)?;
    assert_eq!(contradicted, read(&store)?);
    Ok(())
}

#[test]
fn upgrades_existing_selected_models_and_merges_exact_catalog_aliases() -> Result<()> {
    let fixture = Fixture::new()?;
    let path = fixture.file();
    let mut store = Store::initialize(&path)?;
    base(&store.db)?;
    issue(&store.db, "root", None)?;
    issue(&store.db, "child", Some("root"))?;
    assignment(&store.db, "job", "child", "runner")?;
    let run = native_run("test-run", "job");
    let mut records = [
        observation(&run, 1, None),
        observation(&run, 2, None),
        observation(&run, 3, None),
    ];
    records[0].model = Some(TokenModel::TurnContext("gpt-6.2-sol".into()));
    records[0].last = TokenCounts::from_fields([101, 100, 20, 10, 1, 0].map(Some));
    records[1].model = Some(TokenModel::Response("openai/gpt-6.2-sol-20261117".into()));
    records[1].last = TokenCounts::from_fields([272002, 272001, 20, 10, 1, 0].map(Some));
    records[2].model = Some(TokenModel::TurnContext("gpt-6.2-sol-unlisted".into()));
    records[2].last = records[0].last;
    sync(&mut store, "runner", std::slice::from_ref(&run), &records)?;
    let expected = view(&store, "root")?["subtree"].clone();
    assert_eq!(expected["models"].as_array().unwrap().len(), 2);
    assert_eq!(expected["models"][0]["model"], "gpt-6.2-sol");
    assert_eq!(expected["models"][0]["selected_model_requests"], 1);
    assert_eq!(expected["models"][0]["response_model_requests"], 1);
    assert_eq!(expected["models"][0]["estimate"]["usd"], "1.08813");
    assert_eq!(expected["models"][1]["estimate_gap"], "no_exact_price");
    assert!(expected["models"][1]["estimate"]["usd"].is_null());
    assert_eq!(
        expected["estimate"],
        json!({"usd":"1.08813", "partial":true,"unpriced_requests":1})
    );

    store.db.execute_batch(
        "UPDATE token_run_tallies SET tally=json_set(json_remove(tally,'$.model_usage.selected'),
           '$.model_usage.requested', json('{\"gpt-6.2-sol\":1,\"gpt-6.2-sol-unlisted\":1}'));
         UPDATE token_issue_rollups SET summary=json_remove(summary,'$.tally.model_usage.selected')
           WHERE json_type(summary,'$.tally.model_usage.selected') IS NOT NULL;
         UPDATE token_global_rollup SET summary=json_remove(summary,'$.tally.model_usage.selected');
         PRAGMA user_version=48;",
    )?;
    drop(store);
    let mut upgraded = Store::initialize(&path)?;
    assert_eq!(view(&upgraded, "root")?["subtree"], expected);
    assert_eq!(view(&upgraded, "child")?["direct"], expected);
    assert_eq!(
        to_value(upgraded.token_overview(None, None)?)?["total"],
        expected
    );
    upgraded.rebuild_token_usage(&protocol::TokenUsageScope::All)?;
    assert_eq!(view(&upgraded, "root")?["subtree"], expected);
    drop(upgraded);
    assert_eq!(
        view(&Store::initialize(&path)?, "root")?["subtree"],
        expected
    );
    Ok(())
}

#[test]
fn retains_zero_and_provable_long_rate_in_rebuild() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut store = Store::initialize(&fixture.file())?;
    base(&store.db)?;
    issue(&store.db, "root", None)?;
    assignment(&store.db, "job", "root", "runner")?;
    let run = native_run("test-run", "job");
    let mut records = vec![
        observation(&run, 1, None),
        observation(&run, 2, None),
        observation(&run, 3, None),
    ];
    for (record, (read, output)) in records
        .iter_mut()
        .zip([(0, 0), (272_000, 10), (272_001, 10)])
    {
        record.last =
            TokenCounts::from_fields([None, None, Some(read), Some(0), Some(output), None]);
        record.model = Some(TokenModel::Response("gpt-6.2-sol".into()));
    }
    sync(&mut store, "runner", &[run], &records)?;
    let before = view(&store, "root")?;
    assert_eq!(
        before["direct"]["estimate"],
        json!({"usd":"0.0545502","partial":true,"unpriced_requests":1})
    );
    let tiers = &before["direct"]["models"][0]["price"]["tiers"];
    assert_eq!(tiers[0]["tier"], "long");
    assert_eq!(tiers[1]["tier"], "unknown");
    assert_eq!(
        tiers[1]["components"][3]["estimate"],
        json!({"usd":"0","partial":true})
    );
    store.rebuild_token_usage(&protocol::TokenUsageScope::All)?;
    assert_eq!(before, view(&store, "root")?);
    Ok(())
}

#[test]
fn verifies_original_identity_and_counts_each_run_once_even_when_batches_reorder() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut store = Store::initialize(&fixture.file())?;
    base(&store.db)?;
    issue(&store.db, "root", None)?;
    issue(&store.db, "child", Some("root"))?;
    issue(&store.db, "grand", Some("child"))?;
    issue(&store.db, "outside", None)?;
    for (task, id) in [
        ("parent-a", "root"),
        ("child-a", "child"),
        ("grand-a", "grand"),
        ("outside-a", "outside"),
    ] {
        assignment(&store.db, task, id, "runner")?;
    }
    let parent = native_run("parent-run", "parent-a");
    let child = native_run("child-run", "child-a");
    let grand = native_run("grand-run", "grand-a");
    let outside = native_run("outside-run", "outside-a");
    let mut parent_usage = observation(&parent, 50, Some(20));
    parent_usage.provider_counts = true;
    let child_usage = observation(&child, 70, Some(25));
    let outside_usage = observation(&outside, 60, Some(50));

    // Usage arrives twice before Started. Unknown work cannot enter the tally.
    sync(
        &mut store,
        "runner",
        &[],
        std::slice::from_ref(&parent_usage),
    )?;
    sync(
        &mut store,
        "runner",
        &[],
        std::slice::from_ref(&parent_usage),
    )?;
    let mut wrong_turn = observation(&parent, 200, Some(100));
    wrong_turn.turn_id = "wrong-turn".into();
    let router = observation(&native_run("router-run", "router-only"), 1_000, Some(1_000));
    sync(&mut store, "runner", &[], &[wrong_turn, router])?;
    sync(&mut store, "other", &[], std::slice::from_ref(&child_usage))?;
    assert_eq!(view(&store, "root")?["subtree"]["started_runs"], 0);
    assert_eq!(
        store
            .db
            .query_row("SELECT count(*) FROM token_usage_pending", [], |r| r
                .get::<_, i64>(0))?,
        2
    );

    sync(
        &mut store,
        "runner",
        &[parent, child, grand, outside],
        &[child_usage.clone(), outside_usage],
    )?;
    let tree = view(&store, "root")?;
    assert_eq!(tree["direct"]["total_tokens"]["tokens"], "20");
    assert_eq!(tree["subtree"]["total_tokens"]["tokens"], "45");
    assert_eq!(tree["subtree"]["input_tokens"]["tokens"], Value::Null);
    assert_eq!(tree["subtree"]["cached_input_tokens"]["tokens"], "0");
    assert_eq!(tree["subtree"]["started_runs"], 3);
    assert_eq!(tree["subtree"]["observed_runs"], 2);
    assert_eq!(tree["subtree"]["unobserved_no_usage"], 1);
    assert_eq!(tree["nodes"].as_array().unwrap().len(), 3);
    assert_eq!(tree["nodes"][1]["direct"]["total_tokens"]["tokens"], "25");
    assert_eq!(view(&store, "child")?["subtree"]["started_runs"], 2);

    let mut duplicate = child_usage.clone();
    duplicate.event_id = "other-receipt-same-request".into();
    sync(
        &mut store,
        "runner",
        &[],
        &[parent_usage.clone(), duplicate],
    )?;
    assert_eq!(view(&store, "root")?, tree);
    assert_eq!(
        store
            .db
            .query_row("SELECT count(*) FROM token_usage_pending", [], |r| r
                .get::<_, i64>(0))?,
        0
    );

    let mut conflict = child_usage.clone();
    conflict.last.total_tokens = Some(26);
    conflict.event_id = "conflict-receipt".into();
    for _ in 0..2 {
        sync(
            &mut store,
            "runner",
            &[],
            &[child_usage.clone(), conflict.clone()],
        )?;
        let tree = view(&store, "root")?;
        assert_eq!(tree["subtree"]["started_runs"], 3);
        assert_eq!(tree["subtree"]["observed_runs"], 1);
        assert_eq!(tree["subtree"]["total_tokens"]["tokens"], "20");
        assert_eq!(
            tree["nodes"][1]["direct"]["total_tokens"]["tokens"],
            Value::Null
        );
    }
    assert_eq!(
        store.db.query_row(
            "SELECT count(*) FROM token_requests WHERE conflict IS NOT NULL",
            [],
            |r| r.get::<_, i64>(0)
        )?,
        1
    );
    assert!(store.issue_token_usage("unknown")?.is_none());
    Ok(())
}

#[test]
fn migration_counts_only_accepted_starts_and_late_use_repairs_the_original_run() -> Result<()> {
    let fixture = Fixture::new()?;
    let path = fixture.file();
    let store = Store::initialize(&path)?;
    base(&store.db)?;
    issue(&store.db, "root", None)?;
    issue(&store.db, "child", Some("root"))?;
    assignment(&store.db, "original", "child", "runner")?;
    assignment(&store.db, "not-started", "root", "runner")?;
    let start = native_run("old", "original");
    assert!(crate::store::run::apply(&store.db, "runner", &start)?);
    let mut stop = native_run("old", "original");
    stop.event_id = "stop-old".into();
    stop.phase = RunPhase::Stopped {
        outcome: RunOutcome::Cancelled,
        error: None,
        waiting_for_children: false,
        waiting_for_attention: false,
        waiting_for_external: false,
        queued_input: false,
    };
    assert!(crate::store::run::apply(&store.db, "runner", &stop)?);
    // Stopped-before-start with ids present does not establish a denominator.
    let mut failed = native_run("fail", "not-started");
    failed.phase = stop.phase;
    assert!(crate::store::run::apply(&store.db, "runner", &failed)?);
    crate::store::remove_token_usage_schema(&store.db)?;
    crate::store::codebase_comments::drop_schema(&store.db)?;
    crate::store::discussion::remove_quick_handoff_and_title_schema(&store.db)?;
    store.db.pragma_update(None, "user_version", 37)?;
    drop(store);

    let mut store = Store::initialize(&path)?;
    let rollup = view(&store, "root")?;
    assert_eq!(rollup["subtree"]["unobserved_before_collection"], 1);
    assert_eq!(rollup["subtree"]["started_runs"], 1);
    assert_eq!(rollup["subtree"]["total_tokens"]["tokens"], Value::Null);

    // A replacement already owns and started the same Task. Replay cannot
    // change its lifecycle or assign the old usage to the replacement.
    store.db.execute(
        "INSERT INTO runner_assignments (id, task_id, runner_id, contract, state, expires_at)
                     VALUES ('replacement', 'original', 'other', ?1, 'accepted', 0)",
        [r#"{"placement":{"agent":{"id":"work"}}}"#],
    )?;
    store.db.execute(
        "UPDATE tasks SET current_assignment='replacement', status='accepted' WHERE id='original'",
        [],
    )?;
    let mut successor = native_run("new", "original");
    successor.assignment_id = "replacement".into();
    sync(&mut store, "other", &[successor], &[])?;
    let late = observation(&start, 0, Some(0));
    sync(&mut store, "runner", &[start], &[late])?;
    let rollup = view(&store, "root")?;
    assert_eq!(rollup["subtree"]["started_runs"], 2);
    assert_eq!(rollup["subtree"]["observed_runs"], 1);
    assert_eq!(rollup["subtree"]["unobserved_before_collection"], 0);
    assert_eq!(rollup["subtree"]["unobserved_no_usage"], 1);
    assert_eq!(rollup["subtree"]["total_tokens"]["tokens"], "0");
    assert_eq!(
        store
            .db
            .query_row("SELECT run_id FROM token_requests", [], |r| r
                .get::<_, String>(0))?,
        "old"
    );
    assert_eq!(
        store
            .db
            .query_row("SELECT status FROM tasks WHERE id='original'", [], |r| {
                r.get::<_, String>(0)
            })?,
        "running"
    );
    Ok(())
}

fn sample_ms(samples: &mut [f64]) -> Value {
    samples.sort_by(f64::total_cmp);
    json!({"p50": samples[samples.len() / 2], "p95": samples[(samples.len() * 95 / 100).min(samples.len() - 1)], "max": samples[samples.len() - 1]})
}

#[test]
#[ignore = "representative-volume SQLite benchmark; run in release mode"]
fn token_usage_scale() -> Result<()> {
    const ISSUES: usize = 2_000;
    const RUNS: usize = 20_000;
    const PER_RUN: usize = 25;
    let fixture = Fixture::new()?;
    let path = fixture.file();
    let mut store = Store::initialize(&path)?;
    store.db.pragma_update(None, "synchronous", "FULL")?;
    let runs = {
        let tx = store.db.transaction()?;
        base(&tx)?;
        for i in 0..ISSUES {
            let parent = i.checked_sub(1).map(|i| format!("issue-{}", i / 5));
            issue(&tx, &format!("issue-{i}"), parent.as_deref())?;
        }
        let mut runs = Vec::new();
        for i in 0..RUNS {
            let task = format!("task-{i}");
            assignment(
                &tx,
                &task,
                &format!("issue-{}", i / (RUNS / ISSUES)),
                "runner",
            )?;
            let run = native_run(&format!("run-{i}"), &task);
            // Seed verified lifecycle facts without unrelated timeline/control work.
            tx.prepare_cached("INSERT INTO task_runs(id, assignment_id, task_id, observation) VALUES (?1, ?2, ?2, ?3)")?
                .execute(params![run.run_id, run.task_id, serde_json::to_string(&run)?])?;
            runs.push(run);
        }
        apply_batch(&tx, "runner", &runs, &[])?;
        tx.commit()?;
        runs
    };
    store.db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
    drop(store);
    let initial_bytes = fs::metadata(&path)?.len();
    for batch_rollups in [true, false] {
        let db_path = fixture.0.join(format!("mode-{batch_rollups}.sqlite"));
        fs::copy(&path, &db_path)?;
        let mut store = Store::open(&db_path)?;
        store.db.pragma_update(None, "synchronous", "FULL")?;
        let baseline_changes = store.db.total_changes();
        let mut wall = Vec::new();
        let mut body = Vec::new();
        let mut commits = Vec::new();
        let mut run_updates = 0;
        let mut buffer = Vec::with_capacity(RUNNER_SYNC_BATCH);
        let begin = Instant::now();
        for (index, run) in runs.iter().enumerate() {
            for j in 1..=PER_RUN {
                let mut usage = observation(run, (j * 50) as i64, Some(50));
                usage.response_id = format!("resp-{:053}", index * PER_RUN + j);
                usage.last.input_tokens = Some(45);
                usage.last.output_tokens = Some(5);
                usage.last.cache_write_input_tokens = Some(0);
                usage.last.reasoning_output_tokens = Some(2);
                buffer.push(usage);
                if buffer.len() != RUNNER_SYNC_BATCH && (index + 1 < RUNS || j < PER_RUN) {
                    continue;
                }
                let started = Instant::now();
                let tx = store
                    .db
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                let body_started = Instant::now();
                if batch_rollups {
                    apply_batch(&tx, "runner", &[], &buffer)?;
                    run_updates += 1 + buffer
                        .windows(2)
                        .filter(|pair| pair[0].run_id != pair[1].run_id)
                        .count();
                } else {
                    for usage in &buffer {
                        apply_batch(&tx, "runner", &[], std::slice::from_ref(usage))?;
                    }
                    run_updates += buffer.len();
                }
                let commit_started = Instant::now();
                tx.commit()?;
                body.push((commit_started - body_started).as_secs_f64() * 1000.0);
                commits.push(commit_started.elapsed().as_secs_f64() * 1000.0);
                wall.push(started.elapsed().as_secs_f64() * 1000.0);
                buffer.clear();
            }
        }
        let seconds = begin.elapsed().as_secs_f64();
        let raw = RUNS * PER_RUN;
        let writes = store.db.total_changes() - baseline_changes;
        let issue_writes = writes - raw as u64 - run_updates as u64;
        store.db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        let measured = json!({"batched_rollups": batch_rollups, "raw": raw, "runs": RUNS, "issues": ISSUES,
            "seconds": seconds, "us_per_record": seconds * 1e6 / raw as f64, "committed_db_bytes": fs::metadata(&db_path)?.len(),
            "db_growth_bytes": fs::metadata(&db_path)?.len() - initial_bytes, "baseline_db_bytes": initial_bytes,
            "sync_ms": sample_ms(&mut wall), "body_ms": sample_ms(&mut body), "commit_ms": sample_ms(&mut commits),
            "all_sqlite_row_writes_per_record": writes as f64 / raw as f64, "issue_rollup_row_writes": issue_writes,
            "issue_rollup_row_writes_per_record": issue_writes as f64 / raw as f64, "run_row_writes": run_updates});
        println!("{measured}");
        let result = view(&store, "issue-0")?;
        assert_eq!(result["subtree"]["reported_requests"], raw);
        assert_eq!(
            result["subtree"]["total_tokens"]["tokens"],
            (raw * 50).to_string()
        );
        if batch_rollups {
            let mut sql = store
                .db
                .prepare(&format!("EXPLAIN QUERY PLAN {ISSUE_USAGE_QUERY}"))?;
            let query_plan: Vec<String> = sql
                .query_map(["issue-0"], |row| row.get(3))?
                .collect::<rusqlite::Result<_>>()?;
            assert!(
                query_plan
                    .iter()
                    .any(|line| line.contains("SEARCH e USING PRIMARY KEY (ancestor=?)")),
                "{query_plan:?}"
            );
            assert!(
                query_plan.iter().all(|line| !line.contains("SCAN ")),
                "{query_plan:?}"
            );
            println!("query_plan={query_plan:?}");
            for id in ["issue-0", "issue-1999"] {
                let (mut read, mut serialized) = (Vec::new(), Vec::new());
                for _ in 0..100 {
                    let t = Instant::now();
                    let result = store.issue_token_usage(id)?.unwrap();
                    read.push(t.elapsed().as_secs_f64() * 1000.0);
                    let mut sink = Vec::new();
                    serde_json::to_writer(&mut sink, &result)?;
                    std::hint::black_box(sink);
                    serialized.push(t.elapsed().as_secs_f64() * 1000.0);
                }
                println!(
                    "{}",
                    json!({"id": id, "aggregate_sql_per_read": 1, "read_ms": sample_ms(&mut read), "read_and_json_ms": sample_ms(&mut serialized)})
                );
            }
            if let Some(output) = std::env::var_os("TEND_TOKEN_USAGE_BENCH_DB") {
                fs::copy(&db_path, output)?;
            }
        }
    }
    Ok(())
}

fn backfill_job(
    store: &mut Store,
    id: &str,
    scope: protocol::TokenUsageScope,
    policy: protocol::TokenConflictPolicy,
) -> Result<()> {
    let job = protocol::StartTokenBackfill {
        job_id: id.into(),
        scope,
        policy,
    };
    let result = store.start_token_backfill(&job)?.unwrap();
    assert_eq!(
        store.start_token_backfill(&job)?.unwrap()["selected_runs"],
        result["selected_runs"]
    );
    Ok(())
}

fn page(item: i64, offset: i64, counts: &[(i64, i64)], done: bool) -> protocol::TokenBackfillPage {
    protocol::TokenBackfillPage {
        item,
        offset,
        content: protocol::TokenBackfillContent::Records {
            snapshot: "0".repeat(64),
            done,
            prefix: false,
            child_gap: None,
            child_checked: true,
            records: counts
                .iter()
                .map(|(key, count)| protocol::NativeTokenRecord {
                    response_id: format!("resp-{key}"),
                    child: None,
                    counts: Some(TokenCounts {
                        total_tokens: Some(*count),
                        ..TokenCounts::default()
                    }),
                    invalid: false,
                    variant: None,
                    model: None,
                })
                .collect(),
        },
    }
}

fn deliver(store: &mut Store, reader: &str, page: &protocol::TokenBackfillPage) -> Result<()> {
    assert!(page.valid());
    let tx = store.db.transaction()?;
    backfill::receive(&tx, reader, page)?;
    tx.commit()?;
    Ok(())
}

#[test]
fn new_native_backfill_repairs_legacy_placeholders_but_keeps_real_conflicts() -> Result<()> {
    use protocol::{TokenBackfillContent, TokenConflictPolicy::Quarantine, TokenUsageScope::All};
    let fixture = Fixture::new()?;
    let mut store = Store::initialize(&fixture.file())?;
    base(&store.db)?;
    issue(&store.db, "root", None)?;
    assignment(&store.db, "job", "root", "runner")?;
    let run = native_run("run", "job");
    let thread = run.thread_id.as_deref().unwrap();
    let turn = run.turn_id.as_deref().unwrap();
    let counts = TokenCounts::from_fields([Some(863), Some(831), Some(180), None, Some(32), None]);
    let native = |id: i64, model: Value| {
        json!({"type":"token_usage_record","payload":{
            "thread_id":thread,"turn_id":turn,"response_id":format!("resp-{id}"),
            "usage":counts,"thread_token_usage":{"total_tokens":863*id},"response_model":model
        }})
    };
    let path = fixture.file().with_extension("jsonl");
    fs::write(
        &path,
        [
            json!({"type":"turn_context","payload":{"turn_id":turn,"model":"gpt-6.2-sol"}}),
            json!({"type":"event_msg","payload":{"type":"model_reroute","to_model":"unbound"}}),
            native(1, Value::Null),
            native(2, json!("invalid value")),
            native(3, json!("gpt-6.2-astra")),
            native(3, json!("gpt-6.2-sol")),
            // No current selection is available for the malformed copies.
            json!({"type":"turn_context","payload":{"turn_id":"unrelated","model":"unrelated"}}),
            native(4, json!("invalid value")),
            native(4, json!("gpt-6.2-astra")),
            native(5, json!("gpt-6.2-astra")),
            native(5, json!("invalid value")),
            native(6, json!("gpt-6.2-sol")),
            native(6, json!("openai/gpt-6.2-sol-20261117")),
        ]
        .iter()
        .map(|line| format!("{line}\n"))
        .collect::<String>(),
    )?;
    let legacy =
        codex_app::usage::native_snapshot_legacy(fs::File::open(&path)?, thread, turn, false)
            .unwrap();
    let corrected =
        codex_app::usage::native_snapshot(fs::File::open(&path)?, thread, turn, false).unwrap();
    let mut live = observation(&run, 1, None);
    live.last = counts;
    live.model = Some(TokenModel::Ambiguous);
    sync(&mut store, "runner", std::slice::from_ref(&run), &[live])?;
    for (job, native) in [
        ("legacy", &legacy),
        ("rechecked", &corrected),
        ("legacy-replay", &legacy),
    ] {
        backfill_job(&mut store, job, All, Quarantine)?;
        let work = backfill::pending(&store.db, "runner")?.unwrap();
        let mut reply = page(work.item, 0, &[], true);
        let TokenBackfillContent::Records { records, .. } = &mut reply.content else {
            unreachable!()
        };
        records.clone_from(native);
        deliver(&mut store, "runner", &reply)?;
        assert_eq!(
            store.token_backfill_status(job, 0)?.unwrap()["counts"]["scanned"],
            1
        );
        let direct = &view(&store, "root")?["direct"];
        assert_eq!(direct["reported_requests"], 6);
        if job == "legacy" {
            assert!(direct["estimate"]["usd"].is_null());
        } else {
            assert_eq!(direct["models"][0]["model"], "gpt-6.2-sol");
            assert_eq!(direct["models"][0]["selected_model_requests"], 2);
            assert_eq!(direct["models"][0]["response_model_requests"], 1);
            assert_eq!(direct["models"][1]["model"], "gpt-6.2-astra");
            assert_eq!(direct["models"][1]["response_model_requests"], 2);
            assert_eq!(direct["models"][2]["model"], Value::Null);
            assert_eq!(
                direct["estimate"],
                json!({"usd":"0.01804","partial":true,"unpriced_requests":1})
            );
        }
    }
    let before = view(&store, "root")?;
    store.rebuild_token_usage(&All)?;
    assert_eq!(view(&store, "root")?, before);
    Ok(())
}

#[test]
fn explicit_cache_miss_enriches_legacy_backfill_without_erasing_or_ignoring_conflicts() -> Result<()>
{
    use protocol::{TokenBackfillContent, TokenConflictPolicy::Quarantine, TokenUsageScope::All};
    let fixture = Fixture::new()?;
    let mut store = Store::initialize(&fixture.file())?;
    base(&store.db)?;
    issue(&store.db, "root", None)?;
    assignment(&store.db, "job", "root", "runner")?;
    let run = native_run("run", "job");
    sync(&mut store, "runner", &[run], &[])?;
    let direct = |s: &Store| Ok::<Value, anyhow::Error>(view(s, "root")?["direct"].clone());
    let mut after_explicit = Value::Null;
    for (job, miss) in [
        ("legacy", None),
        ("explicit", Some(15)),
        ("old-reader-again", None),
        ("real-conflict", Some(16)),
    ] {
        backfill_job(&mut store, job, All, Quarantine)?;
        let work = backfill::pending(&store.db, "runner")?.unwrap();
        let mut reply = page(work.item, 0, &[(1, 120)], true);
        let TokenBackfillContent::Records { records, .. } = &mut reply.content else {
            unreachable!()
        };
        records[0].model = Some(TokenModel::Response("deepseek-v4.1-flash".into()));
        records[0].counts = Some(TokenCounts {
            cache_miss_input_tokens: miss,
            ..TokenCounts::from_fields([Some(120), Some(100), Some(70), None, Some(20), None])
        });
        deliver(&mut store, "runner", &reply)?;
        let now = direct(&store)?;
        match job {
            "legacy" => assert_eq!(now["models"][0]["uncached_input_tokens"]["tokens"], "30"),
            "explicit" => {
                assert_eq!(now["models"][0]["uncached_input_tokens"]["tokens"], "15");
                after_explicit = now;
            }
            "old-reader-again" => assert_eq!(now, after_explicit),
            "real-conflict" => assert_eq!(now["estimate"]["usd"], Value::Null),
            _ => unreachable!(),
        }
    }
    Ok(())
}

#[test]
fn runner_replay_with_an_exact_catalog_alias_keeps_the_request_price() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut store = Store::initialize(&fixture.file())?;
    base(&store.db)?;
    issue(&store.db, "root", None)?;
    assignment(&store.db, "job", "root", "runner")?;
    let run = native_run("run", "job");
    let mut live = observation(&run, 1, None);
    live.last = TokenCounts::from_fields([Some(863), Some(831), Some(180), None, Some(32), None]);
    live.model = Some(TokenModel::RequestSelection("gpt-6.2-sol".into()));
    sync(&mut store, "runner", &[run], std::slice::from_ref(&live))?;
    let before = view(&store, "root")?;
    live.event_id = "alias-proof".into();
    live.model = Some(TokenModel::RequestSelection(
        "openai/gpt-6.2-sol-20261117".into(),
    ));
    sync(&mut store, "runner", &[], std::slice::from_ref(&live))?;
    assert_eq!(view(&store, "root")?, before);
    assert_eq!(before["direct"]["estimate"]["usd"], "0.00164");
    live.event_id = "different-model".into();
    live.model = Some(TokenModel::RequestSelection("gpt-6.2-astra".into()));
    sync(&mut store, "runner", &[], &[live])?;
    let conflict = view(&store, "root")?;
    assert_eq!(conflict["direct"]["reported_requests"], 1);
    assert_eq!(conflict["direct"]["models"][0]["model"], Value::Null);
    assert_eq!(conflict["direct"]["estimate"]["usd"], Value::Null);
    Ok(())
}

#[test]
fn native_recheck_does_not_hide_a_conflicting_live_actual_model() -> Result<()> {
    use protocol::{TokenBackfillContent, TokenConflictPolicy::Quarantine, TokenUsageScope::All};
    // The legacy placeholder, exact live response, and modern native scan
    // are independently delivered and may arrive in any order.
    for order in [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ] {
        let fixture = Fixture::new()?;
        let mut store = Store::initialize(&fixture.file())?;
        base(&store.db)?;
        issue(&store.db, "root", None)?;
        assignment(&store.db, "job", "root", "runner")?;
        let run = native_run("run", "job");
        let thread = run.thread_id.as_deref().unwrap();
        let turn = run.turn_id.as_deref().unwrap();
        let mut marker = observation(&run, 1, None);
        marker.last =
            TokenCounts::from_fields([Some(863), Some(831), Some(180), None, Some(32), None]);
        marker.model = Some(TokenModel::LegacyResponseConflict);
        let mut live = marker.clone();
        live.event_id = "explicit-live".into();
        live.model = Some(TokenModel::Response("gpt-6.2-sol".into()));
        let path = fixture.file().with_extension("jsonl");
        fs::write(&path, ["invalid model", "gpt-6.2-astra"].into_iter().map(|model| {
            format!("{}\n", json!({"type":"token_usage_record","payload":{
                "thread_id":thread,"turn_id":turn,"response_id":marker.response_id,
                "usage":marker.last,"thread_token_usage":{"total_tokens":863},"response_model":model
            }}))
        }).collect::<String>())?;
        let scanned =
            codex_app::usage::native_snapshot(fs::File::open(&path)?, thread, turn, false).unwrap();
        sync(&mut store, "runner", &[run], &[])?;
        for step in order {
            match step {
                0 | 1 => {
                    let evidence = if step == 0 { &marker } else { &live };
                    sync(&mut store, "runner", &[], std::slice::from_ref(evidence))?;
                }
                2 => {
                    backfill_job(&mut store, "modern", All, Quarantine)?;
                    let work = backfill::pending(&store.db, "runner")?.unwrap();
                    let mut reply = page(work.item, 0, &[], true);
                    let TokenBackfillContent::Records { records, .. } = &mut reply.content else {
                        unreachable!()
                    };
                    records.clone_from(&scanned);
                    deliver(&mut store, "runner", &reply)?;
                }
                _ => unreachable!(),
            }
        }
        let before = view(&store, "root")?;
        assert_eq!(before["direct"]["reported_requests"], 1, "{order:?}");
        assert_eq!(
            before["direct"]["models"][0]["model"],
            Value::Null,
            "{order:?}"
        );
        assert_eq!(
            before["direct"]["estimate"]["usd"],
            Value::Null,
            "{order:?}"
        );
        assert!(store.db.query_row(
            "SELECT json_extract(last,'$.model.observed_conflict') FROM token_requests",
            [],
            |r| r.get::<_, bool>(0)
        )?);
        live.event_id = "explicit-live-replay".into();
        sync(&mut store, "runner", &[], &[live])?;
        store.rebuild_token_usage(&All)?;
        assert_eq!(view(&store, "root")?, before);
        drop(store);
        assert_eq!(view(&Store::initialize(&fixture.file())?, "root")?, before);
    }
    Ok(())
}

#[test]
fn compatible_agent_repair_can_release_a_held_issue_model_without_adding_usage() {
    let actual = TokenModel::Response("gpt-6.2-sol".into());
    let issue = RequestMeter {
        counts: TokenCounts {
            total_tokens: Some(863),
            ..Default::default()
        },
        model: TokenModel::merge(
            Some(TokenModel::LegacyResponseConflict),
            Some(actual.clone()),
        ),
        provider_counts: false,
    };
    let agent = RequestMeter {
        counts: TokenCounts {
            input_tokens: Some(831),
            ..Default::default()
        },
        model: Some(TokenModel::Unresolved.rechecked(protocol::ModelRepair::native(false, true))),
        provider_counts: false,
    };
    let projected = model_with_agent(&issue, Some(&agent));
    assert_eq!(
        projected.model.as_ref().map(TokenModel::basis),
        Some(&actual)
    );
    assert_eq!(projected.counts, issue.counts);
}

#[test]
fn backfill_uses_original_run_and_retired_same_host_reader_after_a_lost_page_receipt() -> Result<()>
{
    use protocol::{TokenConflictPolicy::*, TokenUsageScope::*};
    let fixture = Fixture::new()?;
    let mut store = Store::initialize(&fixture.file())?;
    base(&store.db)?;
    issue(&store.db, "root", None)?;
    assignment(&store.db, "original", "root", "runner")?;
    let started = native_run("original-run", "original");
    sync(&mut store, "runner", std::slice::from_ref(&started), &[])?;
    store.db.execute(
        "INSERT INTO runners (id, host_id, profile, last_seen) VALUES ('next', 'local', '{}', unixepoch())", [],
    )?;
    store.db.execute(
        "INSERT INTO runner_replacements (predecessor, successor, drained_at)
         VALUES ('runner', 'next', unixepoch())",
        [],
    )?;
    backfill_job(&mut store, "job", Host("local".into()), Quarantine)?;
    let work = backfill::pending(&store.db, "runner")?.unwrap();
    let records = (10..42).map(|key| (key, 1)).collect::<Vec<_>>();
    let first = page(work.item, 0, &records, false);
    let tail = page(work.item, 32, &[(99, 2)], true);
    deliver(&mut store, "runner", &tail)?; // out of order
    deliver(&mut store, "other", &first)?; // another Host
    deliver(&mut store, "next", &first)?; // same Host, before retirement
    assert_eq!(view(&store, "root")?["direct"]["reported_requests"], 0);
    deliver(&mut store, "runner", &first)?;
    deliver(&mut store, "runner", &first)?;
    assert_eq!(view(&store, "root")?["direct"]["reported_requests"], 32);
    store.db.execute(
        "UPDATE runner_replacements SET retired_at = unixepoch()",
        [],
    )?;
    assert_eq!(backfill::pending(&store.db, "next")?.unwrap().offset, 32);
    assert!(backfill::pending(&store.db, "runner")?.is_none());
    deliver(&mut store, "runner", &tail)?;
    deliver(&mut store, "next", &first)?;
    deliver(&mut store, "next", &tail)?;
    assert!(backfill::pending(&store.db, "next")?.is_none());
    let rollup = view(&store, "root")?;
    assert_eq!(rollup["direct"]["total_tokens"]["tokens"], "34");
    assert_eq!(rollup["direct"]["reported_requests"], 33);
    assert_eq!(
        store.db.query_row(
            "SELECT count(*) FROM token_requests WHERE run_id = 'original-run'",
            [],
            |r| r.get::<_, i64>(0)
        )?,
        33
    );
    backfill_job(&mut store, "repeat", Run("original-run".into()), Quarantine)?;
    let work = backfill::pending(&store.db, "next")?.unwrap();
    deliver(&mut store, "next", &page(work.item, 0, &records, false))?;
    deliver(&mut store, "next", &page(work.item, 32, &[(99, 2)], true))?;
    assert_eq!(view(&store, "root")?, rollup);
    Ok(())
}

#[test]
fn backfill_conflict_choices_survive_live_interleaving_and_rebuild_from_requests() -> Result<()> {
    use protocol::{TokenConflictPolicy::*, TokenUsageScope::*};
    let fixture = Fixture::new()?;
    let mut store = Store::initialize(&fixture.file())?;
    base(&store.db)?;
    issue(&store.db, "root", None)?;
    issue(&store.db, "child", Some("root"))?;
    assignment(&store.db, "task", "child", "runner")?;
    let started = native_run("run", "task");
    let mut before = observation(&started, 1, Some(8));
    before.last.cached_input_tokens = None;
    let mut after = observation(&started, 2, Some(8));
    after.last.cached_input_tokens = None;
    let untouched = observation(&started, 9, Some(4));
    sync(
        &mut store,
        "runner",
        &[started],
        &[before.clone(), untouched],
    )?;
    let original = view(&store, "root")?;
    let scope = Task("task".into());
    backfill_job(&mut store, "quarantine", scope.clone(), Quarantine)?;
    let work = backfill::pending(&store.db, "runner")?.unwrap();
    deliver(
        &mut store,
        "runner",
        &page(work.item, 0, &[(1, 10), (2, 10), (3, 0)], true),
    )?;
    sync(&mut store, "runner", &[], std::slice::from_ref(&after))?;
    assert_eq!(
        view(&store, "root")?["subtree"]["total_tokens"]["tokens"],
        "4"
    );
    assert_eq!(view(&store, "root")?["subtree"]["reported_requests"], 2);
    let findings = store.token_backfill_audit("quarantine", 0)?.unwrap();
    let entries = findings["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 3);
    for (index, source) in [(0, "rollout"), (1, "server"), (2, "live")] {
        assert_eq!(entries[index]["source"], source);
        assert_eq!(
            entries[index]["evidence"]["server"]["total_tokens"],
            if index == 1 { 4 } else { 8 }
        );
    }

    for (job, policy, total) in [
        ("server", PreferServer, "20"),
        ("native", PreferNative, "24"),
        ("repeat", Quarantine, "24"),
    ] {
        backfill_job(&mut store, job, scope.clone(), policy)?;
        let work = backfill::pending(&store.db, "runner")?.unwrap();
        deliver(
            &mut store,
            "runner",
            &page(work.item, 0, &[(1, 10), (2, 10), (3, 0)], true),
        )?;
        sync(&mut store, "runner", &[], &[before.clone(), after.clone()])?;
        assert_eq!(
            view(&store, "root")?["subtree"]["total_tokens"]["tokens"],
            total
        );
    }
    assert_ne!(view(&store, "root")?, original);
    let expected = view(&store, "root")?;
    store.db.execute(
        "UPDATE token_run_tallies SET tally = ?1, valid_records = 0",
        [serde_json::to_string(&Tally::default())?],
    )?;
    store.db.execute("DELETE FROM token_issue_rollups", [])?;
    store
        .db
        .execute("DELETE FROM token_run_tallies WHERE run_id = 'run'", [])?;
    for scope in [Issue("root".into()), All] {
        store.rebuild_token_usage(&scope)?;
        assert_eq!(view(&store, "root")?, expected);
    }
    Ok(())
}

#[test]
fn backfill_gaps_and_ambiguous_native_keys_are_not_zero_or_destructive() -> Result<()> {
    use protocol::{
        TokenBackfillContent, TokenConflictPolicy::*, TokenSourceGap::*, TokenUsageScope::*,
    };
    let fixture = Fixture::new()?;
    let mut store = Store::initialize(&fixture.file())?;
    base(&store.db)?;
    issue(&store.db, "legacy", None)?;
    assignment(&store.db, "task", "legacy", "runner")?;
    let started = native_run("run", "task");
    sync(&mut store, "runner", std::slice::from_ref(&started), &[])?;
    store.db.execute("UPDATE token_runs SET legacy = 1", [])?;
    store.rebuild_token_usage(&All)?;
    let expected = view(&store, "legacy")?;
    assert_eq!(expected["direct"]["unobserved_before_collection"], 1);
    assert_eq!(expected["direct"]["total_tokens"]["tokens"], Value::Null);
    backfill_job(&mut store, "gap", All, PreferNative)?;
    assert_eq!(
        store.token_backfill_status("gap", 0)?.unwrap()["counts"]["reader_offline"],
        1
    );
    let work = backfill::pending(&store.db, "runner")?.unwrap();
    deliver(
        &mut store,
        "runner",
        &protocol::TokenBackfillPage {
            item: work.item,
            offset: 0,
            content: TokenBackfillContent::Gap {
                reason: SourceMissing,
            },
        },
    )?;
    assert_eq!(
        store.token_backfill_status("gap", 0)?.unwrap()["runs"][0]["gap"],
        "source_missing"
    );
    assert_eq!(view(&store, "legacy")?, expected);

    backfill_job(&mut store, "invalid", All, PreferNative)?;
    let work = backfill::pending(&store.db, "runner")?.unwrap();
    let mut invalid = page(work.item, 0, &[(1, 7), (2, 0)], true);
    let TokenBackfillContent::Records { records, .. } = &mut invalid.content else {
        unreachable!()
    };
    records[0].variant = Some(TokenCounts {
        total_tokens: Some(8),
        ..TokenCounts::default()
    });
    records[1].invalid = true;
    deliver(&mut store, "runner", &invalid)?;
    assert_eq!(view(&store, "legacy")?, expected);
    assert_eq!(
        store.token_backfill_audit("invalid", 0)?.unwrap()["entries"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let late = observation(&started, 1, Some(7));
    sync(&mut store, "runner", &[], &[late])?;
    assert_eq!(view(&store, "legacy")?, expected);
    Ok(())
}

#[test]
fn changed_native_evidence_requires_an_explicit_decision_even_without_a_server_copy() -> Result<()>
{
    use protocol::{TokenConflictPolicy::*, TokenUsageScope::*};
    let fixture = Fixture::new()?;
    let mut store = Store::initialize(&fixture.file())?;
    base(&store.db)?;
    issue(&store.db, "root", None)?;
    assignment(&store.db, "task", "root", "runner")?;
    sync(&mut store, "runner", &[native_run("run", "task")], &[])?;
    for (job, policy, value, expected) in [
        ("first", Quarantine, 5, json!("5")),
        ("changed", Quarantine, 8, Value::Null),
        ("retry", Quarantine, 8, Value::Null),
        ("confirmed", PreferNative, 8, json!("8")),
        ("after", Quarantine, 8, json!("8")),
    ] {
        backfill_job(&mut store, job, All, policy)?;
        let work = backfill::pending(&store.db, "runner")?.unwrap();
        deliver(
            &mut store,
            "runner",
            &page(work.item, 0, &[(1, value)], true),
        )?;
        assert_eq!(
            view(&store, "root")?["direct"]["total_tokens"]["tokens"],
            expected
        );
    }
    let audit = store.token_backfill_audit("changed", 0)?.unwrap();
    assert_eq!(
        audit["entries"][0]["evidence"]["prior_native"]["counts"]["total_tokens"],
        5
    );
    assert_eq!(audit["entries"][0]["evidence"]["native"]["total_tokens"], 8);
    Ok(())
}

#[test]
fn late_duplicate_started_retracts_only_backfill_before_during_and_after_scan() -> Result<()> {
    use protocol::{TokenConflictPolicy::*, TokenUsageScope::*};
    for when in ["same_sync", "between_pages", "after_completion"] {
        let fixture = Fixture::new()?;
        let mut store = Store::initialize(&fixture.file())?;
        base(&store.db)?;
        issue(&store.db, "a", None)?;
        issue(&store.db, "b", Some("a"))?;
        assignment(&store.db, "task-a", "a", "runner")?;
        assignment(&store.db, "task-b", "b", "runner")?;
        let a = native_run("a", "task-a");
        let mut b = native_run("b", "task-b");
        b.thread_id.clone_from(&a.thread_id);
        b.turn_id.clone_from(&a.turn_id);
        let mut server = observation(&a, 10, Some(2));
        server.last.cached_input_tokens = None;
        sync(
            &mut store,
            "runner",
            std::slice::from_ref(&a),
            std::slice::from_ref(&server),
        )?;
        backfill_job(&mut store, "scan", Run("a".into()), PreferNative)?;
        let work = backfill::pending(&store.db, "runner")?.unwrap();
        let batch = (10..42).map(|key| (key, 8)).collect::<Vec<_>>();
        if when == "same_sync" {
            let tx = store.db.transaction()?;
            assert!(crate::store::run::apply(&tx, "runner", &b)?);
            apply_batch(&tx, "runner", std::slice::from_ref(&b), &[])?;
            backfill::receive(&tx, "runner", &page(work.item, 0, &batch, true))?;
            tx.commit()?;
        } else {
            deliver(
                &mut store,
                "runner",
                &page(work.item, 0, &batch, when == "after_completion"),
            )?;
            assert_eq!(view(&store, "a")?["direct"]["reported_requests"], 32);
            sync(&mut store, "runner", &[b], &[])?;
            deliver(&mut store, "runner", &page(work.item, 32, &[(99, 8)], true))?;
            let audit = store.token_backfill_audit("scan", 0)?.unwrap();
            assert!(audit["entries"].as_array().unwrap().iter().any(|row| {
                row["source"] == "assignment" && row["evidence"]["decision"] == "ambiguous_turn"
            }));
        }
        assert_eq!(
            store.token_backfill_status("scan", 0)?.unwrap()["runs"][0]["gap"],
            "ambiguous_turn"
        );
        let expected = view(&store, "a")?;
        assert_eq!(expected["subtree"]["started_runs"], 2);
        assert_eq!(expected["subtree"]["reported_requests"], 1);
        assert_eq!(expected["subtree"]["total_tokens"]["tokens"], "2");
        assert_eq!(
            expected["subtree"]["cached_input_tokens"]["tokens"],
            Value::Null
        );
        sync(&mut store, "runner", &[], &[server])?;
        backfill_job(&mut store, "repeat", All, PreferNative)?;
        assert!(backfill::pending(&store.db, "runner")?.is_none());
        store.rebuild_token_usage(&All)?;
        assert_eq!(view(&store, "a")?, expected);
    }
    Ok(())
}

#[test]
fn complementary_live_fields_merge_only_when_every_known_value_agrees() -> Result<()> {
    use protocol::{TokenBackfillContent, TokenConflictPolicy::*, TokenUsageScope::*};
    for before in [true, false] {
        let fixture = Fixture::new()?;
        let mut store = Store::initialize(&fixture.file())?;
        base(&store.db)?;
        issue(&store.db, "root", None)?;
        assignment(&store.db, "task", "root", "runner")?;
        let run = native_run("run", "task");
        sync(&mut store, "runner", std::slice::from_ref(&run), &[])?;
        let mut total = observation(&run, 1, Some(10));
        total.last.cached_input_tokens = None;
        let mut input = total.clone();
        input.last.input_tokens = Some(8);
        let mut output = total.clone();
        output.last.total_tokens = None;
        output.last.output_tokens = Some(2);
        let copies = [total, input.clone(), output];
        if before {
            sync(&mut store, "runner", &[], &copies)?;
            assert_eq!(
                view(&store, "root")?["direct"]["input_tokens"]["tokens"],
                "8"
            );
        }
        backfill_job(&mut store, "scan", All, Quarantine)?;
        let work = backfill::pending(&store.db, "runner")?.unwrap();
        let mut scan = page(work.item, 0, &[(1, 10)], true);
        let TokenBackfillContent::Records { records, .. } = &mut scan.content else {
            unreachable!()
        };
        let counts = records[0].counts.as_mut().unwrap();
        counts.input_tokens = Some(8);
        counts.output_tokens = Some(2);
        counts.cached_input_tokens = Some(0);
        deliver(&mut store, "runner", &scan)?;
        if !before {
            sync(&mut store, "runner", &[], &copies)?;
        }
        let view_after = view(&store, "root")?;
        assert_eq!(view_after["direct"]["reported_requests"], 1);
        assert_eq!(view_after["direct"]["total_tokens"]["tokens"], "10");
        assert_eq!(
            view_after["direct"]["cached_input_tokens"]["tokens"],
            Value::Null
        );
        assert_eq!(view_after["direct"]["output_tokens"]["tokens"], "2");
        input.last.input_tokens = Some(9);
        sync(&mut store, "runner", &[], &[input])?;
        assert_eq!(view(&store, "root")?["direct"]["reported_requests"], 0);
    }
    Ok(())
}

#[test]
fn revoked_rollout_only_keys_do_not_choose_the_first_verified_live_owner() -> Result<()> {
    use protocol::{TokenConflictPolicy::*, TokenUsageScope::*};
    for historic in [false, true] {
        for same_batch in [false, true] {
            for b_first in [false, true] {
                let fixture = Fixture::new()?;
                let mut store = Store::initialize(&fixture.file())?;
                base(&store.db)?;
                issue(&store.db, "left", None)?;
                issue(&store.db, "right", Some("left"))?;
                assignment(&store.db, "task-a", "left", "runner")?;
                assignment(&store.db, "task-b", "right", "runner")?;
                let a = native_run("a", "task-a");
                let mut b = native_run("b", "task-b");
                b.thread_id.clone_from(&a.thread_id);
                b.turn_id.clone_from(&a.turn_id);
                let from_a = observation(&a, 10, Some(6));
                let from_b = observation(&b, 10, Some(6));
                sync(&mut store, "runner", &[a], &[])?;
                if historic {
                    backfill_job(&mut store, "scan", All, PreferNative)?;
                    let work = backfill::pending(&store.db, "runner")?.unwrap();
                    deliver(&mut store, "runner", &page(work.item, 0, &[(10, 25)], true))?;
                }
                let (first, second) = if b_first {
                    (from_b, from_a)
                } else {
                    (from_a, from_b)
                };
                if same_batch {
                    sync(&mut store, "runner", &[b], std::slice::from_ref(&first))?;
                } else {
                    sync(&mut store, "runner", &[b], &[])?;
                    assert_eq!(view(&store, "left")?["subtree"]["reported_requests"], 0);
                    sync(&mut store, "runner", &[], std::slice::from_ref(&first))?;
                }
                sync(&mut store, "runner", &[], &[second, first])?;
                let expected = view(&store, "left")?;
                assert_eq!(expected["subtree"]["reported_requests"], 1);
                assert_eq!(expected["subtree"]["total_tokens"]["tokens"], "6");
                assert_eq!(expected["direct"]["reported_requests"], i64::from(!b_first));
                let owner: String =
                    store
                        .db
                        .query_row("SELECT run_id FROM token_requests", [], |row| row.get(0))?;
                assert_eq!(owner, if b_first { "b" } else { "a" });
                if historic {
                    let status = store.token_backfill_status("scan", 0)?.unwrap();
                    assert_eq!(status["runs"][0]["gap"], "ambiguous_turn");
                    assert_eq!(
                        store.token_backfill_audit("scan", 0)?.unwrap()["entries"][0]["evidence"]["native"]
                            ["total_tokens"],
                        25
                    );
                }
                store.rebuild_token_usage(&All)?;
                assert_eq!(view(&store, "left")?, expected);
            }
        }
    }
    Ok(())
}

#[test]
fn legacy_root_page_is_partial_and_child_audit_uses_native_keys() -> Result<()> {
    use protocol::{
        NativeTurnKey, TokenBackfillContent::Records, TokenConflictPolicy::Quarantine,
        TokenUsageScope::All,
    };
    let fixture = Fixture::new()?;
    let mut store = Store::initialize(&fixture.file())?;
    base(&store.db)?;
    issue(&store.db, "issue", None)?;
    assignment(&store.db, "task", "issue", "runner")?;
    let run = native_run("run", "task");
    let root = NativeTurnKey {
        thread_id: run.thread_id.clone().unwrap(),
        turn_id: run.turn_id.clone().unwrap(),
    };
    sync(&mut store, "runner", std::slice::from_ref(&run), &[])?;
    backfill_job(&mut store, "old-reader", All, Quarantine)?;
    let old = backfill::pending(&store.db, "runner")?.unwrap();
    assert!(old.include_children);
    let mut wire = to_value(page(old.item, 0, &[(0, 7)], true))?;
    wire.as_object_mut().unwrap().remove("child_checked");
    deliver(&mut store, "runner", &serde_json::from_value(wire)?)?;
    let status = store.token_backfill_status("old-reader", 0)?.unwrap();
    assert_eq!(status["state"], "finished_with_findings");
    assert_eq!(status["runs"][0]["gap"], "child_reader_unsupported");
    assert_eq!(status["runs"][0]["native_keys"], 1);
    assert_eq!(
        view(&store, "issue")?["direct"]["total_tokens"]["tokens"],
        "7"
    );

    backfill_job(&mut store, "current-reader", All, Quarantine)?;
    let current = backfill::pending(&store.db, "runner")?.unwrap();
    let mut scan = page(current.item, 0, &[(1, 10), (2, 10)], true);
    let Records { records, .. } = &mut scan.content else {
        unreachable!()
    };
    for (id, row) in records.iter_mut().enumerate() {
        row.response_id = "shared-response".into();
        row.child = Some(NativeTurnKey {
            thread_id: "child-thread".into(),
            turn_id: format!("child-turn-{id}"),
        });
    }
    deliver(&mut store, "runner", &scan)?;
    for n in 0..2 {
        let mut usage = observation(&run, n, Some(20));
        usage.last.cached_input_tokens = None;
        usage.root = Some(root.clone());
        usage.thread_id = "child-thread".into();
        usage.turn_id = format!("child-turn-{n}");
        usage.response_id = "shared-response".into();
        sync(&mut store, "runner", &[], &[usage])?;
    }
    let audit = store.token_backfill_audit("current-reader", 0)?.unwrap();
    let live: Vec<_> = audit["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["source"] == "live")
        .collect();
    assert_eq!(live.len(), 2, "{audit}");
    for (id, row) in live.into_iter().enumerate() {
        assert_eq!(row["thread_id"], "child-thread");
        assert_eq!(row["turn_id"], format!("child-turn-{id}"));
        assert_eq!(row["response_id"], "shared-response");
        assert_eq!(row["root_thread_id"], root.thread_id);
        assert_eq!(row["root_turn_id"], root.turn_id);
    }
    Ok(())
}

#[test]
fn legacy_optional_zero_prices_stay_partial_through_migration_rebuild_and_replay() -> Result<()> {
    use protocol::TokenUsageScope::All;
    let fixture = Fixture::new()?;
    let path = fixture.file();
    let mut store = Store::initialize(&path)?;
    base(&store.db)?;
    issue(&store.db, "root", None)?;
    assignment(&store.db, "job", "root", "runner")?;
    let run = native_run("run", "job");
    let mut record = observation(&run, 0, Some(1050));
    record.last = TokenCounts::from_fields([1050, 1000, 0, 0, 50, 0].map(Some));
    record.model = Some(TokenModel::Response("gpt-6.2-sol".into()));
    record.provider_counts = true;
    sync(&mut store, "runner", &[run], std::slice::from_ref(&record))?;

    // Recreate an old writer's ledger and raw coverage (same values, without
    // provider provenance). Neither migration nor rebuild may edit its bytes.
    store.db.execute_batch(
        "UPDATE token_requests SET last=json_remove(last,'$.provider_counts');
         PRAGMA user_version=51;",
    )?;
    let raw = |s: &Store| -> Result<String> {
        Ok(s.db.query_row("SELECT last FROM token_requests", [], |r| r.get(0))?)
    };
    let original = raw(&store)?;
    drop(store);
    let mut store = Store::initialize(&path)?;
    let verify = |s: &Store, raw_read: Value| -> Result<()> {
        let direct = view(s, "root")?["direct"].clone();
        let overview = to_value(s.token_overview(None, None)?)?;
        assert_eq!(overview["total"], direct);
        assert_eq!(overview["sources"][0]["usage"], direct);
        assert_eq!(
            direct["estimate"],
            json!({"usd":"0.0025","partial":true,"unpriced_requests":0})
        );
        assert_eq!(direct["cached_input_tokens"]["tokens"], raw_read);
        let tier = &direct["models"][0]["price"]["tiers"][0];
        assert_eq!(tier["components"][1]["priced_requests"], 0);
        assert_eq!(tier["components"][2]["priced_requests"], 0);
        assert_eq!(tier["baseline_input"]["tokens"], "1000");
        assert_eq!(
            tier["notes"],
            json!({"read_discount_unobserved":1, "write_adjustment_unobserved":1})
        );
        Ok(())
    };
    verify(&store, json!("0"))?;
    store.rebuild_token_usage(&All)?;
    verify(&store, json!("0"))?;
    assert_eq!(raw(&store)?, original);

    record.provider_counts = false;
    record.event_id = "legacy-replay".into();
    sync(&mut store, "runner", &[], std::slice::from_ref(&record))?;
    verify(&store, Value::Null)?;

    record.provider_counts = true;
    record.event_id = "exact-replay".into();
    sync(&mut store, "runner", &[], &[record])?;
    let direct = view(&store, "root")?["direct"].clone();
    assert_eq!(
        direct["estimate"],
        json!({"usd":"0.0025","partial":false,"unpriced_requests":0})
    );
    assert_eq!(
        direct["models"][0]["price"]["tiers"][0]["components"][1]["priced_requests"],
        1
    );
    store.rebuild_token_usage(&All)?;
    assert_eq!(view(&store, "root")?["direct"], direct);
    Ok(())
}

#[test]
fn verified_ingress_normalizes_legacy_defaults_before_and_after_backfill() -> Result<()> {
    use protocol::{TokenConflictPolicy::Quarantine, TokenUsageScope::All};
    for scan_first in [false, true] {
        let fixture = Fixture::new()?;
        let mut store = Store::initialize(&fixture.file())?;
        base(&store.db)?;
        issue(&store.db, "issue", None)?;
        assignment(&store.db, "task", "issue", "runner")?;
        let run = native_run("run", "task");
        sync(&mut store, "runner", std::slice::from_ref(&run), &[])?;

        let mut legacy_wire = observation(&run, 0, Some(7));
        legacy_wire.last.cache_write_input_tokens = Some(0);
        legacy_wire.last.reasoning_output_tokens = Some(0);
        let check = |store: &Store, expected: Value| -> Result<()> {
            let direct = &view(store, "issue")?["direct"];
            assert_eq!(direct["reported_requests"], 1);
            assert_eq!(direct["total_tokens"]["tokens"], "7");
            assert_eq!(direct["cache_write_input_tokens"]["tokens"], expected);
            assert_eq!(direct["reasoning_output_tokens"]["tokens"], expected);
            Ok(())
        };

        if !scan_first {
            sync(
                &mut store,
                "runner",
                &[],
                std::slice::from_ref(&legacy_wire),
            )?;
            check(&store, Value::Null)?;
        }
        backfill_job(&mut store, "scan", All, Quarantine)?;
        let work = backfill::pending(&store.db, "runner")?.unwrap();
        deliver(&mut store, "runner", &page(work.item, 0, &[(0, 7)], true))?;
        let mut exact = legacy_wire.clone();
        exact.event_id.push_str("-raw");
        exact.provider_counts = true;
        sync(
            &mut store,
            "runner",
            &[],
            std::slice::from_ref(&legacy_wire),
        )?;
        check(&store, Value::Null)?;
        sync(&mut store, "runner", &[], &[exact, legacy_wire])?;
        check(&store, json!("0"))?;
        store.rebuild_token_usage(&All)?;
        check(&store, json!("0"))?;
    }
    Ok(())
}
