use super::{Accepted, PAGE_SIZE, Store, coordination};
use anyhow::{Result, ensure};
use codex_app::ToolCall;
use protocol::{
    DispatchProvenance, IssueInput, IssueInputCause, IssueMessage, IssueMessageKind,
    IssueMessagePage, Receipt, SendMessage,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

const TREE: &str = "WITH RECURSIVE tree(id) AS (
    SELECT id FROM issues WHERE id = ?1
    UNION ALL SELECT i.id FROM issues i JOIN tree p ON i.parent_issue_id = p.id
)";
const MESSAGE_COLUMNS: &str = "m.id, m.issue_id, i.title, m.kind, m.author, m.text, m.reply_to,
     m.run_id, m.source_message_id, m.created_at, m.forwarded_from, n.number,
     CASE WHEN m.kind = 'dispatch' THEN
          (SELECT source.chat_id FROM chat_messages source WHERE source.id = m.source_message_id)
     END, m.dispatcher_name";

pub fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE INDEX issues_by_parent ON issues(parent_issue_id);
         CREATE TABLE issue_messages (
             seq INTEGER PRIMARY KEY,
             id TEXT NOT NULL UNIQUE,
             issue_id TEXT NOT NULL REFERENCES issues(id),
             kind TEXT NOT NULL CHECK(kind IN ('user', 'agent', 'dispatch', 'relay', 'run')),
             author TEXT NOT NULL,
             text TEXT NOT NULL,
             reply_to TEXT REFERENCES issue_messages(id),
             run_id TEXT REFERENCES task_runs(id),
             source_message_id TEXT REFERENCES chat_messages(id),
             forwarded_from TEXT REFERENCES issue_messages(id),
             operation_id TEXT UNIQUE,
             viewed_issue_id TEXT REFERENCES issues(id),
             created_at INTEGER DEFAULT (unixepoch())
         );
         CREATE INDEX issue_message_pages ON issue_messages(issue_id, seq);
         CREATE UNIQUE INDEX issue_relay_by_source ON issue_messages(issue_id, source_message_id) WHERE kind = 'relay';
         CREATE UNIQUE INDEX issue_relay_by_message ON issue_messages(issue_id, forwarded_from) WHERE forwarded_from IS NOT NULL;
         INSERT INTO issue_messages (id, issue_id, kind, author, text, source_message_id, created_at)
          SELECT coalesce(d.operation_id, c.operation_id), i.id, 'dispatch',
                 coalesce(json_extract(a.contract, '$.placement.agent.id'), 'Router'), i.objective,
                 CASE WHEN d.operation_id IS NOT NULL THEN m.id END, m.created_at
          FROM issues i JOIN chat_messages m ON m.seq = i.source_seq
          LEFT JOIN router_dispatches d ON d.issue_id = i.id
          LEFT JOIN tasks t ON t.issue_id = i.id AND t.previous_task_id IS NULL
          LEFT JOIN child_dispatches c ON c.task_id = t.id
          LEFT JOIN runner_operations op ON op.id = c.operation_id
          LEFT JOIN runner_assignments a ON a.id = json_extract(op.operation, '$.assignment_id')
          WHERE d.operation_id IS NOT NULL OR c.operation_id IS NOT NULL ORDER BY i.rowid;
         INSERT INTO issue_messages (id, issue_id, kind, author, text, run_id, created_at)
          SELECT r.id, t.issue_id, 'run', json_extract(a.contract, '$.placement.agent.id'), '', r.id, NULL
          FROM task_runs r JOIN tasks t ON t.id = r.task_id JOIN runner_assignments a ON a.id = r.assignment_id
          WHERE json_extract(r.observation, '$.turn_id') IS NOT NULL ORDER BY r.rowid;",
    )?;
    Ok(())
}

