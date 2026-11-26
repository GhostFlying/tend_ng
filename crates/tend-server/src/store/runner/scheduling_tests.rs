use super::*;
use codex_app::ToolCall;
use protocol::{AgentPin, AgentProfile, DispatchIssue, IssueDetail};
use serde_json::json;
use std::path::{Path, PathBuf};

fn fixture(path: &Path) -> Result<Store> {
    let store = Store::initialize(path)?;
    store.db.execute_batch(
        "INSERT INTO chat_messages (seq, id, chat_id, text) VALUES (1, 'source', 'main', 'Request');
         INSERT INTO router_inputs (seq, message_seq, chat_id) VALUES (1, 1, 'main');
         INSERT INTO router_attempts (seq, number, id, thread_id, turn_id)
           VALUES (1, 1, 'attempt', 'thread', 'turn');",
    )?;
    Ok(store)
}

fn runner(id: &str, limit: u32, available: u32) -> RunnerProfile {
    RunnerProfile {
        runner_id: id.into(),
        host_id: "host".into(),
        repos: vec!["repo".into()],
        agents: [
            ("owner", "issue-router", vec![]),
            ("worker", "implementation", vec!["image_gen".into()]),
            ("reviewer", "review", vec![]),
        ]
        .into_iter()
        .map(|(id, category, capabilities)| AgentProfile {
            id: id.into(),
            category: category.into(),
            revision: "1".into(),
            model: None,
            reasoning_effort: None,
            app_server: None,
            capabilities,
            description: None,
        })
        .collect(),
        slot_limit: limit,
        // The Runner can have used admission capacity without any running Run.
        running_slots: 0,
        available_for_assignment: available,
        inspect_url: None,
        workspace_root: None,
        standby: false,
        shared_reviews: true,
        handoff_slots: false,
    }
}

fn announce(store: &mut Store, profile: RunnerProfile, protocol: u32) -> Result<()> {
    assert!(profile.valid());
    store
        .sync_runner(
            RunnerSync {
                repair_host_artifacts: false,
                protocol,
                profile,
                drained: false,
                drain_active_slots: None,
                reclaim_receipts: vec![],
                receipts: vec![],
                runs: vec![],
                records: vec![],
                token_usage: vec![],
            },
            None,
        )?
        .expect("fixture heartbeat");
    Ok(())
}

fn work() -> DispatchIssue {
    DispatchIssue {
        title: "Test issue".into(),
        objective: "Test objective".into(),
        repo_ids: vec![],
        required_capabilities: vec![],
        base_refs: Default::default(),
        agent: None,
    }
}

fn dispatch(
    store: &mut Store,
    call: &str,
    work: &DispatchIssue,
    record_only: bool,
) -> Result<IssueDetail> {
    Ok(store
        .dispatch_issue(
            "attempt",
            &ToolCall {
                thread_id: "thread".into(),
                turn_id: "turn".into(),
                call_id: call.into(),
                tool: "tend_dispatch_issue".into(),
                namespace: None,
                arguments: json!(work),
            },
            work,
            record_only,
            None,
        )?
        .expect("fixture router dispatch"))
}

fn top_level(store: &mut Store, call: &str, work: &DispatchIssue) -> Result<Box<IssueDispatch>> {
    let IssueDetail::Dispatched(issue) = dispatch(store, call, work, false)? else {
        panic!("expected a dispatched Issue");
    };
    Ok(issue)
}

fn placement(store: &Store, task: &str) -> Result<Option<String>> {
    Ok(store.db.query_row(
        "SELECT a.runner_id FROM tasks t LEFT JOIN runner_assignments a ON a.id = t.current_assignment
         WHERE t.id = ?1",
        [task],
        |row| row.get(0),
    )?)
}

fn finish(store: &Store, task: &str) -> Result<()> {
    store.db.execute(
        "UPDATE runner_assignments SET state = 'accepted'
         WHERE id = (SELECT current_assignment FROM tasks WHERE id = ?1)",
        [task],
    )?;
    store.db.execute(
        "UPDATE tasks SET status = 'completed' WHERE id = ?1",
        [task],
    )?;
    Ok(())
}

#[test]
fn initial_root_uses_occupied_fraction_not_occupied_count() -> Result<()> {
    let mut store = fixture(Path::new(":memory:"))?;
    announce(&mut store, runner("a-small", 6, 3), 2)?;
    announce(&mut store, runner("z-large", 12, 9), 2)?;

    let IssueDetail::Recorded(record) = dispatch(&mut store, "record", &work(), true)? else {
        panic!("expected a recorded Issue");
    };
    let Some(IssueDetail::Dispatched(issue)) =
        store.start_recorded_issue(&record.issue_id, None)?
    else {
        panic!("expected to start a recorded Issue");
    };
    assert_eq!(
        placement(&store, &issue.task_id)?.as_deref(),
        Some("z-large")
    );
    Ok(())
}

