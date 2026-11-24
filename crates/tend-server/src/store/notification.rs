use super::{PAGE_SIZE, Store};
use anyhow::Result;
use protocol::{IssueActivity, NotificationPage};
use rusqlite::{Connection, params};

pub fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE issue_activity (
             seq INTEGER PRIMARY KEY,
             chat_id TEXT NOT NULL,
             issue_id TEXT NOT NULL REFERENCES issues(id),
             task_id TEXT REFERENCES tasks(id),
             previous TEXT NOT NULL,
             current TEXT NOT NULL,
             changed_at INTEGER NOT NULL DEFAULT (unixepoch())
         );
         CREATE INDEX issue_activity_by_chat ON issue_activity(chat_id, seq);
         CREATE TRIGGER notify_issue_state AFTER UPDATE OF state ON issues
         WHEN NEW.state != OLD.state
         BEGIN
             INSERT INTO issue_activity (chat_id, issue_id, previous, current)
             SELECT m.chat_id, NEW.id, OLD.state, NEW.state
             FROM chat_messages m WHERE m.seq = NEW.source_seq;
         END;
         CREATE TRIGGER notify_task_status AFTER UPDATE OF status ON tasks
         WHEN NEW.status != OLD.status AND NEW.status IN ('running', 'waiting', 'completed', 'failed', 'cancelled')
         BEGIN
             INSERT INTO issue_activity (chat_id, issue_id, task_id, previous, current)
             SELECT m.chat_id, i.id, NEW.id, OLD.status, NEW.status
             FROM issues i JOIN chat_messages m ON m.seq = i.source_seq WHERE i.id = NEW.issue_id;
         END;",
    )?;
    Ok(())
}

impl Store {
    pub fn notifications(&self, chat: &str, before: i64) -> Result<NotificationPage> {
        let mut query = self.db.prepare(
            "SELECT a.seq, a.issue_id, i.title, a.task_id, a.previous, a.current, a.changed_at
             FROM issue_activity a JOIN issues i ON i.id = a.issue_id
             WHERE a.chat_id = ?1 AND a.seq < ?2 ORDER BY a.seq DESC LIMIT ?3",
        )?;
        let mut activities = query
            .query_map(params![chat, before, PAGE_SIZE + 1], |row| {
                Ok(IssueActivity {
                    seq: row.get(0)?,
                    issue_id: row.get(1)?,
                    issue_title: row.get(2)?,
                    task_id: row.get(3)?,
                    previous: row.get(4)?,
                    current: row.get(5)?,
                    changed_at: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let next_before = if activities.len() > PAGE_SIZE {
            activities.pop();
            activities.last().map(|activity| activity.seq)
        } else {
            None
        };
        Ok(NotificationPage {
            activities,
            next_before,
        })
    }
}