pub fn migrate_dispatch_and_takeover(db: &Connection) -> Result<()> {
    // Keep seq stable for both message cursors and inline approval anchors.
    db.execute_batch(
        "ALTER TABLE issue_messages RENAME TO old_issue_messages;
         CREATE TABLE issue_messages (
             seq INTEGER PRIMARY KEY,
             id TEXT NOT NULL UNIQUE,
             issue_id TEXT NOT NULL REFERENCES issues(id),
             kind TEXT NOT NULL CHECK(kind IN ('user', 'agent', 'dispatch', 'relay', 'run', 'takeover')),
             author TEXT NOT NULL,
             text TEXT NOT NULL,
             reply_to TEXT REFERENCES issue_messages(id),
             run_id TEXT REFERENCES task_runs(id),
             source_message_id TEXT REFERENCES chat_messages(id),
             forwarded_from TEXT REFERENCES issue_messages(id),
             operation_id TEXT UNIQUE,
             viewed_issue_id TEXT REFERENCES issues(id),
             created_at INTEGER DEFAULT (unixepoch()),
             dispatcher_name TEXT CHECK(dispatcher_name IS NULL OR kind = 'dispatch'),
             assignment_id TEXT UNIQUE REFERENCES runner_assignments(id),
             CHECK((kind = 'takeover') = (assignment_id IS NOT NULL))
         );
         INSERT INTO issue_messages
             (seq, id, issue_id, kind, author, text, reply_to, run_id, source_message_id,
              forwarded_from, operation_id, viewed_issue_id, created_at, dispatcher_name)
         SELECT m.seq, m.id, m.issue_id, m.kind, m.author, m.text, m.reply_to, m.run_id,
                m.source_message_id, m.forwarded_from, m.operation_id, m.viewed_issue_id, m.created_at,
                CASE WHEN m.kind = 'dispatch' AND m.source_message_id IS NULL THEN
                     CASE WHEN m.author <> 'Router' THEN m.author
                          WHEN EXISTS (
                              SELECT 1 FROM runner_operations op
                              JOIN runner_assignments a ON a.id = json_extract(op.operation, '$.assignment_id')
                              WHERE op.id = m.id AND json_extract(a.contract, '$.placement.agent.id') = m.author
                          ) THEN m.author END
                END
         FROM old_issue_messages m ORDER BY m.seq;
         DROP TABLE old_issue_messages;
         CREATE INDEX issue_message_pages ON issue_messages(issue_id, seq);
         CREATE UNIQUE INDEX issue_relay_by_source ON issue_messages(issue_id, source_message_id) WHERE kind = 'relay';
         CREATE UNIQUE INDEX issue_relay_by_message ON issue_messages(issue_id, forwarded_from) WHERE forwarded_from IS NOT NULL;",
    )?;
    Ok(())
}

fn read_message(row: &rusqlite::Row<'_>, offset: usize) -> rusqlite::Result<IssueMessage> {
    let kind: String = row.get(offset + 3)?;
    let kind = match kind.as_str() {
        "user" => IssueMessageKind::User,
        "agent" => IssueMessageKind::Agent,
        "dispatch" => IssueMessageKind::Dispatch,
        "relay" => IssueMessageKind::Relay,
        "run" => IssueMessageKind::Run,
        "takeover" => IssueMessageKind::Takeover,
        _ => unreachable!("issue_messages.kind is constrained"),
    };
    Ok(IssueMessage {
        id: row.get(offset)?,
        issue_id: row.get(offset + 1)?,
        issue_title: row.get(offset + 2)?,
        kind,
        author: row.get(offset + 4)?,
        text: row.get(offset + 5)?,
        reply_to: row.get(offset + 6)?,
        run_id: row.get(offset + 7)?,
        source_message_id: row.get(offset + 8)?,
        created_at: row.get(offset + 9)?,
        forwarded_from: row.get(offset + 10)?,
        short_number: row.get(offset + 11)?,
        dispatch_provenance: if kind == IssueMessageKind::Dispatch {
            let agent_name = row.get(offset + 13)?;
            let source_chat: Option<String> = row.get(offset + 12)?;
            Some(match source_chat.as_deref() {
                Some("main") => DispatchProvenance::MainChat { agent_name },
                Some(_) => DispatchProvenance::OtherChat,
                None => DispatchProvenance::IssueDelegation { agent_name },
            })
        } else {
            None
        },
    })
}