#[test]
fn equal_load_rotates_across_reopen_and_ignores_pinned_assignments() -> Result<()> {
    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let dir = TempDir(std::env::temp_dir().join(format!("tend-rotation-{}", protocol::new_id())));
    std::fs::create_dir(&dir.0)?;
    let db = dir.0.join("fixture.db");
    let mut store = fixture(&db)?;
    announce(&mut store, runner("a", 6, 6), 2)?;
    announce(&mut store, runner("b", 6, 6), 2)?;
    // Reconstruct a v34 fixture with intact runner registrations.
    crate::store::remove_token_usage_schema(&store.db)?;
    crate::store::pull::restore_legacy_schema(&store.db)?;
    store.db.execute_batch(
        "DROP TABLE runner_auto_assignment_order;
         DROP VIEW runner_handoff_limits;
         ALTER TABLE runner_replacements DROP COLUMN occupied_slots;
         ALTER TABLE runner_replacements DROP COLUMN slot_budget;
         PRAGMA user_version = 34;",
    )?;
    drop(store);
    let mut store = Store::initialize(&db)?;
    for (call, expected) in [
        ("first", "a"),
        ("second", "b"),
        ("third", "a"),
        ("fourth", "b"),
    ] {
        let issue = top_level(&mut store, call, &work())?;
        assert_eq!(
            placement(&store, &issue.task_id)?.as_deref(),
            Some(expected)
        );
        finish(&store, &issue.task_id)?;
        if call == "first" {
            let mut pinned = work();
            pinned.agent = Some(AgentPin {
                id: "owner".into(),
                runner_id: Some("b".into()),
            });
            let pinned = top_level(&mut store, "pinned", &pinned)?;
            assert_eq!(placement(&store, &pinned.task_id)?.as_deref(), Some("b"));
            finish(&store, &pinned.task_id)?;
            drop(store);
            store = Store::initialize(&db)?;
            // Refresh both profiles through the upsert path, retaining equal load.
            announce(&mut store, runner("a", 6, 5), 2)?;
            announce(&mut store, runner("b", 6, 5), 2)?;
        }
    }
    Ok(())
}

#[test]
fn a_batch_observes_earlier_reservations_and_issued_work_still_counts() -> Result<()> {
    let mut store = fixture(Path::new(":memory:"))?;
    let first = top_level(&mut store, "first", &work())?;
    let second = top_level(&mut store, "second", &work())?;
    assert_eq!(first.status, "awaiting_assignment");
    assert_eq!(second.status, "awaiting_assignment");

    // Initially 2/6 < 5/12. After the first reservation, 3/6 > 5/12.
    announce(&mut store, runner("a", 6, 4), 2)?;
    announce(&mut store, runner("b", 12, 7), 2)?;
    store.schedule_assignments(&mut 0)?;
    assert_eq!(placement(&store, &first.task_id)?.as_deref(), Some("a"));
    assert_eq!(placement(&store, &second.task_id)?.as_deref(), Some("b"));

    announce(&mut store, runner("a", 6, 4), 2)?;
    let state: String = store.db.query_row(
        "SELECT state FROM runner_assignments WHERE task_id = ?1",
        [&first.task_id],
        |row| row.get(0),
    )?;
    assert_eq!(state, "issued");
    finish(&store, &second.task_id)?;
    let third = top_level(&mut store, "third", &work())?;
    assert_eq!(placement(&store, &third.task_id)?.as_deref(), Some("b"));
    Ok(())
}

