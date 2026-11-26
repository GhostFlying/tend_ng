use super::*;
use crate::store::token_usage::tests::{
    Fixture, assignment, base, issue, native_run, observation, sync,
};
use serde_json::{Value, json, to_value};

fn overview(
    store: &Store,
    parent: Option<&str>,
    before: Option<&TokenOverviewCursor>,
) -> Result<Value> {
    Ok(to_value(store.token_overview(parent, before)?.unwrap())?)
}

#[test]
fn global_counts_direct_contributions_exactly_once_and_seeds_from_existing_rollups() -> Result<()> {
    let file = Fixture::new()?;
    let mut store = Store::initialize(&file.file())?;
    let empty = overview(&store, None, None)?;
    assert_eq!(empty["issues"], json!([]));
    assert_eq!(empty["total"]["started_runs"], 0);
    assert_eq!(empty["total"]["total_tokens"]["tokens"], Value::Null);
    base(&store.db)?;
    issue(&store.db, "root", None)?;
    issue(&store.db, "child", Some("root"))?;
    issue(&store.db, "grand", Some("child"))?;
    issue(&store.db, "other", None)?;
    issue(&store.db, "empty", None)?;
    for id in ["root", "child", "grand", "other"] {
        assignment(&store.db, id, id, "runner")?;
    }
    let root = native_run("run-root", "root");
    let child = native_run("run-child", "child");
    let grand = native_run("run-grand", "grand");
    let other = native_run("run-other", "other");
    let root_usage = observation(&root, 0, Some(i64::MAX));
    let child_usage = observation(&child, 0, Some(0));
    let grand_usage = observation(&grand, 0, Some(i64::MAX));
    sync(
        &mut store,
        "runner",
        &[root, child, grand, other],
        &[
            root_usage,
            child_usage.clone(),
            grand_usage.clone(),
            grand_usage,
        ],
    )?;
    let first = overview(&store, None, None)?;
    assert_eq!(
        first["total"]["total_tokens"],
        json!({"tokens": "18446744073709551614", "reported_requests": 3})
    );
    assert_eq!(first["total"]["observed_runs"], 3);
    assert_eq!(first["total"]["started_runs"], 4);
    assert_eq!(first["total"]["unobserved_no_usage"], 1);
    assert_eq!(first["issues"].as_array().unwrap().len(), 3);
    let children = overview(&store, Some("root"), None)?;
    assert!(children.get("total").is_none());
    let [child] = children["issues"].as_array().unwrap().as_slice() else {
        panic!("expected only the direct child")
    };
    assert_eq!(child["issue_id"], "child");
    assert_eq!(child["direct"]["total_tokens"]["tokens"], "0");
    assert_eq!(
        child["subtree"]["total_tokens"]["tokens"],
        i64::MAX.to_string()
    );
    assert_eq!(child["has_children"], true);
    assert_eq!(
        overview(&store, Some("child"), None)?["issues"][0]["issue_id"],
        "grand"
    );
    assert_eq!(overview(&store, Some("empty"), None)?["issues"], json!([]));
    assert!(store.token_overview(Some("absent"), None)?.is_none());

    assignment(&store.db, "other-metered", "other", "runner")?;
    let outside = native_run("run-other-metered", "other-metered");
    let outside_usage = observation(&outside, 0, Some(13));
    sync(&mut store, "runner", &[outside], &[outside_usage])?;
    let across_roots = overview(&store, None, None)?;
    assert_eq!(
        across_roots["total"]["total_tokens"],
        json!({"tokens": "18446744073709551627", "reported_requests": 4})
    );

    crate::store::remove_token_usage_overview_schema(&store.db)?;
    store.db.pragma_update(None, "user_version", 43)?;
    // Migration reads only the earlier rollups, independently of raw source facts.
    store.db.execute_batch("DELETE FROM token_requests;")?;
    let mut store = Store::initialize(&file.file())?;
    assert_eq!(overview(&store, None, None)?, across_roots);
    // Explicit rebuild is the operation that may consult raw facts.
    store.rebuild_token_usage(&protocol::TokenUsageScope::All)?;
    let rebuilt = overview(&store, None, None)?;
    assert_eq!(rebuilt["total"]["started_runs"], 5);
    assert_eq!(rebuilt["total"]["observed_runs"], 0);
    assert_eq!(rebuilt["total"]["total_tokens"]["tokens"], Value::Null);

    let mut conflict = child_usage.clone();
    conflict.event_id = "conflict".into();
    conflict.last.total_tokens = Some(2);
    sync(&mut store, "runner", &[], &[child_usage])?;
    assert_eq!(
        overview(&store, None, None)?["total"]["total_tokens"]["tokens"],
        "0"
    );
    sync(&mut store, "runner", &[], &[conflict])?;
    assert_eq!(overview(&store, None, None)?["total"], rebuilt["total"]);
    Ok(())
}

