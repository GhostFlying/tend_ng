use super::*;

const NOW: i64 = 1_790_000_000;
const OWNER: &str = "ou_fixture";
const OLD: i64 = NOW - 8 * 86_400;

fn scan_case(store: &Store) -> Result<Case> {
    Ok(by_subject(&store.db, SCAN, OWNER)?.expect("scan case"))
}

fn store() -> Result<Store> {
    Store::initialize(std::path::Path::new(":memory:"))
}

#[test]
fn scan_age_is_not_checkpoint_age_and_recovery_requires_complete_window() -> Result<()> {
    let mut s = store()?;
    s.record_feishu_window(OWNER, OLD, OLD, true, &mut [])?;
    s.feishu_scan_phase(OWNER, "running", None)?;
    s.maintenance_probe_scan(Some(OWNER), NOW)?;
    let failed = scan_case(&s)?;
    assert_eq!((&*failed.status, failed.revision), ("active", 1));
    assert!(matches!(
        s.maintenance_ignore(&failed.id, 1, true)?,
        Change::Ok(())
    ));

    // A fresh successful commit at the same cursor is not actual progress.
    s.record_feishu_window(OWNER, OLD, NOW + 1, true, &mut [])?;
    s.maintenance_probe_scan(Some(OWNER), NOW + 1)?;
    let same = scan_case(&s)?;
    assert_eq!(same.id, failed.id);
    assert_eq!(
        (same.first_seen, same.last_seen, same.revision, same.ignored),
        (NOW, NOW + 1, 1, true)
    );

    // More than seven days behind but an atomically committed new window.
    s.record_feishu_window(OWNER, OLD + 86400, NOW + 2, false, &mut [])?;
    s.maintenance_probe_scan(Some(OWNER), NOW + 2)?;
    let catching_up = scan_case(&s)?;
    assert_eq!(
        (
            &*catching_up.status,
            catching_up.revision,
            catching_up.ignored
        ),
        ("not_observed", 2, false)
    );
    assert_eq!(catching_up.last_seen, NOW + 1);
    assert!(matches!(
        s.maintenance_ignore(&failed.id, 1, true)?,
        Change::Conflict
    ));

    // A full window at an old target can still leave substantial backlog.
    s.record_feishu_window(OWNER, OLD + 2 * 86400, NOW + 3, true, &mut [])?;
    s.maintenance_probe_scan(Some(OWNER), NOW + 3)?;
    assert_eq!(scan_case(&s)?.status, "not_observed");
    s.record_feishu_window(OWNER, NOW + 4, NOW + 4, false, &mut [])?;
    s.maintenance_probe_scan(Some(OWNER), NOW + 4)?;
    assert_eq!(scan_case(&s)?.status, "not_observed");
    s.record_feishu_window(OWNER, NOW + 4, NOW + 5, true, &mut [])?;
    s.maintenance_probe_scan(Some(OWNER), NOW + 5)?;
    let recovered = scan_case(&s)?;
    assert_eq!(
        (&*recovered.status, &recovered.id, recovered.first_seen),
        ("recovered", &failed.id, NOW)
    );

    s.maintenance_probe_scan(Some(OWNER), NOW + 5 + STALL_AFTER)?;
    let renewed = scan_case(&s)?;
    assert_eq!(
        (&*renewed.status, renewed.id, renewed.first_seen),
        ("active", failed.id, NOW)
    );
    assert!(renewed.revision > recovered.revision);
    Ok(())
}

