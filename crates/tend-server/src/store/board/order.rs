use super::{read_attentions, read_progress};
use anyhow::Result;
use protocol::BoardStage;
use rusqlite::{Connection, Transaction, params};

// The two ordered streams are disjoint. A deadline is a second key only
// once it has actually elapsed. Keep the seek and limit ahead of card joins.
pub(super) const BOARD_KEYS: &str = "WITH keys AS (
    SELECT i.id, i.board_entry_key AS entry_ms FROM issues i
    WHERE (i.board_entry_key, i.id) < (?2, ?3)
      AND (i.board_ready_at IS NULL OR i.board_ready_at > ?1)
      AND (NOT ?4 OR i.parent_issue_id IS NULL)
      AND (?5 IS NULL OR EXISTS (
            SELECT 1 FROM chat_messages m WHERE m.seq = i.source_seq AND m.chat_id = ?5))
    UNION ALL
    SELECT i.id, i.board_ready_at AS entry_ms FROM issues i
    WHERE i.board_ready_at <= ?1 AND (i.board_ready_at, i.id) < (?2, ?3)
      AND (NOT ?4 OR i.parent_issue_id IS NULL)
      AND (?5 IS NULL OR EXISTS (
            SELECT 1 FROM chat_messages m WHERE m.seq = i.source_seq AND m.chat_id = ?5))
)";