#[test]
fn keyset_pages_roots_and_direct_children_with_indexed_bounds() -> Result<()> {
    let file = Fixture::new()?;
    let store = Store::initialize(&file.file())?;
    base(&store.db)?;
    store.db.execute("INSERT INTO chat_messages (seq, id, chat_id, text) VALUES (2, 'new-source', 'fixture', 'New')", [])?;
    for n in 0..70 {
        issue(&store.db, &format!("root-{n:02}"), None)?;
        if n % 2 == 0 {
            store.db.execute(
                "UPDATE issues SET source_seq = 2 WHERE id = ?1",
                [format!("root-{n:02}")],
            )?;
        }
    }
    for n in 0..35 {
        issue(&store.db, &format!("child-{n:02}"), Some("root-00"))?;
    }
    issue(&store.db, "grandchild", Some("child-00"))?;
    store.db.execute(
        "UPDATE issues SET state = 'cancelled' WHERE id = 'root-00'",
        [],
    )?;
    for (parent, expected) in [
        (
            None,
            (0..70)
                .rev()
                .filter(|n| n % 2 == 0)
                .chain((0..70).rev().filter(|n| n % 2 == 1))
                .map(|n| format!("root-{n:02}"))
                .collect::<Vec<_>>(),
        ),
        (
            Some("root-00"),
            (0..35).rev().map(|n| format!("child-{n:02}")).collect(),
        ),
    ] {
        let mut actual = Vec::new();
        let mut before = None;
        for expected_page in expected.chunks(PAGE_SIZE) {
            let page = overview(&store, parent, before.as_ref())?;
            let issues = page["issues"].as_array().unwrap();
            assert_eq!(issues.len(), expected_page.len());
            actual.extend(
                issues
                    .iter()
                    .map(|issue| issue["issue_id"].as_str().unwrap().to_owned()),
            );
            assert_eq!(page.get("total").is_some(), parent.is_none());
            before = page["next_before"]
                .as_str()
                .map(|value| TokenOverviewCursor::decode(value).unwrap());
        }
        assert_eq!(actual, expected);
        assert!(before.is_none());
    }
    let plan = store
        .db
        .prepare(&format!("EXPLAIN QUERY PLAN {PAGE_QUERY}"))?
        .query_map(params![None::<&str>, 2, "root-24", PAGE_SIZE + 1], |row| {
            row.get::<_, String>(3)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    assert!(
        plan.iter().any(|step| step
            .contains("issues_token_usage_page (parent_issue_id=? AND (source_seq,id)<(?,?))")),
        "{plan:?}"
    );
    let page_scan = plan
        .iter()
        .position(|step| step == "SCAN p")
        .expect("materialized page scan");
    assert!(
        !plan[..page_scan]
            .iter()
            .any(|step| step.contains("TEMP B-TREE")),
        "unbounded sort before LIMIT: {plan:?}"
    );
    assert!(
        !plan.iter().any(|step| step.contains("token_requests")
            || step.contains("token_runs")
            || step.contains("token_issue_tree")),
        "{plan:?}"
    );
    Ok(())
}
