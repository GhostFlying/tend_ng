use super::*;
use crate::store::{PAGE_SIZE, Store};
use protocol::{
    ExternalFact, ExternalKind, IssueBoardCursor, IssueTransition, LinkPull, PullFacts, PullRef,
    SendMessage,
};
use rusqlite::TransactionBehavior::Immediate;
use std::path::Path;

fn fixture() -> Result<Store> {
    let store = Store::initialize(Path::new(":memory:"))?;
    store.db.execute_batch(
        "INSERT INTO chat_messages (seq, id, chat_id, text) VALUES
          (1, 'source', 'main', 'Fixture'), (2, 'other', 'other', 'Fixture');
         INSERT INTO runners (id, host_id, profile, last_seen) VALUES ('runner', 'fixture', '{}', 0);",
    )?;
    Ok(store)
}

fn write(store: &mut Store, time: i64, sql: &str) -> Result<()> {
    let tx = store.db.transaction_with_behavior(Immediate)?;
    tx.execute_batch(sql)?;
    refresh_using(&tx, || Ok(time))?;
    tx.commit()?;
    Ok(())
}

fn saved(store: &Store, id: &str) -> Result<(String, Option<i64>, Option<i64>)> {
    Ok(store.db.query_row(
        "SELECT board_stage, board_entered_at, board_ready_at FROM issues WHERE id = ?1",
        [id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?)
}

fn front(store: &Store) -> Result<Vec<(String, BoardStage)>> {
    Ok(store
        .issue_board(None, None, true)?
        .issues
        .into_iter()
        .map(|card| (card.id, card.stage))
        .collect())
}

#[test]
fn reentry_moves_the_card_but_messages_attention_and_pr_ci_do_not() -> Result<()> {
    let mut store = fixture()?;
    write(
        &mut store,
        100,
        "
      INSERT INTO issues (id, source_seq, title, objective) VALUES ('z', 1, 'First Issue', '');
      INSERT INTO tasks (id, issue_id, category, status, repo_ids, required_capabilities)
        VALUES ('tz', 'z', 'fixture', 'accepted', '[]', '[]');",
    )?;
    write(
        &mut store,
        200,
        "
      INSERT INTO issues (id, source_seq, title, objective) VALUES ('a', 1, 'Second Issue', '');
      INSERT INTO tasks (id, issue_id, category, status, repo_ids, required_capabilities)
        VALUES ('ta', 'a', 'fixture', 'accepted', '[]', '[]');",
    )?;
    assert_eq!(
        front(&store)?,
        [
            ("a".into(), BoardStage::Todo),
            ("z".into(), BoardStage::Todo)
        ]
    );
    assert_eq!(saved(&store, "z")?, ("todo".into(), Some(100), None));

    assert!(
        store
            .post_issue_message(
                "z",
                &SendMessage {
                    operation_id: "message".into(),
                    text: "Same stage".into(),
                    reply_to: None,
                }
            )?
            .is_some()
    );
    write(
        &mut store,
        300,
        "
      INSERT INTO runner_operations (id, runner_id, operation) VALUES ('attention', 'runner', '{}');
      INSERT INTO attentions (id, issue_id, task_id, source_seq, request)
        VALUES ('attention', 'z', 'tz', 1, '{}');
      UPDATE attentions SET question_seq = 2 WHERE id = 'attention';",
    )?;

    let pull = PullRef::parse("https://github.com/example/guide/pull/12").unwrap();
    store.link_pull(
        "z",
        &LinkPull {
            operation_id: "link".into(),
            url: pull.url(),
        },
        &pull,
    )?;
    let sha = "a".repeat(40);
    store.observe_pull(
        &pull,
        Ok((
            PullFacts {
                title: "Fixture".into(),
                state: "open".into(),
                draft: false,
                head_sha: sha.clone(),
                ci: Some("success".into()),
                head_branch: None,
                merge_commit_sha: None,
                head_repository: None,
            },
            vec![ExternalFact {
                url: pull.url(),
                event: ExternalKind::Ci,
                state: "success".into(),
                head_sha: Some(sha),
                check: Some("test".into()),
                run_id: Some("1".into()),
            }],
        )),
    )?;
    let page = store.issue_board(None, None, true)?;
    assert_eq!(
        page.issues
            .iter()
            .map(|card| card.id.as_str())
            .collect::<Vec<_>>(),
        ["a", "z"]
    );
    assert!(page.issues[1].attention.is_some());
    assert_eq!(
        page.issues[1].pulls[0]
            .facts
            .as_ref()
            .unwrap()
            .ci
            .as_deref(),
        Some("success")
    );
    assert_eq!(saved(&store, "z")?, ("todo".into(), Some(100), None));

    write(
        &mut store,
        400,
        "UPDATE tasks SET status = 'running' WHERE id = 'tz';",
    )?;
    assert_eq!(saved(&store, "z")?, ("in_progress".into(), Some(400), None));
    write(
        &mut store,
        500,
        "UPDATE tasks SET status = 'accepted' WHERE id = 'tz';",
    )?;
    assert_eq!(saved(&store, "z")?, ("todo".into(), Some(500), None));
    assert_eq!(
        front(&store)?,
        [
            ("z".into(), BoardStage::Todo),
            ("a".into(), BoardStage::Todo)
        ]
    );
    Ok(())
}

#[test]
fn projection_follows_the_whole_tree_without_changing_stage_priority() -> Result<()> {
    let mut store = fixture()?;
    write(&mut store, 100, "
      INSERT INTO issues (id, source_seq, title, objective) VALUES ('root', 1, 'Root', '');
      INSERT INTO issues (id, source_seq, title, objective, parent_issue_id)
        VALUES ('child', 1, 'Child', '', 'root'), ('leaf', 1, 'Leaf', '', 'child');
      INSERT INTO tasks (id, issue_id, category, status, repo_ids, required_capabilities, workspace_key)
        VALUES ('worker', 'leaf', 'fixture', 'running', '[]', '[]', 'shared');")?;
    // A descendant's actual run outranks both ancestors' missing own tasks.
    assert_eq!(front(&store)?, [("root".into(), BoardStage::InProgress)]);
    assert_eq!(
        saved(&store, "child")?,
        ("in_progress".into(), Some(100), None)
    );
    write(&mut store, 200, "
      INSERT INTO tasks (id, issue_id, category, status, repo_ids, required_capabilities, workspace_key)
        VALUES ('owner', 'root', 'fixture', 'accepted', '[]', '[]', 'shared');
      UPDATE tasks SET status = 'waiting' WHERE id = 'worker';
      INSERT INTO runner_assignments (id, task_id, runner_id, contract, state, expires_at)
        VALUES ('a', 'worker', 'runner', '{}', 'accepted', 0);
      INSERT INTO task_runs (id, assignment_id, task_id, observation)
        VALUES ('run', 'a', 'worker', '{\"phase\":{\"waiting_for_external\":true}}');")?;
    assert_eq!(saved(&store, "root")?, ("waiting".into(), Some(200), None));
    assert_eq!(front(&store)?, [("root".into(), BoardStage::Waiting)]);
    write(&mut store, 300, "
      UPDATE tasks SET workspace_key = 'independent' WHERE id = 'owner';
      UPDATE task_runs SET observation = '{\"phase\":{\"waiting_for_external\":true}}' WHERE id = 'run';")?;
    assert_eq!(saved(&store, "root")?, ("todo".into(), Some(300), None));

    write(
        &mut store,
        400,
        "
      UPDATE tasks SET status = 'running' WHERE id = 'worker';
      UPDATE issues SET state = 'closed' WHERE id = 'root';",
    )?;
    assert_eq!(front(&store)?, [("root".into(), BoardStage::Done)]);
    assert_eq!(saved(&store, "root")?, ("done".into(), Some(400), None));

    let cancellation = IssueTransition {
        operation_id: "cancel".into(),
        state: "cancelled".into(),
        reason: "Fixture".into(),
    };
    store.transition_issue("root", &cancellation)?;
    let cancelled = saved(&store, "root")?;
    assert_eq!(cancelled.0, "cancelled");
    assert!(cancelled.1.unwrap() > 400);
    store.transition_issue("root", &cancellation)?;
    assert_eq!(saved(&store, "root")?, cancelled);
    assert_eq!(front(&store)?, [("root".into(), BoardStage::Cancelled)]);
    Ok(())
}

#[test]
fn child_window_uses_its_deadline_and_is_replaced_or_cancelled_atomically() -> Result<()> {
    let mut store = fixture()?;
    write(
        &mut store,
        100,
        "
      INSERT INTO issues (id, source_seq, title, objective)
        VALUES ('due', 1, 'Due', ''), ('suppressed', 1, 'Suppressed', ''),
               ('still-future', 1, 'Future', ''), ('compare', 1, 'Compare', '');
      INSERT INTO tasks (id, issue_id, category, status, repo_ids, required_capabilities)
        SELECT id, id, 'fixture', 'waiting', '[]', '[]' FROM issues;
      INSERT INTO issue_inputs(id, task_id, payload, wake_at) VALUES
        ('due-input', 'due', '{}', 400), ('suppressed-input', 'suppressed', '{}', 400),
        ('future-input', 'still-future', '{}', 9000000000000);",
    )?;
    assert_eq!(
        saved(&store, "due")?,
        ("waiting".into(), Some(100), Some(400))
    );

    // A rolled-back transaction leaves neither a changed deadline nor dirty entries.
    {
        let tx = store.db.transaction_with_behavior(Immediate)?;
        tx.execute(
            "UPDATE issue_inputs SET wake_at = 500 WHERE id = 'due-input'",
            [],
        )?;
        refresh_using(&tx, || Ok(200))?;
    }
    assert_eq!(
        saved(&store, "due")?,
        ("waiting".into(), Some(100), Some(400))
    );
    assert_eq!(
        store
            .db
            .query_row("SELECT count(*) FROM temp.board_dirty", [], |row| row
                .get::<_, u32>(0))?,
        0
    );

    write(
        &mut store,
        300,
        "
      UPDATE issue_inputs SET wake_at = 450 WHERE id = 'due-input';
      UPDATE tasks SET status = 'running' WHERE id = 'suppressed';
      UPDATE tasks SET status = 'accepted' WHERE id = 'compare';",
    )?;
    assert_eq!(
        saved(&store, "due")?,
        ("waiting".into(), Some(100), Some(450))
    );
    assert_eq!(
        saved(&store, "suppressed")?,
        ("in_progress".into(), Some(300), None)
    );
    // The fixture's 450 is already in the past to a real board read.
    assert_eq!(
        front(&store)?,
        [
            ("due".into(), BoardStage::Todo),
            ("suppressed".into(), BoardStage::InProgress),
            ("compare".into(), BoardStage::Todo),
            ("still-future".into(), BoardStage::Waiting),
        ]
    );
    assert_eq!(
        saved(&store, "due")?,
        ("waiting".into(), Some(100), Some(450)),
        "board read never writes"
    );
    write(
        &mut store,
        600,
        "UPDATE tasks SET status = 'accepted' WHERE id = 'due';",
    )?;
    assert_eq!(
        saved(&store, "due")?,
        ("todo".into(), Some(450), None),
        "keeps original deadline, not the later write time"
    );
    Ok(())
}

#[test]
fn keyset_sorts_before_limiting_and_keeps_equal_and_unknown_entries_stable() -> Result<()> {
    let mut store = fixture()?;
    // Simulate pre-v28 data inserted with no projection or temp-tracked creation.
    remove_projection_schema(&store.db)?;
    store.db.execute_batch(
        "
      INSERT INTO issues (id, source_seq, title, objective)
        VALUES ('legacy-z', 1, 'Unknown Z', ''), ('legacy-a', 1, 'Unknown A', '');",
    )?;
    store.db.execute("DELETE FROM temp.board_dirty", [])?;
    migrate(&store.db)?;
    assert_eq!(saved(&store, "legacy-z")?, ("backlog".into(), None, None));

    // Insert the two newest Issue rows FIRST and the tied rows in another order.
    write(
        &mut store,
        300,
        "
      INSERT INTO issues (id, source_seq, title, objective)
        VALUES ('newest', 1, 'Newest entry', ''), ('other-chat', 2, 'Other chat', ''),
               ('nested', 1, 'Nested', '');
      UPDATE issues SET parent_issue_id = 'legacy-z' WHERE id = 'nested';",
    )?;
    write(
        &mut store,
        200,
        "
      WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i < 51)
      INSERT INTO issues (id, source_seq, title, objective)
        SELECT printf('tie-%02d', i), 1, 'Tied entry', '' FROM n;",
    )?;
    assert_eq!(saved(&store, "legacy-z")?.1, None);
    let first = store.issue_board(Some("main"), None, true)?;
    let mut expected = vec!["newest".to_owned()];
    expected.extend((3..=51).rev().map(|i| format!("tie-{i:02}")));
    assert_eq!(first.issues.len(), PAGE_SIZE);
    assert_eq!(
        first
            .issues
            .into_iter()
            .map(|card| card.id)
            .collect::<Vec<_>>(),
        expected
    );
    let cursor = IssueBoardCursor::decode(&first.next_before.unwrap()).unwrap();
    assert_eq!(
        cursor,
        IssueBoardCursor {
            entry_ms: 200,
            issue_id: "tie-03".into()
        }
    );
    let next = store.issue_board(Some("main"), Some(&cursor), true)?;
    assert_eq!(
        next.issues
            .into_iter()
            .map(|card| card.id)
            .collect::<Vec<_>>(),
        ["tie-02", "tie-01", "legacy-z", "legacy-a"]
    );
    assert!(next.next_before.is_none());
    assert_eq!(
        store
            .issue_board(
                Some("main"),
                Some(&IssueBoardCursor {
                    entry_ms: 0,
                    issue_id: "legacy-z".into(),
                }),
                true
            )?
            .issues[0]
            .id,
        "legacy-a"
    );
    let all = store.issue_board(None, None, false)?;
    assert_eq!(
        all.issues[..3]
            .iter()
            .map(|card| card.id.as_str())
            .collect::<Vec<_>>(),
        ["other-chat", "newest", "nested"]
    );
    Ok(())
}

#[test]
fn board_keys_use_ordered_index_streams() -> Result<()> {
    let store = fixture()?;
    let plan: Vec<String> = store.db.prepare(&format!(
        "EXPLAIN QUERY PLAN {BOARD_KEYS} SELECT id, entry_ms FROM keys ORDER BY entry_ms DESC, id DESC LIMIT ?6"
    ))?.query_map(params![500, 400, "cursor-id", true, "main", 51], |row| row.get(3))?
        .collect::<rusqlite::Result<_>>()?;
    let plan = plan.join("\n");
    assert!(
        plan.contains("SEARCH i USING INDEX board_entry_order"),
        "{plan}"
    );
    assert!(
        plan.contains("SEARCH i USING INDEX board_deadline_order"),
        "{plan}"
    );
    assert!(!plan.contains("TEMP B-TREE"), "{plan}");
    Ok(())
}