fn message(db: &Connection, id: &str) -> Result<IssueMessage> {
    Ok(db.query_row(
        &format!(
            "SELECT {MESSAGE_COLUMNS} FROM issue_messages m JOIN issues i ON i.id = m.issue_id
                  JOIN issue_short_numbers n ON n.issue_id = i.id WHERE m.id = ?1"
        ),
        [id],
        |row| read_message(row, 0),
    )?)
}

pub fn dispatch(
    db: &Connection,
    id: &str,
    issue: &str,
    author: &str,
    objective: &str,
    source: Option<&str>,
    dispatcher_name: Option<&str>,
) -> Result<()> {
    db.execute(
        "INSERT INTO issue_messages (id, issue_id, kind, author, text, source_message_id, dispatcher_name)
         VALUES (?1, ?2, 'dispatch', ?3, ?4, ?5, ?6)",
        params![id, issue, author, objective, source, dispatcher_name],
    )?;
    Ok(())
}

pub fn record_takeover(db: &Connection, assignment: &str) -> Result<()> {
    ensure!(
        db.execute(
            "INSERT INTO issue_messages (id, issue_id, kind, author, text, assignment_id)
             SELECT ?2, t.issue_id, 'takeover', json_extract(a.contract, '$.placement.agent.id'), '', a.id
             FROM runner_assignments a JOIN tasks t ON t.id = a.task_id
             WHERE a.id = ?1 AND a.state = 'accepted'",
            params![assignment, protocol::new_id()],
        )? == 1,
        "takeover needs an accepted assignment"
    );
    Ok(())
}

pub fn record_run(db: &Connection, run: &str) -> Result<()> {
    db.execute(
        "INSERT INTO issue_messages (id, issue_id, kind, author, text, run_id)
         SELECT r.id, t.issue_id, 'run', json_extract(a.contract, '$.placement.agent.id'), '', r.id
         FROM task_runs r JOIN tasks t ON t.id = r.task_id JOIN runner_assignments a ON a.id = r.assignment_id
         WHERE r.id = ?1",
        [run],
    )?;
    Ok(())
}