pub(crate) fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(
        "ALTER TABLE issues ADD COLUMN board_stage TEXT;
         ALTER TABLE issues ADD COLUMN board_entered_at INTEGER;
         ALTER TABLE issues ADD COLUMN board_ready_at INTEGER;
         ALTER TABLE issues ADD COLUMN board_entry_key INTEGER
             GENERATED ALWAYS AS (COALESCE(board_entered_at, 0)) VIRTUAL;
         DROP INDEX pending_issue_inputs;
         CREATE INDEX pending_issue_inputs ON issue_inputs(task_id, wake_at) WHERE run_id IS NULL;",
    )?;
    // Seed only current comparison facts, never an entry time for a legacy card.
    let now = now(db)?;
    let mut after = String::new();
    loop {
        let batch = db
            .prepare_cached("SELECT id FROM issues WHERE id > ?1 ORDER BY id LIMIT 128")?
            .query_map([&after], |row| Ok((row.get::<_, String>(0)?, false)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let Some((last, _)) = batch.last() else {
            break;
        };
        after.clone_from(last);
        project(db, &batch, now)?;
    }
    db.execute_batch(
        "CREATE INDEX board_entry_order ON issues(board_entry_key DESC, id DESC);
         CREATE INDEX board_deadline_order ON issues(board_ready_at DESC, id DESC)
             WHERE board_ready_at IS NOT NULL;",
    )?;
    Ok(())
}

// Install only after the main schema is ready. The dirty set is in memory on
// each connection, participates in the surrounding transaction and is empty
// again after either commit or rollback. None of these triggers write to WAL.
pub(crate) fn install_tracking(db: &Connection) -> Result<()> {
    db.execute_batch(
        "PRAGMA temp_store = MEMORY;
         CREATE TEMP TABLE board_dirty (id TEXT PRIMARY KEY, created INTEGER NOT NULL DEFAULT 0) WITHOUT ROWID;
         CREATE TEMP TRIGGER board_new_issue AFTER INSERT ON main.issues BEGIN
             INSERT INTO board_dirty(id, created) VALUES (NEW.id, 1)
             ON CONFLICT(id) DO UPDATE SET created = 1;
         END;
         CREATE TEMP TRIGGER board_issue_state AFTER UPDATE OF state ON main.issues
         WHEN OLD.state IS NOT NEW.state BEGIN
             INSERT INTO board_dirty(id) VALUES (NEW.id) ON CONFLICT(id) DO NOTHING;
         END;
         CREATE TEMP TRIGGER board_park_insert AFTER INSERT ON main.issue_parks BEGIN
             INSERT INTO board_dirty(id) VALUES (NEW.issue_id) ON CONFLICT(id) DO NOTHING;
         END;
         CREATE TEMP TRIGGER board_park_delete AFTER DELETE ON main.issue_parks BEGIN
             INSERT INTO board_dirty(id) VALUES (OLD.issue_id) ON CONFLICT(id) DO NOTHING;
         END;
         CREATE TEMP TRIGGER board_new_task AFTER INSERT ON main.tasks BEGIN
             INSERT INTO board_dirty(id) VALUES (NEW.issue_id) ON CONFLICT(id) DO NOTHING;
         END;
         CREATE TEMP TRIGGER board_task_status AFTER UPDATE OF status, workspace_key ON main.tasks
         WHEN OLD.status IS NOT NEW.status OR OLD.workspace_key IS NOT NEW.workspace_key BEGIN
             INSERT INTO board_dirty(id) VALUES (NEW.issue_id) ON CONFLICT(id) DO NOTHING;
         END;
         CREATE TEMP TRIGGER board_new_run AFTER INSERT ON main.task_runs BEGIN
             INSERT INTO board_dirty(id) SELECT issue_id FROM tasks WHERE id = NEW.task_id AND status = 'waiting'
             ON CONFLICT(id) DO NOTHING;
         END;
         CREATE TEMP TRIGGER board_run_phase AFTER UPDATE OF observation ON main.task_runs
         WHEN OLD.observation IS NOT NEW.observation BEGIN
             INSERT INTO board_dirty(id) SELECT issue_id FROM tasks WHERE id = NEW.task_id AND status = 'waiting'
             ON CONFLICT(id) DO NOTHING;
         END;
         CREATE TEMP TRIGGER board_new_input AFTER INSERT ON main.issue_inputs
         WHEN NEW.run_id IS NULL BEGIN
             INSERT INTO board_dirty(id) SELECT issue_id FROM tasks WHERE id = NEW.task_id AND status = 'waiting'
             ON CONFLICT(id) DO NOTHING;
         END;
         CREATE TEMP TRIGGER board_input_readiness AFTER UPDATE OF task_id, run_id, wake_at ON main.issue_inputs
         WHEN OLD.task_id IS NOT NEW.task_id OR OLD.run_id IS NOT NEW.run_id OR OLD.wake_at IS NOT NEW.wake_at BEGIN
             INSERT INTO board_dirty(id) SELECT issue_id FROM tasks WHERE status = 'waiting' AND id IN (
                 CASE WHEN OLD.run_id IS NULL THEN OLD.task_id END,
                 CASE WHEN NEW.run_id IS NULL THEN NEW.task_id END
             ) ON CONFLICT(id) DO NOTHING;
         END;
         CREATE TEMP TRIGGER board_new_attention AFTER INSERT ON main.attentions
         WHEN NEW.question_seq IS NOT NULL AND NEW.answer_input_id IS NULL BEGIN
             INSERT INTO board_dirty(id) VALUES (NEW.issue_id) ON CONFLICT(id) DO NOTHING;
         END;
         CREATE TEMP TRIGGER board_attention_status AFTER UPDATE OF question_seq, answer_input_id ON main.attentions
         WHEN (OLD.question_seq IS NOT NULL AND OLD.answer_input_id IS NULL)
           != (NEW.question_seq IS NOT NULL AND NEW.answer_input_id IS NULL) BEGIN
             INSERT INTO board_dirty(id) VALUES (NEW.issue_id) ON CONFLICT(id) DO NOTHING;
         END;",
    )?;
    Ok(())
}

pub(crate) fn commit(tx: Transaction<'_>) -> Result<()> {
    refresh(&tx)?;
    tx.commit()?;
    Ok(())
}

/// Finalize a transaction that can mutate any of the board's input facts.
/// Timed write paths call this before the existing timing::commit.
pub(crate) fn refresh(tx: &Transaction<'_>) -> Result<()> {
    refresh_using(tx, || now(tx))
}

fn refresh_using(
    tx: &Transaction<'_>,
    clock: impl FnOnce() -> rusqlite::Result<i64>,
) -> Result<()> {
    let affected = tx
        .prepare_cached(
            "WITH RECURSIVE affected(id) AS (
                 SELECT id FROM temp.board_dirty
                 UNION
                 SELECT i.parent_issue_id FROM affected a JOIN issues i ON i.id = a.id
                 WHERE i.parent_issue_id IS NOT NULL
             )
             SELECT a.id, COALESCE(d.created, 0)
             FROM affected a LEFT JOIN temp.board_dirty d ON d.id = a.id",
        )?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !affected.is_empty() {
        project(tx, &affected, clock()?)?;
        tx.execute("DELETE FROM temp.board_dirty", [])?;
    }
    Ok(())
}