#[test]
fn scan_retry_phase_and_same_failure_do_not_reset_ignored_evidence() -> Result<()> {
    let mut s = store()?;
    s.record_feishu_window(OWNER, OLD, OLD, true, &mut [])?;
    s.feishu_scan_phase(OWNER, "failed", Some("provider_unavailable"))?;
    s.maintenance_probe_scan(Some(OWNER), NOW)?;
    let first = scan_case(&s)?;
    assert!(matches!(
        s.maintenance_ignore(&first.id, 1, true)?,
        Change::Ok(())
    ));

    for (step, phase, error) in [
        (1, "running", None),
        (2, "catching_up", None),
        (3, "failed", Some("provider_unavailable")),
        (4, "catching_up", None),
    ] {
        s.feishu_scan_phase(OWNER, phase, error)?;
        if phase == "catching_up" {
            let public = s.feishu_status(Some(OWNER))?;
            assert_eq!(public["phase"], "running");
            assert_eq!(public["gap"], "catching_up");
        }
        s.maintenance_probe_scan(Some(OWNER), NOW + step)?;
        let same = scan_case(&s)?;
        assert_eq!(
            (same.id, same.revision, same.ignored),
            (first.id.clone(), 1, true)
        );
        assert_eq!(same.last_seen, NOW + step);
        assert_eq!(same.fault, first.fault);
    }
    s.interrupt_feishu_scan(OWNER)?;
    assert_eq!(s.feishu_status(Some(OWNER))?["gap"], "interrupted");
    s.maintenance_probe_scan(Some(OWNER), NOW + 5)?;
    assert!(scan_case(&s)?.ignored);

    s.feishu_scan_phase(OWNER, "failed", Some("incomplete_search"))?;
    s.maintenance_probe_scan(Some(OWNER), NOW + 6)?;
    let changed = scan_case(&s)?;
    assert_eq!(
        (changed.id, changed.revision, changed.ignored),
        (first.id, 2, false)
    );
    Ok(())
}

#[test]
fn scan_restart_preserves_ignored_evidence_and_projects_resume_phase() -> Result<()> {
    for gap in [
        None,
        Some("catching_up"),
        Some("interrupted"),
        Some("provider_unavailable"),
    ] {
        let mut s = store()?;
        s.record_feishu_window(OWNER, OLD, OLD, true, &mut [])?;
        s.db.execute(
            "UPDATE feishu_scan SET phase='catching_up',gap=?1 WHERE owner=?2",
            params![gap, OWNER],
        )?;
        s.maintenance_probe_scan(Some(OWNER), NOW)?;
        let first = scan_case(&s)?;
        assert!(matches!(
            s.maintenance_ignore(&first.id, first.revision, true)?,
            Change::Ok(())
        ));

        s.interrupt_feishu_scan(OWNER)?;
        let public = s.feishu_status(Some(OWNER))?;
        assert_eq!(public["phase"], "interrupted");
        assert_eq!(public["gap"], "interrupted");
        for (step, phase, hint) in [
            (1, "running", "interrupted"),
            (2, "interrupted", "interrupted"),
            (3, "running", "interrupted"),
            (4, "catching_up", "catching_up"),
        ] {
            if phase == "interrupted" {
                s.interrupt_feishu_scan(OWNER)?;
            } else {
                s.feishu_scan_phase(OWNER, phase, None)?;
            }
            let public = s.feishu_status(Some(OWNER))?;
            assert_eq!(
                public["phase"],
                if phase == "catching_up" {
                    "running"
                } else {
                    phase
                }
            );
            assert_eq!(public["gap"], hint);
            let persisted: Option<String> = s.db.query_row(
                "SELECT gap FROM feishu_scan WHERE owner=?1",
                [OWNER],
                |row| row.get(0),
            )?;
            assert_eq!(persisted.as_deref(), gap);
            s.maintenance_probe_scan(Some(OWNER), NOW + step)?;
            let same = scan_case(&s)?;
            assert_eq!(
                (same.id, same.revision, same.ignored),
                (first.id.clone(), 1, true)
            );
            assert_eq!(same.fault, first.fault);
        }
        s.feishu_scan_phase(OWNER, "failed", Some("incomplete_search"))?;
        let public = s.feishu_status(Some(OWNER))?;
        assert_eq!(public["phase"], "failed");
        assert_eq!(public["gap"], "incomplete_search");
        s.maintenance_probe_scan(Some(OWNER), NOW + 5)?;
        let failed = scan_case(&s)?;
        assert_eq!(
            (failed.id, failed.revision, failed.ignored),
            (first.id, 2, false)
        );
    }
    Ok(())
}