pub fn agent_message(
    db: &Connection,
    id: &str,
    task: &str,
    run: &str,
    text: &str,
    reply: Option<&str>,
) -> Result<()> {
    db.execute(
        "INSERT INTO issue_messages (id, issue_id, kind, author, text, reply_to, run_id)
         SELECT ?1, t.issue_id, 'agent', json_extract(a.contract, '$.placement.agent.id'), ?3, ?4, r.id
         FROM task_runs r JOIN tasks t ON t.id = r.task_id JOIN runner_assignments a ON a.id = r.assignment_id
         WHERE r.id = ?2",
        params![id, run, text, reply],
    )?;
    let saved = message(db, id)?;
    let ancestors = {
        let mut statement = db.prepare(
            "WITH RECURSIVE parents(id) AS (
                SELECT parent_issue_id FROM issues WHERE id = ?1 AND parent_issue_id IS NOT NULL
                UNION ALL SELECT i.parent_issue_id FROM issues i JOIN parents p ON i.id = p.id
                 WHERE i.parent_issue_id IS NOT NULL
             ) SELECT id FROM parents",
        )?;
        statement
            .query_map([&saved.issue_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for issue in ancestors {
        if let Some(target) = coordination::latest_task(db, &issue)? {
            enqueue(db, &target, task, &saved)?;
        }
    }
    Ok(())
}

pub fn forward(
    db: &Connection,
    operation: &protocol::WorkOperation,
    issue: &str,
    task: &str,
    child: &str,
    original: &str,
) -> Result<()> {
    let saved = db.execute(
        "INSERT INTO issue_messages (id, issue_id, kind, author, text, source_message_id, forwarded_from)
         SELECT ?1, child.id, 'relay', json_extract(a.contract, '$.placement.agent.id'),
                m.text, m.source_message_id, m.id
           FROM issue_messages m JOIN issues child ON child.id = ?2 AND child.parent_issue_id = m.issue_id
           JOIN runner_assignments a ON a.id = ?3
          WHERE m.id = ?4 AND m.issue_id = ?5 AND m.kind IN ('user', 'relay')
         ON CONFLICT DO NOTHING",
        params![operation.operation_id, child, operation.assignment_id, original, issue],
    )?;
    if saved == 1
        && let Some(target) = coordination::target_task(db, child)?
    {
        enqueue(db, &target, task, &message(db, &operation.operation_id)?)?;
    }
    Ok(())
}

fn enqueue(db: &Connection, target: &str, source_task: &str, message: &IssueMessage) -> Result<()> {
    coordination::enqueue(
        db,
        target,
        IssueInput {
            input_id: protocol::new_id(),
            child_issue_id: message.issue_id.clone(),
            child_task_id: source_task.to_owned(),
            summary: message.text.clone(),
            cause: IssueInputCause::IssueMessage {
                // Runner replay compares the persisted delivery payload for equality.
                message: IssueMessage {
                    short_number: None,
                    ..message.clone()
                },
            },
        },
    )
}

impl Store {
    pub fn issue_messages(&self, issue: &str, before: i64) -> Result<Option<IssueMessagePage>> {
        let header = self
            .db
            .query_row(
                "SELECT i.title, source.chat_id,
                        EXISTS (SELECT 1 FROM router_dispatches d WHERE d.issue_id = i.id AND d.task_id IS NULL), n.number
                 FROM issues i JOIN chat_messages source ON source.seq = i.source_seq
                 JOIN issue_short_numbers n ON n.issue_id = i.id WHERE i.id = ?1",
                [issue],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let Some((title, chat_id, awaiting_start, short_number)) = header else {
            return Ok(None);
        };
        let tx = self.db.unchecked_transaction()?;
        let mut statement = tx.prepare(&format!(
            "{TREE}
            SELECT m.seq, {MESSAGE_COLUMNS} FROM issue_messages m
            JOIN tree ON tree.id = m.issue_id JOIN issues i ON i.id = m.issue_id
            JOIN issue_short_numbers n ON n.issue_id = i.id
            WHERE m.seq < ?2 ORDER BY m.seq DESC LIMIT ?3"
        ))?;
        let mut page = statement
            .query_map(params![issue, before, PAGE_SIZE + 1], |row| {
                Ok((row.get::<_, i64>(0)?, read_message(row, 1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let next_before = if page.len() > PAGE_SIZE {
            page.pop();
            page.last().map(|(seq, _)| *seq)
        } else {
            None
        };
        let approvals =
            super::proposals::issue_events(&tx, issue, next_before.unwrap_or(0), before)?
                .into_iter()
                .map(|(seq, mut event)| {
                    event.before_message_id = page
                        .iter()
                        .rev()
                        .find(|(message_seq, _)| *message_seq > seq)
                        .map(|(_, message)| message.id.clone());
                    event
                })
                .collect();
        drop(statement);
        tx.commit()?;
        Ok(Some(IssueMessagePage {
            issue_id: Some(issue.to_owned()),
            short_number,
            title,
            chat_id,
            awaiting_start,
            messages: page.into_iter().rev().map(|(_, message)| message).collect(),
            next_before,
            approvals,
        }))
    }

    pub fn post_issue_message(
        &mut self,
        issue: &str,
        request: &SendMessage,
    ) -> Result<Option<Accepted>> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = post_message(&tx, issue, request)?;
        super::board::commit(tx)?;
        Ok(result)
    }

    pub fn relay_to_issue(
        &mut self,
        attempt: &str,
        call: &ToolCall,
        issue: &str,
    ) -> Result<Option<IssueMessage>> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let user: Option<(String, String)> = tx.query_row(
            "SELECT user.id, user.text FROM router_attempts r JOIN router_inputs input ON input.seq = r.seq
             JOIN chat_messages user ON user.seq = input.message_seq
             JOIN issues i ON i.id = ?4 JOIN chat_messages origin ON origin.seq = i.source_seq
             WHERE r.id = ?1 AND r.thread_id = ?2 AND r.turn_id = ?3 AND input.outcome IS NULL
               AND input.report_id IS NULL AND input.attention_id IS NULL
               AND input.external_batch IS NULL AND origin.chat_id = input.chat_id",
            params![attempt, call.thread_id, call.turn_id, issue],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?;
        let Some((source, text)) = user else {
            return Ok(None);
        };
        let prior: Option<String> = tx.query_row(
            "SELECT id FROM issue_messages WHERE issue_id = ?1 AND source_message_id = ?2 AND kind = 'relay'",
            params![issue, source], |row| row.get(0),
        ).optional()?;
        if let Some(prior) = prior {
            return Ok(Some(message(&tx, &prior)?));
        }
        let Some(task) = coordination::target_task(&tx, issue)? else {
            return Ok(None);
        };
        let id = protocol::new_id();
        tx.execute(
            "INSERT INTO issue_messages (id, issue_id, kind, author, text, source_message_id)
             VALUES (?1, ?2, 'relay', 'Router', ?3, ?4)",
            params![id, issue, text, source],
        )?;
        let saved = message(&tx, &id)?;
        enqueue(&tx, &task, &task, &saved)?;
        super::board::commit(tx)?;
        Ok(Some(saved))
    }
}

pub(super) fn post_message(
    db: &Connection,
    issue: &str,
    request: &SendMessage,
) -> Result<Option<Accepted>> {
    let prior: Option<(String, String, String, Option<String>)> = db.query_row(
            "SELECT id, viewed_issue_id, text, reply_to FROM issue_messages WHERE operation_id = ?1",
            [&request.operation_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        ).optional()?;
    if let Some((message_id, viewed, text, reply)) = prior {
        return Ok(Some(
            if viewed == issue && text == request.text && reply == request.reply_to {
                Accepted::Received(Receipt { message_id })
            } else {
                Accepted::Conflict
            },
        ));
    }

    let target = if let Some(reply) = &request.reply_to {
        let target: Option<String> = db
            .query_row(
                &format!(
                    "{TREE}
                 SELECT m.issue_id FROM issue_messages m JOIN tree ON tree.id = m.issue_id
                 WHERE m.id = ?2 AND m.kind NOT IN ('run', 'takeover')"
                ),
                params![issue, reply],
                |row| row.get(0),
            )
            .optional()?;
        let Some(target) = target else {
            return Ok(Some(Accepted::InvalidReply));
        };
        target
    } else {
        issue.to_owned()
    };
    let Some(task) = coordination::target_task(db, &target)? else {
        return Ok(None);
    };
    let message_id = protocol::new_id();
    db.execute(
            "INSERT INTO issue_messages (id, issue_id, kind, author, text, reply_to, operation_id, viewed_issue_id)
             VALUES (?1, ?2, 'user', 'user', ?3, ?4, ?5, ?6)",
            params![message_id, target, request.text, request.reply_to, request.operation_id, issue],
        )?;
    enqueue(db, &task, &task, &message(db, &message_id)?)?;
    Ok(Some(Accepted::Received(Receipt { message_id })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn migration_preserves_dispatch_and_reply_identity_without_inventing_history() -> Result<()> {
        let store = Store::initialize(Path::new(":memory:"))?;
        let db = &store.db;
        // Recreate the v33 timeline schema, leaving the adjacent tables and numbering intact.
        db.execute_batch("DROP TABLE issue_messages; DROP INDEX issues_by_parent;")?;
        migrate(db)?;
        db.execute_batch(
            "INSERT INTO chat_messages (seq,id,chat_id,text) VALUES
                (1,'source','main','Original user text'), (2,'other-source','other','Other Chat text');
             INSERT INTO issues (id,source_seq,title,objective,parent_issue_id) VALUES
                ('root',1,'Root','Original objective',NULL),
                ('child',1,'Child','Child objective','root'),
                ('unknown-child',1,'Earlier child','Earlier objective','root'),
                ('other-root',2,'Other root','Other objective',NULL);
             INSERT INTO issue_messages (seq,id,issue_id,kind,author,text,source_message_id,created_at) VALUES
                (7,'root-dispatch','root','dispatch','Router','Original objective','source',123),
                (9,'child-dispatch','child','dispatch','Delegator','Child objective',NULL,124),
                (10,'old-child-dispatch','unknown-child','dispatch','Router','Earlier objective',NULL,124),
                (12,'relay','root','relay','Router','Original user text','source',125),
                (14,'other-dispatch','other-root','dispatch','router','Other objective','other-source',126);
             INSERT INTO issue_messages (seq,id,issue_id,kind,author,text,forwarded_from,source_message_id)
                VALUES (13,'forward','child','relay','Delegator','Original user text','relay','source');
             INSERT INTO issue_messages (seq,id,issue_id,kind,author,text,reply_to,operation_id,viewed_issue_id)
                VALUES (18,'reply','root','user','user','Follow-up','root-dispatch','post-op','root');",
        )?;
        let tx = db.unchecked_transaction()?;
        migrate_dispatch_and_takeover(&tx)?;
        tx.commit()?;

        let root = message(db, "root-dispatch")?;
        assert_eq!(root.author, "Router");
        assert_eq!(root.text, "Original objective");
        assert_eq!(root.created_at, Some(123));
        assert_eq!(root.source_message_id.as_deref(), Some("source"));
        assert_eq!(
            root.dispatch_provenance,
            Some(DispatchProvenance::MainChat { agent_name: None })
        );
        let child = message(db, "child-dispatch")?;
        assert_eq!(
            child.dispatch_provenance,
            Some(DispatchProvenance::IssueDelegation {
                agent_name: Some("Delegator".into()),
            })
        );
        assert_eq!(
            message(db, "old-child-dispatch")?.dispatch_provenance,
            Some(DispatchProvenance::IssueDelegation { agent_name: None })
        );
        assert_eq!(
            message(db, "other-dispatch")?.dispatch_provenance,
            Some(DispatchProvenance::OtherChat)
        );
        let relay = message(db, "forward")?;
        assert_eq!(relay.dispatch_provenance, None);
        assert_eq!(relay.source_message_id.as_deref(), Some("source"));
        assert_eq!(relay.forwarded_from.as_deref(), Some("relay"));
        let reply = message(db, "reply")?;
        assert_eq!(reply.reply_to.as_deref(), Some("root-dispatch"));
        let saved: (i64, String, String) = db.query_row(
            "SELECT seq, operation_id, viewed_issue_id FROM issue_messages WHERE id = 'reply'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(saved, (18, "post-op".into(), "root".into()));
        assert!(!db.prepare("PRAGMA foreign_key_check")?.exists([])?);
        assert!(
            !db.prepare("SELECT 1 FROM issue_messages WHERE kind = 'takeover'")?
                .exists([])?
        );

        dispatch(
            db,
            "named-dispatch",
            "root",
            "router",
            "New objective",
            Some("source"),
            Some("Named Router"),
        )?;
        assert_eq!(
            message(db, "named-dispatch")?.dispatch_provenance,
            Some(DispatchProvenance::MainChat {
                agent_name: Some("Named Router".into()),
            })
        );
        Ok(())
    }
}
