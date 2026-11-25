use super::Store;
use anyhow::Result;
use rusqlite::{Connection, TransactionBehavior, params};

pub(crate) type Digest = [u8; 32];

pub(super) fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE web_sessions (
             id_digest BLOB PRIMARY KEY NOT NULL CHECK(length(id_digest) = 32),
             credential_generation BLOB NOT NULL CHECK(length(credential_generation) = 32),
             origin TEXT NOT NULL,
             expires_at INTEGER NOT NULL
         ) WITHOUT ROWID;
         CREATE INDEX web_sessions_by_expiry ON web_sessions(expires_at);",
    )?;
    Ok(())
}

impl Store {
    pub(crate) fn prepare_web_sessions(&self, generation: &Digest, now: i64) -> Result<()> {
        self.db.execute(
            "DELETE FROM web_sessions WHERE credential_generation != ?1 OR expires_at <= ?2",
            params![generation, now],
        )?;
        Ok(())
    }

    pub(crate) fn issue_web_session(
        &mut self,
        id: &Digest,
        replaced: Option<&Digest>,
        generation: &Digest,
        origin: &str,
        now: i64,
        expires_at: i64,
    ) -> Result<()> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("DELETE FROM web_sessions WHERE expires_at <= ?1", [now])?;
        if let Some(replaced) = replaced {
            tx.execute(
                "DELETE FROM web_sessions
                 WHERE id_digest = ?1 AND credential_generation = ?2 AND origin = ?3",
                params![replaced, generation, origin],
            )?;
        }
        tx.execute(
            "INSERT INTO web_sessions(id_digest, credential_generation, origin, expires_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![id, generation, origin, expires_at],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn valid_web_session(
        &self,
        id: &Digest,
        generation: &Digest,
        origin: &str,
        now: i64,
    ) -> Result<bool> {
        Ok(self.db.query_row(
            "SELECT EXISTS (
                 SELECT 1 FROM web_sessions
                 WHERE id_digest = ?1 AND credential_generation = ?2
                   AND origin = ?3 AND expires_at > ?4
             )",
            params![id, generation, origin, now],
            |row| row.get(0),
        )?)
    }

    pub(crate) fn revoke_web_session(
        &self,
        id: &Digest,
        generation: &Digest,
        origin: &str,
    ) -> Result<()> {
        self.db.execute(
            "DELETE FROM web_sessions
             WHERE id_digest = ?1 AND credential_generation = ?2 AND origin = ?3",
            params![id, generation, origin],
        )?;
        Ok(())
    }
}
