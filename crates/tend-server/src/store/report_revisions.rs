use super::{Store, timing};
use anyhow::Result;
use protocol::{BackfillReportRevision, ReportRevisionReceipt, valid_id};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::fmt;

pub(super) fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE report_revisions (
             previous_report_id TEXT NOT NULL PRIMARY KEY REFERENCES issue_reports(id),
             report_id TEXT NOT NULL UNIQUE REFERENCES issue_reports(id),
             previous_message_id TEXT NOT NULL REFERENCES issue_report_publications(message_id),
             message_id TEXT NOT NULL REFERENCES issue_report_publications(message_id),
             CHECK (previous_report_id != report_id AND previous_message_id != message_id)
         );
         CREATE TABLE report_revision_receipts (
             operation_id TEXT NOT NULL PRIMARY KEY,
             previous_report_id TEXT NOT NULL REFERENCES report_revisions(previous_report_id),
             created INTEGER NOT NULL CHECK (created IN (0, 1))
         );
         CREATE INDEX report_proposal_candidates
           ON runner_operations(json_extract(operation, '$.action.proposal.report_id'))
           WHERE json_extract(operation, '$.action.kind') = 'ask_attention';",
    )?;
    Ok(())
}

#[derive(Debug)]
pub enum Rejection {
    Invalid(&'static str),
    Missing(&'static str),
    Conflict(&'static str),
}

impl fmt::Display for Rejection {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) | Self::Missing(message) | Self::Conflict(message) => {
                out.write_str(message)
            }
        }
    }
}

impl std::error::Error for Rejection {}

impl Store {
    pub fn backfill_report_revision(
        &mut self,
        request: &BackfillReportRevision,
    ) -> Result<ReportRevisionReceipt> {
        if ![
            &request.operation_id,
            &request.old_report_id,
            &request.new_report_id,
        ]
        .into_iter()
        .chain(request.old_message_id.iter())
        .chain(request.new_message_id.iter())
        .all(|id| valid_id(id))
        {
            return Err(Rejection::Invalid("invalid operation, report or message ID").into());
        }
        if request.old_report_id == request.new_report_id {
            return Err(Rejection::Invalid("a report cannot revise itself").into());
        }

        let (tx, measured) = timing::begin(&mut self.db, "report_revision_backfill")?;
        let receipt = record(&tx, request)?;
        timing::commit(tx, measured)?;
        Ok(receipt)
    }
}

struct Topic {
    issue: String,
    chat: String,
    seq: i64,
    message: String,
    user_request: bool,
}