#[test]
fn old_scan_baseline_survives_same_checkpoint_and_missing_coverage_is_not_recovery() -> Result<()> {
    let mut s = store()?;
    s.feishu_scan_phase(OWNER, "running", None)?;
    s.maintenance_probe_scan(Some(OWNER), NOW)?;
    assert!(by_subject(&s.db, SCAN, OWNER)?.is_none());

    s.db.execute(
        "UPDATE feishu_scan SET checkpoint_at=?1,last_success_at=?1 WHERE owner=?2",
        params![OLD, OWNER],
    )?;
    s.record_feishu_window(OWNER, OLD, NOW, true, &mut [])?;
    s.maintenance_probe_scan(Some(OWNER), NOW)?;
    let recorded = scan_case(&s)?;
    assert_eq!(recorded.status, "active");
    s.maintenance_probe_scan(Some("ou_other"), NOW + 1)?;
    let not_current = scan_case(&s)?;
    assert_eq!(not_current.status, "not_observed");
    assert!(
        not_current
            .followup
            .expect("followup")
            .known
            .contains("不代表扫描恢复")
    );
    s.maintenance_probe_scan(Some(OWNER), NOW + 2)?;
    let again = scan_case(&s)?;
    assert_eq!((&*again.status, again.id), ("active", recorded.id));
    Ok(())
}

