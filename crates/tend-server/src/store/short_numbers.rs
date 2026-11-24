use super::Store;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use std::borrow::Cow;

pub fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE issue_short_numbers (
             number INTEGER PRIMARY KEY AUTOINCREMENT CHECK (number > 0),
             issue_id TEXT NOT NULL UNIQUE REFERENCES issues(id)
         );
         INSERT INTO issue_short_numbers (issue_id) SELECT id FROM issues ORDER BY rowid;
         CREATE TRIGGER allocate_issue_short_number AFTER INSERT ON issues
         BEGIN
             INSERT INTO issue_short_numbers (issue_id) VALUES (NEW.id);
         END;
         CREATE TRIGGER immutable_issue_short_number_update BEFORE UPDATE ON issue_short_numbers
         BEGIN
             SELECT RAISE(ABORT, 'issue short number is immutable');
         END;
         CREATE TRIGGER immutable_issue_short_number_delete BEFORE DELETE ON issue_short_numbers
         BEGIN
             SELECT RAISE(ABORT, 'issue short number is immutable');
         END;
         CREATE TRIGGER immutable_issue_short_number_replace BEFORE INSERT ON issue_short_numbers
         WHEN EXISTS (SELECT 1 FROM issue_short_numbers WHERE number = NEW.number)
           OR EXISTS (SELECT 1 FROM issue_short_numbers WHERE issue_id = NEW.issue_id)
         BEGIN
             SELECT RAISE(ABORT, 'issue short number is immutable');
         END;",
    )?;
    Ok(())
}

impl Store {
    pub fn resolve_issue<'a>(&self, reference: &'a str) -> Result<Option<Cow<'a, str>>> {
        if let Some(number) = protocol::parse_issue_number(reference) {
            return Ok(self
                .db
                .query_row(
                    "SELECT issue_id FROM issue_short_numbers WHERE number = ?1",
                    [number],
                    |row| row.get(0),
                )
                .optional()?
                .map(Cow::Owned));
        }
        Ok(protocol::valid_id(reference).then_some(Cow::Borrowed(reference)))
    }
}
