use super::*;

struct Database(std::path::PathBuf);

impl Database {
    fn temp() -> Result<Self> {
        let directory = std::env::temp_dir().join(protocol::new_id());
        std::fs::create_dir(&directory)?;
        Ok(Self(directory))
    }

    fn path(&self) -> std::path::PathBuf {
        self.0.join("server.sqlite")
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn cookie_policy_uses_only_explicit_https_or_canonical_loopback_authorities() -> Result<()> {
    let https = Entry::https("https://tend.example:8443")?;
    assert_eq!(https.host, "tend.example:8443");
    assert_eq!(
        https.cookie("fixture", Some(REMEMBER_TTL)).value.as_str(),
        "__Host-tend-session=fixture; Path=/; HttpOnly; SameSite=Lax; Secure; Max-Age=2592000"
    );
    assert_eq!(
        https.cookie("fixture", None).value.as_str(),
        "__Host-tend-session=fixture; Path=/; HttpOnly; SameSite=Lax; Secure"
    );
    for host in ["localhost:8080", "127.0.0.1:8080", "[::1]:8080"] {
        let local = Entry::loopback(host).expect("loopback entry");
        assert_eq!(
            local.cookie("fixture", None).value.as_str(),
            "tend-loopback-session=fixture; Path=/; HttpOnly; SameSite=Lax"
        );
        assert_eq!(local.origin, format!("http://{host}"));
    }
    for value in [
        "http://tend.example",
        "https://tend.example/",
        "https://tend.example/x",
    ] {
        assert!(Entry::https(value).is_err());
    }
    for value in ["tend.example", "127.0.0.2", "localhost.example"] {
        assert!(Entry::loopback(value).is_none());
    }
    Ok(())
}

#[test]
fn cookie_routes_are_explicit_and_method_specific() {
    for (method, path) in [
        (Method::Get, "/api/v1/runners"),
        (Method::Get, protocol::EXECUTION_HOSTS_PATH),
        (Method::Get, "/api/v1/issues/%2342/messages?before=10"),
        (Method::Get, "/api/v1/issues/42/attention-replies?before=10"),
        (Method::Get, "/api/v1/issues/42/token-usage"),
        (Method::Post, "/api/v1/issues/42/attentions/question/answer"),
        (Method::Post, "/api/v1/issues/42/start"),
        (Method::Post, "/api/v1/issues/%2342/backlog"),
        (Method::Post, "/api/v1/issues/42/state"),
        (
            Method::Post,
            "/api/v1/chats/main/attentions/test-id/approve",
        ),
        (Method::Get, "/api/v1/artifacts/fixture"),
        (Method::Get, "/api/v1/runs/fixture/rollout"),
    ] {
        assert!(web_route(&method, path), "{method} {path}");
    }
    for (method, path) in [
        (Method::Post, "/api/v1/runners/fixture/sync"),
        (Method::Post, "/api/v1/runners/fixture/reconcile/task"),
        (Method::Put, "/api/v1/runners/fixture/artifacts/file"),
        (Method::Post, "/api/v1/runners/fixture/retire"),
        (Method::Get, "/api/v1/upgrade-check/2"),
        (Method::Get, "/api/v1/issues/42/start"),
        (Method::Get, "/api/v1/issues/42/backlog"),
        (Method::Get, "/api/v1/issues/42/reclaim"),
        (Method::Post, "/api/v1/issues/42/reclaim"),
        (Method::Get, "/api/v1/issues/42/attentions/question/answer"),
        (Method::Post, "/api/v1/issues/42/attention-replies"),
        (
            Method::Post,
            "/api/v1/issues/42/attentions/question/approve",
        ),
        (Method::Get, "/api/v1/runs/fixture"),
        (Method::Head, "/api/v1/issues/42/state"),
        (Method::Post, protocol::REPORT_REVISIONS_PATH),
    ] {
        assert!(!web_route(&method, path), "{method} {path}");
    }
}

#[test]
fn stored_sessions_are_origin_bound_absolute_and_independently_revocable() -> Result<()> {
    let db = Database::temp()?;
    let path = db.path();
    let mut store = Store::initialize(&path)?;
    let origin = "https://tend.example";
    let first = Sessions::new(Some(origin), "fixture-old-token")?.generation;
    let second = Sessions::new(Some(origin), "fixture-new-token")?.generation;
    let session = digest(&[1u8; 32]);
    let remembered = digest(&[2u8; 32]);
    let now = 1000;
    store.issue_web_session(&session, None, &first, origin, now, now + SESSION_TTL)?;
    store.issue_web_session(&remembered, None, &first, origin, now, now + REMEMBER_TTL)?;
    drop(store);
    let store = Store::initialize(&path)?;
    store.prepare_web_sessions(&first, now + 1)?;
    assert!(store.valid_web_session(&session, &first, origin, now + SESSION_TTL - 1)?);
    assert!(!store.valid_web_session(&session, &first, origin, now + SESSION_TTL)?);
    assert!(store.valid_web_session(&remembered, &first, origin, now + REMEMBER_TTL - 1)?);
    assert!(!store.valid_web_session(&remembered, &first, origin, now + REMEMBER_TTL)?);
    assert!(!store.valid_web_session(&remembered, &first, "https://tend.example:8443", now)?);
    assert!(!store.valid_web_session(&remembered, &second, origin, now)?);

    store.revoke_web_session(&session, &first, origin)?;
    assert!(!store.valid_web_session(&session, &first, origin, now)?);
    assert!(store.valid_web_session(&remembered, &first, origin, now)?);
    store.prepare_web_sessions(&second, now)?;
    store.prepare_web_sessions(&first, now)?;
    assert!(!store.valid_web_session(&remembered, &first, origin, now)?);
    Ok(())
}

#[test]
fn upgrades_v29_without_recreating_the_issue_attention_reply_table() -> Result<()> {
    use rusqlite::Connection;

    let db = Database::temp()?;
    let path = db.path();
    drop(Store::initialize(&path)?);
    let sql = Connection::open(&path)?;
    // Roll back later schemas without recreating the v29 reply table.
    crate::store::remove_token_usage_schema(&sql)?;
    crate::store::pull::restore_legacy_schema(&sql)?;
    sql.execute_batch(
        "DROP TABLE discussion_handoffs;
         DROP TABLE discussion_drafts;
         DROP TABLE discussion_references;
         DROP TABLE discussions;
         DROP TABLE web_sessions;
         DROP VIEW runner_handoff_limits;
         ALTER TABLE runner_replacements DROP COLUMN occupied_slots;
         ALTER TABLE runner_replacements DROP COLUMN slot_budget;
         PRAGMA user_version = 29;",
    )?;
    let reply_schema = |db: &Connection| {
        db.query_row(
            "SELECT rootpage, sql FROM sqlite_schema WHERE name = 'issue_attention_replies'",
            [],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        )
    };
    let before = reply_schema(&sql)?;
    drop(sql);
    let store = Store::initialize(&path)?;
    store.prepare_web_sessions(&[0; 32], 0)?;
    let sql = Connection::open(path)?;
    let version = sql.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))?;
    assert_eq!(version, crate::store::SCHEMA_VERSION);
    assert_eq!(before, reply_schema(&sql)?);
    Ok(())
}