#[test]
fn a_cursor_write_failure_rolls_back_the_reservation() -> Result<()> {
    let mut store = fixture(Path::new(":memory:"))?;
    let waiting = top_level(&mut store, "waiting", &work())?;
    announce(&mut store, runner("a", 6, 6), 2)?;
    store.db.execute_batch(
        "CREATE TRIGGER fail_auto_order BEFORE INSERT ON runner_auto_assignment_order
         BEGIN SELECT RAISE(ABORT, 'fixture cursor write failure'); END;",
    )?;

    let error = store.schedule_assignments(&mut 0).unwrap_err();
    assert!(error.to_string().contains("fixture cursor write failure"));
    let (status, reservations): (String, i64) = store.db.query_row(
        "SELECT status, (SELECT count(*) FROM runner_assignments WHERE task_id = t.id)
         FROM tasks t WHERE id = ?1",
        [&waiting.task_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(status, "awaiting_assignment");
    assert_eq!(reservations, 0);
    assert_eq!(placement(&store, &waiting.task_id)?, None);

    store.db.execute_batch("DROP TRIGGER fail_auto_order;")?;
    store.schedule_assignments(&mut 0)?;
    assert_eq!(placement(&store, &waiting.task_id)?.as_deref(), Some("a"));
    Ok(())
}

#[test]
fn full_runners_wait_without_a_reservation_then_only_available_runner_wins() -> Result<()> {
    let mut store = fixture(Path::new(":memory:"))?;
    announce(&mut store, runner("a", 6, 1), 2)?;
    announce(&mut store, runner("b", 12, 0), 2)?;
    let first = top_level(&mut store, "fill-a", &work())?;
    assert_eq!(placement(&store, &first.task_id)?.as_deref(), Some("a"));
    let waiting = top_level(&mut store, "waiting", &work())?;
    assert_eq!(waiting.status, "awaiting_assignment");
    store.schedule_assignments(&mut 0)?;
    let count: i64 = store.db.query_row(
        "SELECT COUNT(*) FROM runner_assignments WHERE task_id = ?1",
        [&waiting.task_id],
        |row| row.get(0),
    )?;
    assert_eq!(count, 0);

    announce(&mut store, runner("b", 12, 1), 2)?;
    store.schedule_assignments(&mut 0)?;
    assert_eq!(placement(&store, &waiting.task_id)?.as_deref(), Some("b"));
    Ok(())
}

#[test]
fn low_load_does_not_bypass_matching_or_generation_eligibility() -> Result<()> {
    let mut store = fixture(Path::new(":memory:"))?;
    let mut no_repo = runner("a-no-repo", 6, 6);
    no_repo.repos.clear();
    announce(&mut store, no_repo, 2)?;
    let mut no_work_cap = runner("b-no-work-cap", 6, 6);
    no_work_cap.agents[0].capabilities = vec!["image_gen".into()];
    no_work_cap.agents[1].capabilities.clear();
    announce(&mut store, no_work_cap, 2)?;
    let mut no_named_agent = runner("c-no-named", 6, 6);
    no_named_agent.agents[0].id = "other-owner".into();
    announce(&mut store, no_named_agent, 2)?;
    announce(&mut store, runner("d-predecessor", 6, 6), 2)?;
    for id in ["e-standby", "z-successor"] {
        let mut standby = runner(id, 6, if id == "e-standby" { 6 } else { 1 });
        standby.standby = true;
        announce(&mut store, standby, 2)?;
    }
    store.db.execute(
        "INSERT INTO runner_replacements (predecessor, successor) VALUES ('d-predecessor', 'z-successor')",
        [],
    )?;

    let mut required = work();
    required.repo_ids = vec!["repo".into()];
    required.required_capabilities = vec!["image_gen".into()];
    required.agent = Some(AgentPin {
        id: "owner".into(),
        runner_id: None,
    });
    let issue = top_level(&mut store, "requirements", &required)?;
    // This legacy successor does not enforce a dynamic slot limit.
    assert_eq!(placement(&store, &issue.task_id)?, None);
    super::super::handoff::note_drained(&store.db, "d-predecessor")?;
    store.schedule_assignments(&mut 0)?;
    assert_eq!(
        placement(&store, &issue.task_id)?.as_deref(),
        Some("z-successor")
    );
    Ok(())
}

fn child(store: &Store, parent: &str, id: &str, category: &str, pin: &str) -> Result<()> {
    store.db.execute(
        "INSERT INTO issues (id, source_seq, title, objective, parent_issue_id)
         VALUES (?1, 1, ?1, 'Child objective', ?2)",
        params![id, parent],
    )?;
    store.db.execute(
        "INSERT INTO tasks (id, issue_id, category, status, repo_ids, required_capabilities, pinned_runner_id)
         VALUES (?1, ?1, ?2, 'awaiting_assignment', '[]', '[]', ?3)",
        params![id, category, pin],
    )?;
    store.db.execute(
        "INSERT INTO runner_operations (id, runner_id, operation) VALUES (?1, ?2, ?3)",
        params![id, pin, json!({"action": {"work": work()}}).to_string()],
    )?;
    store
        .db
        .execute("INSERT INTO child_dispatches VALUES (?1, ?1)", [id])?;
    Ok(())
}

#[test]
fn explicit_pin_children_and_followup_keep_their_runner() -> Result<()> {
    let mut store = fixture(Path::new(":memory:"))?;
    announce(&mut store, runner("a", 6, 6), 2)?;
    announce(&mut store, runner("z", 6, 3), 2)?;
    let mut pinned = work();
    pinned.agent = Some(AgentPin {
        id: "owner".into(),
        runner_id: Some("z".into()),
    });
    let root = top_level(&mut store, "root", &pinned)?;
    assert_eq!(placement(&store, &root.task_id)?.as_deref(), Some("z"));
    finish(&store, &root.task_id)?;
    store.db.execute(
        "INSERT INTO handoff_roots VALUES (?1, 'z')",
        [&root.issue_id],
    )?;
    child(&store, &root.issue_id, "child", "implementation", "z")?;
    let followup = crate::store::coordination::target_task(&store.db, &root.issue_id)?
        .expect("new task for the same Issue");
    assert_ne!(followup, root.task_id);
    store.schedule_assignments(&mut 0)?;
    assert_eq!(placement(&store, "child")?.as_deref(), Some("z"));
    assert_eq!(placement(&store, &followup)?.as_deref(), Some("z"));
    Ok(())
}

#[test]
fn bound_review_still_requires_shared_protocol_and_released_workspace() -> Result<()> {
    let mut store = fixture(Path::new(":memory:"))?;
    announce(&mut store, runner("z-author", 1, 1), 2)?;
    let root = top_level(&mut store, "author", &work())?;
    finish(&store, &root.task_id)?;
    store.db.execute(
        "INSERT INTO handoff_roots VALUES (?1, 'z-author')",
        [&root.issue_id],
    )?;
    announce(&mut store, runner("a-other", 6, 6), 2)?;
    child(&store, &root.issue_id, "review", "review", "z-author")?;
    crate::store::tend_review::bind(
        &store.db,
        "review",
        &root.issue_id,
        &root.task_id,
        "review",
        "review",
        "artifact:fixture",
        "workspace scope",
        &format!("tasks/{}", root.task_id),
    )?;

    let mut without_shared = runner("z-author", 1, 1);
    without_shared.shared_reviews = false;
    announce(&mut store, without_shared, 2)?;
    store.schedule_assignments(&mut 0)?;
    assert_eq!(placement(&store, "review")?, None);
    announce(&mut store, runner("z-author", 1, 1), 1)?;
    store.schedule_assignments(&mut 0)?;
    assert_eq!(placement(&store, "review")?, None);

    child(
        &store,
        &root.issue_id,
        "holder",
        "implementation",
        "a-other",
    )?;
    store.db.execute_batch(
        "INSERT INTO runner_assignments (id, task_id, runner_id, contract, state, expires_at)
           VALUES ('held', 'holder', 'a-other', '{}', 'accepted', 0);
         UPDATE tasks SET current_assignment = 'held', status = 'waiting' WHERE id = 'holder';
         INSERT INTO task_runs (id, assignment_id, task_id, observation)
           VALUES ('run', 'held', 'holder', '{\"phase\":{\"waiting_for_external\":true}}');",
    )?;
    announce(&mut store, runner("z-author", 1, 0), 2)?;
    store.schedule_assignments(&mut 0)?;
    assert_eq!(placement(&store, "review")?, None);
    store.db.execute(
        "UPDATE tasks SET status = 'completed' WHERE id = 'holder'",
        [],
    )?;
    store.schedule_assignments(&mut 0)?;
    // The shared review may queue on its author even though all its slots are full.
    assert_eq!(placement(&store, "review")?.as_deref(), Some("z-author"));
    Ok(())
}

#[test]
fn first_assignment_balances_against_the_successors_effective_limit() -> Result<()> {
    let mut store = fixture(Path::new(":memory:"))?;
    let mut old = runner("old", 4, 2);
    old.workspace_root = Some("/workspace".into());
    let mut next = runner("a-next", 4, 4);
    next.workspace_root = old.workspace_root.clone();
    next.standby = true;
    next.handoff_slots = true;
    announce(&mut store, old, 2)?;
    announce(&mut store, next, 2)?;
    assert!(store.replace_runner("old", "a-next", None)?.is_some());
    super::super::handoff::note_drain_slots(&store.db, "old", 2)?;

    let first = top_level(&mut store, "handoff-first", &work())?;
    assert_eq!(
        placement(&store, &first.task_id)?.as_deref(),
        Some("a-next")
    );
    // a-next has one outstanding assignment in a shared limit of two.
    // The alternative has one locally occupied slot out of four.
    announce(&mut store, runner("b-other", 4, 3), 2)?;
    let second = top_level(&mut store, "handoff-second", &work())?;
    assert_eq!(
        placement(&store, &second.task_id)?.as_deref(),
        Some("b-other")
    );
    Ok(())
}