const NOW_MS: &str = "CAST(unixepoch('subsec') * 1000 AS INTEGER)";

fn now(db: &Connection) -> rusqlite::Result<i64> {
    db.query_row(&format!("SELECT {NOW_MS}"), [], |row| row.get(0))
}

pub(super) fn snapshot_time(db: &Connection) -> rusqlite::Result<i64> {
    // Touch main so a deferred read transaction pins its WAL snapshot
    // before we sample the clock used for both selection and enrichment.
    db.query_row(
        &format!("SELECT {NOW_MS}, (SELECT 1 FROM issues LIMIT 1)"),
        [],
        |row| row.get(0),
    )
}

fn stage_key(stage: BoardStage) -> &'static str {
    match stage {
        BoardStage::Backlog => "backlog",
        BoardStage::Todo => "todo",
        BoardStage::InProgress => "in_progress",
        BoardStage::Waiting => "waiting",
        BoardStage::Done => "done",
        BoardStage::Cancelled => "cancelled",
    }
}

fn project(db: &Connection, affected: &[(String, bool)], now: i64) -> Result<()> {
    let ids = serde_json::to_string(&affected.iter().map(|(id, _)| id).collect::<Vec<_>>())?;
    let mut progress = read_progress(db, &ids)?;
    let attentions = read_attentions(db, &ids)?;
    let mut facts = db.prepare_cached(
        "SELECT i.state, COALESCE((SELECT status FROM tasks WHERE issue_id = i.id
                    ORDER BY rowid DESC LIMIT 1), 'backlog'),
                i.board_stage, i.board_entered_at, i.board_ready_at
         FROM issues i WHERE i.id = ?1",
    )?;
    let mut update = db.prepare_cached(
        "UPDATE issues SET board_stage = ?2, board_entered_at = ?3, board_ready_at = ?4 WHERE id = ?1",
    )?;

    for (id, created) in affected {
        let (state, task, prior_stage, entered, deadline) = facts.query_row([id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, Option<i64>>(4)?,
            ))
        })?;
        let progress = progress.remove(id).unwrap_or_default();
        let stage = progress.stage(&state, &task, attentions.contains_key(id), now);
        let next_deadline = (stage == BoardStage::Waiting)
            .then_some(progress.ready_at)
            .flatten();
        let stage = stage_key(stage);
        let matured = deadline.filter(|&time| time <= now);
        let effective_stage = if matured.is_some() {
            Some("todo")
        } else {
            prior_stage.as_deref()
        };
        let effective_entry = matured.or(entered);
        let next_entry = if *created || effective_stage.is_some_and(|prior| prior != stage) {
            Some(now)
        } else {
            effective_entry
        };
        if prior_stage.as_deref() != Some(stage)
            || entered != next_entry
            || deadline != next_deadline
        {
            update.execute(params![id, stage, next_entry, next_deadline])?;
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn remove_projection_schema(db: &Connection) -> Result<()> {
    db.execute_batch(
        "DROP INDEX board_entry_order;
         DROP INDEX board_deadline_order;
         ALTER TABLE issues DROP COLUMN board_entry_key;
         ALTER TABLE issues DROP COLUMN board_ready_at;
         ALTER TABLE issues DROP COLUMN board_entered_at;
         ALTER TABLE issues DROP COLUMN board_stage;
         DROP INDEX pending_issue_inputs;
         CREATE INDEX pending_issue_inputs ON issue_inputs(task_id) WHERE run_id IS NULL;",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
