use super::*;
use protocol::{RUNNER_SYNC_BATCH, RunOutcome};
use serde_json::{Value, json, to_value};
use std::{fs, path::PathBuf, time::Instant};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Result<Self> {
        let dir = std::env::temp_dir().join(format!("tend-usage-{}", protocol::new_id()));
        fs::create_dir(&dir)?;
        Ok(Self(dir))
    }
    fn file(&self) -> PathBuf {
        self.0.join("fixture.sqlite")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn base(db: &Connection) -> Result<()> {
    db.execute_batch(
        "INSERT INTO chat_messages (seq, id, chat_id, text) VALUES (1, 'source', 'fixture', 'Synthetic');
         INSERT INTO runners (id, host_id, profile, last_seen) VALUES ('runner', 'local', '{}', 0), ('other', 'other', '{}', 0);",
    )?;
    Ok(())
}

fn issue(db: &Connection, id: &str, parent: Option<&str>) -> Result<()> {
    db.prepare_cached("INSERT INTO issues(id, source_seq, title, objective, parent_issue_id) VALUES(?1, 1, ?1, '', ?2)")?
        .execute(params![id, parent])?;
    Ok(())
}

fn assignment(db: &Connection, id: &str, issue: &str, runner: &str) -> Result<()> {
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

fn native_run(run: &str, task: &str) -> RunObservation {
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

fn observation(run: &RunObservation, cursor: i64, total: Option<i64>) -> RunTokenUsage {
    RunTokenUsage {
        event_id: format!("{}-request-{cursor}", run.run_id),
        assignment_id: run.assignment_id.clone(),
        task_id: run.task_id.clone(),
        run_id: run.run_id.clone(),
        thread_id: run.thread_id.clone().unwrap(),
        turn_id: run.turn_id.clone().unwrap(),
        response_id: format!("resp-{cursor}"),
        last: TokenCounts {
            total_tokens: total,
            cached_input_tokens: Some(0),
            ..TokenCounts::default()
        },
        observed_at: 42,
    }
}

fn sync(
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

fn view(store: &Store, id: &str) -> Result<Value> {
    Ok(to_value(store.issue_token_usage(id)?.unwrap())?)
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
    let parent_usage = observation(&parent, 50, Some(20));
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
    crate::store::remove_token_usage_schema(&store.db)?;
    crate::store::codebase_comments::drop_schema(&store.db)?;
    store.db.pragma_update(None, "user_version", 37)?;
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