#[test]
fn old_cursor_with_fresh_window_and_no_prior_case_does_not_alert() -> Result<()> {
    let mut s = store()?;
    s.record_feishu_window(OWNER, OLD, OLD, true, &mut [])?;
    s.record_feishu_window(OWNER, OLD + 86400, NOW, false, &mut [])?;
    s.maintenance_probe_scan(Some(OWNER), NOW + 5)?;
    assert!(by_subject(&s.db, SCAN, OWNER)?.is_none());
    let complete: i64 = s.db.query_row(
        "SELECT completed_through FROM maintenance_scan_progress",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(complete, OLD);
    Ok(())
}

fn issue(store: &Store) -> Result<()> {
    store.db.execute_batch(
        "INSERT INTO chat_messages(id,chat_id,text) VALUES('request','main','Synthetic work');
         INSERT INTO issues(id,source_seq,title,objective) VALUES('issue',1,'Sample [title]','Synthetic work');
         INSERT INTO issues(id,source_seq,title,objective,parent_issue_id)
            VALUES('child',1,'Sample [child]','Synthetic work','issue');
         INSERT INTO runners(id,host_id,profile,last_seen) VALUES('runner','host','{}',0);
         INSERT INTO tasks(id,issue_id,category,status,repo_ids,required_capabilities)
            VALUES('task','child','implementation','accepted','[]','[]');
         INSERT INTO runner_assignments(id,task_id,runner_id,contract,state,expires_at)
            VALUES('claim','task','runner','{\"placement\":{\"agent\":{\"id\":\"fixture\"}}}','accepted',0);
         UPDATE tasks SET current_assignment='claim' WHERE id='task';"
    )?;
    Ok(())
}

fn run_evidence(store: &Store, id: &str, outcome: RunOutcome, wait: bool) -> Result<()> {
    let mut run = RunObservation {
        event_id: format!("{id}-start"),
        assignment_id: "claim".into(),
        task_id: "task".into(),
        run_id: id.into(),
        thread_id: Some("thread".into()),
        turn_id: Some(format!("{id}-turn")),
        input_ids: Vec::new(),
        branches: Vec::new(),
        started_at: Some(NOW),
        phase: RunPhase::Started,
    };
    assert!(super::super::run::apply(&store.db, "runner", &run)?);
    run.event_id = format!("{id}-stop");
    run.phase = RunPhase::Stopped {
        outcome,
        error: None,
        waiting_for_children: false,
        waiting_for_attention: false,
        queued_input: false,
        waiting_for_external: wait,
    };
    assert!(super::super::run::apply(&store.db, "runner", &run)?);
    Ok(())
}

#[test]
fn task_probe_ignores_queue_wait_unproven_failure_and_ancestor_park() -> Result<()> {
    let mut store = store()?;
    issue(&store)?;
    for state in ["awaiting_assignment", "waiting", "running", "failed"] {
        store.db.execute("UPDATE tasks SET status=?1", [state])?;
        store.maintenance_probe_tasks(&mut TaskCursors::default(), NOW)?;
        assert!(by_subject(&store.db, TASK, "task")?.is_none(), "{state}");
    }
    store.db.execute("UPDATE tasks SET status='accepted'", [])?;
    run_evidence(&store, "wait", RunOutcome::Failed, true)?;
    store.maintenance_probe_tasks(&mut TaskCursors::default(), NOW)?;
    assert!(by_subject(&store.db, TASK, "task")?.is_none());
    store.db.execute("UPDATE tasks SET status='accepted'", [])?;
    run_evidence(&store, "fail", RunOutcome::Failed, false)?;
    store
        .db
        .execute("INSERT INTO issue_parks VALUES('issue')", [])?;
    store.maintenance_probe_tasks(&mut TaskCursors::default(), NOW)?;
    assert!(by_subject(&store.db, TASK, "task")?.is_none());
    Ok(())
}

#[test]
fn task_recovery_is_run_proof_not_status_edit_and_material_replays_are_stable() -> Result<()> {
    let mut s = store()?;
    issue(&s)?;
    let mut cursors = TaskCursors::default();
    run_evidence(&s, "failure-one", RunOutcome::Failed, false)?;
    s.maintenance_probe_tasks(&mut cursors, NOW)?;
    let original = by_subject(&s.db, TASK, "task")?.unwrap();
    assert!(
        original.fault.links[0]
            .href
            .ends_with("#message-failure-one")
    );
    let id = original.id;
    let Change::Ok(main) = s.maintenance_share(&id, 1, Destination::Main)? else {
        panic!("share")
    };
    let Change::Ok(new) = s.maintenance_share(&id, 1, Destination::New)? else {
        panic!("new")
    };
    let new_id = new["chat_id"].as_str().unwrap();
    let Change::Ok(same) = s.maintenance_share(&id, 1, Destination::Existing(new_id))? else {
        panic!("replay")
    };
    assert_eq!(new, same);
    let count: i64 =
        s.db.query_row("SELECT COUNT(*) FROM router_inputs", [], |r| r.get(0))?;
    assert_eq!(count, 0);
    assert_eq!(s.messages("main", i64::MAX)?.unwrap().messages.len(), 2);
    assert_eq!(s.messages(new_id, i64::MAX)?.unwrap().messages.len(), 1);

    s.db.execute("UPDATE issues SET state='closed' WHERE id='issue'", [])?;
    s.maintenance_probe_tasks(&mut cursors, NOW + 1)?;
    assert_eq!(by_id(&s.db, &id)?.unwrap().status, "not_observed");
    let Change::Ok(replayed) = s.maintenance_share(&id, 1, Destination::Main)? else {
        panic!("old replay")
    };
    assert_eq!(main, replayed);
    assert!(matches!(
        s.maintenance_share(&id, 1, Destination::Existing("missing"))?,
        Change::Conflict
    ));

    s.db.execute("UPDATE issues SET state='open' WHERE id='issue'", [])?;
    s.db.execute("UPDATE tasks SET status='completed'", [])?;
    s.maintenance_probe_tasks(&mut cursors, NOW + 2)?;
    assert_eq!(by_id(&s.db, &id)?.unwrap().status, "not_observed");
    s.db.execute("UPDATE tasks SET status='accepted'", [])?;
    run_evidence(&s, "failure-two", RunOutcome::Failed, false)?;
    s.maintenance_probe_tasks(&mut cursors, NOW + 3)?;
    let renewed = by_id(&s.db, &id)?.unwrap();
    assert_eq!(
        (&*renewed.status, &*renewed.fault.marker, renewed.first_seen),
        ("active", "failure-two", NOW)
    );
    s.db.execute("UPDATE tasks SET status='accepted'", [])?;
    run_evidence(&s, "recovered", RunOutcome::Completed, false)?;
    s.maintenance_probe_tasks(&mut cursors, NOW + 4)?;
    let recovered = by_id(&s.db, &id)?.unwrap();
    assert_eq!(recovered.status, "recovered");
    assert_eq!(recovered.last_seen, NOW + 3);
    assert!(
        recovered
            .followup
            .unwrap()
            .known
            .contains("后续 Run recovered")
    );
    assert_eq!(s.messages("main", i64::MAX)?.unwrap().messages.len(), 2);
    Ok(())
}