fn topic(db: &Connection, report: &str, missing: &'static str) -> Result<Topic> {
    db.query_row(
        "SELECT i.id, source.chat_id, source.seq, source.id, source.operation_id IS NOT NULL
         FROM issue_reports r JOIN issues i ON i.id = r.issue_id
         JOIN chat_messages source ON source.seq = i.source_seq WHERE r.id = ?1",
        [report],
        |row| {
            Ok(Topic {
                issue: row.get(0)?,
                chat: row.get(1)?,
                seq: row.get(2)?,
                message: row.get(3)?,
                user_request: row.get(4)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| Rejection::Missing(missing).into())
}

fn check_ordinary(db: &Connection, report: &str, topic: &Topic) -> Result<()> {
    let (proposal, wrong_input, unpublished_input) = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM proposal_versions WHERE report_id = ?1)
             OR EXISTS(SELECT 1 FROM runner_operations
                       WHERE json_extract(operation, '$.action.kind') = 'ask_attention'
                         AND json_extract(operation, '$.action.proposal.report_id') = ?1),
             EXISTS(SELECT 1 FROM router_inputs
                    WHERE report_id = ?1 AND (chat_id != ?2 OR message_seq != ?3)),
             EXISTS(SELECT 1 FROM router_inputs WHERE report_id = ?1 AND outcome IS NOT 'completed')",
        params![report, topic.chat, topic.seq],
        |row| Ok((row.get::<_, bool>(0)?, row.get::<_, bool>(1)?, row.get::<_, bool>(2)?)),
    )?;
    if proposal {
        return Err(
            Rejection::Invalid("structured proposal candidates cannot be backfilled").into(),
        );
    }
    if wrong_input {
        return Err(
            Rejection::Conflict("report Router input does not match the Issue request").into(),
        );
    }
    // A live report may have earlier Router replies to the same user request.
    // Do not assign one before finish() can publish the actual result.
    if unpublished_input {
        return Err(
            Rejection::Conflict("report Router input has not completed successfully").into(),
        );
    }
    Ok(())
}

fn publication(
    db: &Connection,
    topic: &Topic,
    report: &str,
    requested: Option<&str>,
    label: &'static str,
) -> Result<String> {
    let mapped = db
        .prepare("SELECT message_id FROM issue_report_publications WHERE report_id = ?1 LIMIT 2")?
        .query_map([report], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let message = requested
        .or_else(|| mapped.first().map(String::as_str))
        .ok_or(Rejection::Invalid(label))?;
    let (belongs, owner) = db
        .query_row(
            "SELECT m.chat_id = ?2 AND m.reply_to IS ?3 AND m.operation_id IS NULL
                    AND NOT EXISTS (SELECT 1 FROM attentions a WHERE a.question_seq = m.seq),
                    p.report_id
             FROM chat_messages m LEFT JOIN issue_report_publications p ON p.message_id = m.id
             WHERE m.id = ?1",
            params![message, topic.chat, topic.message],
            |row| Ok((row.get::<_, bool>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .optional()?
        .ok_or(Rejection::Missing("publication message not found"))?;
    if !belongs {
        return Err(Rejection::Conflict(
            "publication is not a Router reply to this Issue request in its original Chat",
        )
        .into());
    }
    if mapped.len() > 1 || mapped.first().is_some_and(|id| id != message) {
        return Err(Rejection::Conflict(
            "the report has a different or ambiguous publication mapping",
        )
        .into());
    }
    if owner.is_some_and(|id| id != report) {
        return Err(
            Rejection::Conflict("the publication message belongs to another report").into(),
        );
    }
    Ok(message.to_owned())
}

fn record(tx: &Transaction<'_>, request: &BackfillReportRevision) -> Result<ReportRevisionReceipt> {
    let old = &request.old_report_id;
    let new = &request.new_report_id;
    if let Some(receipt) = tx
        .query_row(
            "SELECT r.issue_id, e.previous_report_id, e.report_id, e.previous_message_id, e.message_id, o.created
             FROM report_revision_receipts o JOIN report_revisions e USING (previous_report_id)
             JOIN issue_reports r ON r.id = e.report_id WHERE o.operation_id = ?1",
            [&request.operation_id],
            |row| {
                Ok(ReportRevisionReceipt {
                    operation_id: request.operation_id.clone(),
                    issue_id: row.get(0)?,
                    old_report_id: row.get(1)?,
                    new_report_id: row.get(2)?,
                    old_message_id: row.get(3)?,
                    new_message_id: row.get(4)?,
                    created: row.get(5)?,
                })
            },
        )
        .optional()?
    {
        if &receipt.old_report_id != old
            || &receipt.new_report_id != new
            || request.old_message_id.as_ref().is_some_and(|id| *id != receipt.old_message_id)
            || request.new_message_id.as_ref().is_some_and(|id| *id != receipt.new_message_id)
        {
            return Err(Rejection::Conflict("operation ID already records different report or message identities").into());
        }
        return Ok(receipt);
    }

    let topic_old = topic(tx, old, "old report not found")?;
    let topic_new = topic(tx, new, "new report not found")?;
    if topic_old.issue != topic_new.issue || topic_old.chat != topic_new.chat {
        return Err(
            Rejection::Conflict("reports must belong to the same Issue and original Chat").into(),
        );
    }
    if !topic_old.user_request {
        return Err(Rejection::Conflict("the Issue source is not an original user request").into());
    }
    check_ordinary(tx, old, &topic_old)?;
    check_ordinary(tx, new, &topic_new)?;
    let old_message = publication(
        tx,
        &topic_old,
        old,
        request.old_message_id.as_deref(),
        "old_message_id is required without a saved publication",
    )?;
    let new_message = publication(
        tx,
        &topic_new,
        new,
        request.new_message_id.as_deref(),
        "new_message_id is required without a saved publication",
    )?;
    if old_message == new_message {
        return Err(
            Rejection::Conflict("the two reports cannot share a publication message").into(),
        );
    }

    let successor = tx
        .query_row(
            "SELECT report_id, previous_message_id, message_id FROM report_revisions WHERE previous_report_id = ?1",
            [old],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)),
        )
        .optional()?;
    let created = if let Some((saved_new, saved_old_message, saved_new_message)) = successor {
        if &saved_new != new || saved_old_message != old_message || saved_new_message != new_message
        {
            return Err(Rejection::Conflict(
                "old report already has a different successor or publication",
            )
            .into());
        }
        false
    } else {
        let (predecessor, cyclic) = tx.query_row(
            "WITH RECURSIVE following(id) AS (
                 SELECT ?2 UNION
                 SELECT e.report_id FROM report_revisions e JOIN following f ON e.previous_report_id = f.id
             )
             SELECT EXISTS(SELECT 1 FROM report_revisions WHERE report_id = ?2),
                    EXISTS(SELECT 1 FROM following WHERE id = ?1)",
            params![old, new],
            |row| Ok((row.get::<_, bool>(0)?, row.get::<_, bool>(1)?)),
        )?;
        if predecessor {
            return Err(
                Rejection::Conflict("new report already has a different predecessor").into(),
            );
        }
        if cyclic {
            return Err(Rejection::Conflict("report revision would form a cycle").into());
        }
        for (report, message) in [(old, &old_message), (new, &new_message)] {
            tx.execute(
                "INSERT INTO issue_report_publications (message_id, report_id) VALUES (?1, ?2)
                 ON CONFLICT(message_id) DO NOTHING",
                params![message, report],
            )?;
        }
        tx.execute(
            "INSERT INTO report_revisions (previous_report_id, report_id, previous_message_id, message_id)
             VALUES (?1, ?2, ?3, ?4)",
            params![old, new, old_message, new_message],
        )?;
        true
    };
    tx.execute(
        "INSERT INTO report_revision_receipts (operation_id, previous_report_id, created) VALUES (?1, ?2, ?3)",
        params![request.operation_id, old, created],
    )?;
    Ok(ReportRevisionReceipt {
        operation_id: request.operation_id.clone(),
        issue_id: topic_old.issue,
        old_report_id: old.clone(),
        new_report_id: new.clone(),
        old_message_id: old_message,
        new_message_id: new_message,
        created,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        sync::{
            Barrier,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
        time::{Duration, Instant},
    };

    #[test]
    fn report_revision_migrates_and_replays_under_real_writer_contention() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("tend-report-revision-{}", protocol::new_id()));
        fs::create_dir(&dir)?;
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(dir.clone());
        let path = dir.join("server.sqlite");
        let original = Store::initialize(&path)?;
        // Restore the pre-feature schema and seed an existing v26 publication.
        super::super::board::remove_projection_schema(&original.db)?;
        original.db.execute_batch(
            "DROP TABLE report_revision_receipts;
             DROP TABLE report_revisions;
             DROP INDEX report_proposal_candidates;
             PRAGMA user_version = 26;
             INSERT INTO chat_messages (seq, id, chat_id, text, operation_id) VALUES
                 (1, 'request', 'main', 'Synthetic request', 'send');
             INSERT INTO chat_messages (id, chat_id, text, reply_to) VALUES
                 ('old-message', 'main', 'Old report', 'request'),
                 ('new-message', 'main', 'New report', 'request');
             INSERT INTO issues (id, source_seq, title, objective) VALUES ('issue', 1, 'Fixture', '');
             INSERT INTO runners (id, host_id, profile, last_seen) VALUES ('runner', 'local', '{}', 0);
             INSERT INTO runner_operations (id, runner_id, operation) VALUES
                 ('old', 'runner', '{}'), ('new', 'runner', '{}');
             INSERT INTO issue_reports VALUES ('old', 'issue', ''), ('new', 'issue', '');
             INSERT INTO issue_report_publications VALUES ('new-message', 'new');",
        )?;
        drop(original);
        let mut keeper = Store::initialize(&path)?;
        let request = BackfillReportRevision {
            operation_id: "operation".into(),
            old_report_id: "old".into(),
            new_report_id: "new".into(),
            old_message_id: Some("old-message".into()),
            new_message_id: None,
        };
        keeper.db.execute_batch(
            "CREATE TRIGGER reject_receipt BEFORE INSERT ON report_revision_receipts
             BEGIN SELECT RAISE(ABORT, 'synthetic receipt failure'); END;",
        )?;
        let failure = keeper.backfill_report_revision(&request).unwrap_err();
        assert!(failure.to_string().contains("synthetic receipt failure"));
        assert_eq!(
            counts(&keeper)?,
            (0, 0, 1),
            "late failure rolls back edge and mapping"
        );
        keeper.db.execute_batch("DROP TRIGGER reject_receipt")?;

        let lock = keeper
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        static BLOCKED: AtomicUsize = AtomicUsize::new(0);
        fn on_busy(attempts: i32) -> bool {
            if attempts == 0 {
                BLOCKED.fetch_add(1, Ordering::SeqCst);
            }
            thread::sleep(Duration::from_millis(4));
            attempts < 2500
        }
        let start = Barrier::new(3);
        let receipts = thread::scope(|scope| {
            let contenders = (0..2)
                .map(|_| {
                    scope.spawn(|| {
                        let mut contender = Store::open(&path).unwrap();
                        contender.db.busy_handler(Some(on_busy)).unwrap();
                        start.wait();
                        contender.backfill_report_revision(&request)
                    })
                })
                .collect::<Vec<_>>();
            start.wait();
            let deadline = Instant::now() + Duration::from_secs(5);
            while BLOCKED.load(Ordering::SeqCst) < 2 && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(4));
            }
            let both_waiting = BLOCKED.load(Ordering::SeqCst) >= 2;
            drop(lock);
            assert!(
                both_waiting,
                "both independent SQLite writers should hit the held lock"
            );
            contenders
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect::<Result<Vec<_>>>()
        })?;
        assert_eq!(receipts[0], receipts[1]);
        assert!(receipts[0].created);
        assert_eq!(receipts[0].new_message_id, "new-message");
        assert_eq!(counts(&keeper)?, (1, 1, 2));
        let issue = keeper.issue_overview("issue")?.unwrap();
        let [current] = issue.reports.as_slice() else {
            panic!("expected one ordinary chain")
        };
        assert_eq!(current.report_id.as_deref(), Some("new"));
        assert_eq!(current.history[0].report_id, "old");
        Ok(())
    }
    #[test]
    fn report_revision_cannot_claim_an_earlier_reply_while_publication_is_pending() -> Result<()> {
        let mut store = Store::initialize(std::path::Path::new(":memory:"))?;
        store.db.execute_batch(
            "INSERT INTO chat_messages (seq, id, chat_id, text, operation_id) VALUES
                 (1, 'request', 'main', 'Synthetic request', 'send');
             INSERT INTO chat_messages (id, chat_id, text, reply_to) VALUES
                 ('old-message', 'main', 'Old report', 'request'),
                 ('earlier-message', 'main', 'An unrelated earlier reply', 'request');
             INSERT INTO issues (id, source_seq, title, objective) VALUES ('issue', 1, 'Fixture', '');
             INSERT INTO runners (id, host_id, profile, last_seen) VALUES ('runner', 'local', '{}', 0);
             INSERT INTO runner_operations (id, runner_id, operation) VALUES
                 ('old', 'runner', '{}'), ('pending', 'runner', '{}'), ('failed', 'runner', '{}');
             INSERT INTO issue_reports VALUES ('old', 'issue', ''), ('pending', 'issue', ''), ('failed', 'issue', '');
             INSERT INTO issue_report_publications VALUES ('old-message', 'old');
             INSERT INTO router_inputs (seq, chat_id, message_seq, report_id, outcome) VALUES
                 (1, 'main', 1, 'pending', NULL), (2, 'main', 1, 'failed', 'failed');",
        )?;
        let mut request = BackfillReportRevision {
            operation_id: "operation".into(),
            old_report_id: "old".into(),
            new_report_id: "failed".into(),
            old_message_id: None,
            new_message_id: Some("earlier-message".into()),
        };
        for name in ["failed", "pending"] {
            request.new_report_id = name.into();
            assert!(matches!(
                store
                    .backfill_report_revision(&request)
                    .unwrap_err()
                    .downcast_ref(),
                Some(Rejection::Conflict(
                    "report Router input has not completed successfully"
                ))
            ));
            assert_eq!(counts(&store)?, (0, 0, 1));
        }
        store.finish(
            1,
            &[codex_app::Answer {
                item_id: "actual-answer",
                text: "Published report",
            }],
            None,
        )?;
        assert!(matches!(
            store
                .backfill_report_revision(&request)
                .unwrap_err()
                .downcast_ref(),
            Some(Rejection::Conflict(
                "the report has a different or ambiguous publication mapping"
            ))
        ));
        request.new_message_id = None;
        let receipt = store.backfill_report_revision(&request)?;
        let genuine: String = store.db.query_row(
            "SELECT id FROM chat_messages WHERE native_item_id = 'actual-answer'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(receipt.new_message_id, genuine);
        assert_eq!(counts(&store)?, (1, 1, 2));
        Ok(())
    }

    fn counts(store: &Store) -> Result<(i64, i64, i64)> {
        Ok(store.db.query_row(
            "SELECT (SELECT count(*) FROM report_revisions),
                    (SELECT count(*) FROM report_revision_receipts),
                    (SELECT count(*) FROM issue_report_publications)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?)
    }
}
