use super::Store;
use anyhow::Result;
use protocol::{ControlAck, ControlAction, ControlState, RunnerControl};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

pub fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE local_controls (
            id TEXT PRIMARY KEY, assignment_id TEXT NOT NULL REFERENCES received_assignments(id),
            command TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'pending'
         );
         CREATE INDEX local_controls_pending ON local_controls(assignment_id) WHERE state != 'done';",
    )?;
    db.pragma_update(None, "user_version", 3)?;
    Ok(())
}

pub fn accept(db: &Connection, control: &RunnerControl) -> Result<()> {
    let status: Option<String> = db
        .query_row(
            "SELECT execution_state FROM received_assignments WHERE id = ?1 AND task_id = ?2",
            params![control.assignment_id, control.task_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(status) = status.filter(|state| state != "handed_off") else {
        return Ok(());
    };
    if db.execute(
        "INSERT INTO local_controls(id, assignment_id, command) VALUES (?1, ?2, ?3) ON CONFLICT DO NOTHING",
        params![control.id, control.assignment_id, serde_json::to_string(control)?],
    )? == 0 { return Ok(()); }
    match &control.action {
        ControlAction::Stop if status != "active" => {
            db.execute(
                "UPDATE received_assignments SET execution_state = 'cancelled', retains_workspace = 0
                 WHERE id = ?1 AND execution_state IN ('queued','waiting','waiting_external')",
                [&control.assignment_id],
            )?;
            finish(db, control, ControlState::Stopped)?;
        }
        ControlAction::Steer { .. } => {
            let live = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM local_runs WHERE id = ?1 AND assignment_id = ?2 AND state = 'running')",
                params![control.run_id, control.assignment_id], |row| row.get::<_, bool>(0),
            )?;
            if !live {
                finish(db, control, ControlState::Undelivered)?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn finish(db: &Connection, command: &RunnerControl, state: ControlState) -> Result<()> {
    let ack = ControlAck {
        id: command.id.clone(),
        assignment_id: command.assignment_id.clone(),
        state,
    };
    db.execute(
        "UPDATE local_controls SET state = 'done' WHERE id = ?1",
        [&command.id],
    )?;
    db.execute(
        "INSERT INTO run_outbox(id, assignment_id, observation, record_kind) VALUES (?1, ?2, ?3, 'control')",
        params![command.id, command.assignment_id, serde_json::to_string(&ack)?],
    )?;
    Ok(())
}

pub fn stopped(
    db: &Connection,
    assignment: &str,
    run: &str,
    items: &[Value],
) -> Result<(bool, Vec<(RunnerControl, ControlState)>)> {
    let items_saved = {
        let mut stmt = db.prepare("SELECT command, state FROM local_controls WHERE assignment_id = ?1 AND state != 'done' ORDER BY rowid")?;
        stmt.query_map([assignment], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut cancelled = false;
    let mut commands = Vec::new();
    for (raw, saved) in items_saved {
        let command: RunnerControl = serde_json::from_str(&raw)?;
        let final_state = match command.action {
            ControlAction::Stop => {
                cancelled = true;
                ControlState::Stopped
            }
            ControlAction::Steer { .. } if command.run_id.as_deref() == Some(run) => {
                if items
                    .iter()
                    .any(|item| item["type"] == "userMessage" && item["clientId"] == command.id)
                {
                    ControlState::Delivered
                } else if saved == "pending" {
                    ControlState::Undelivered
                } else {
                    ControlState::Unknown
                }
            }
            _ => continue,
        };
        commands.push((command, final_state));
    }
    Ok((cancelled, commands))
}

impl Store {
    pub fn has_stop(&self, assignment: &str) -> Result<bool> {
        Ok(self.0.query_row(
            "SELECT EXISTS(SELECT 1 FROM local_controls WHERE assignment_id = ?1 AND state != 'done'
                AND json_extract(command, '$.action.kind') = 'stop')",
            [assignment], |row| row.get(0),
        )?)
    }

    pub fn next_control(&self, assignment: &str, run: &str) -> Result<Option<RunnerControl>> {
        let raw: Option<String> = self.0.query_row(
            "SELECT command FROM local_controls
             WHERE assignment_id = ?1 AND state = 'pending' AND
               (json_extract(command, '$.action.kind') = 'stop' OR json_extract(command, '$.run_id') = ?2)
             ORDER BY json_extract(command, '$.action.kind') != 'stop', rowid LIMIT 1",
            params![assignment, run], |row| row.get(0),
        ).optional()?;
        raw.map(|data| Ok(serde_json::from_str(&data)?)).transpose()
    }

    pub fn sending_control(&self, id: &str) -> Result<()> {
        self.0.execute(
            "UPDATE local_controls SET state = 'sending' WHERE id = ?1 AND state = 'pending'",
            [id],
        )?;
        Ok(())
    }

    pub fn observe_steer(&mut self, run: &str, id: &str) -> Result<()> {
        let raw: Option<String> = self.0.query_row(
            "SELECT command FROM local_controls WHERE id = ?1 AND state = 'sending'
              AND json_extract(command, '$.run_id') = ?2 AND json_extract(command, '$.action.kind') = 'steer'",
            params![id, run], |row| row.get(0),
        ).optional()?;
        if let Some(raw) = raw {
            self.finish_control(&serde_json::from_str(&raw)?, ControlState::Delivered)?;
        }
        Ok(())
    }

    pub fn finish_control(&mut self, command: &RunnerControl, state: ControlState) -> Result<()> {
        let tx = self.0.transaction()?;
        finish(&tx, command, state)?;
        tx.commit()?;
        Ok(())
    }
}
