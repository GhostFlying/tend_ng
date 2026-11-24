use anyhow::Result;
use protocol::{RunObservation, RunOutcome, RunPhase};
use rusqlite::{Connection, OptionalExtension, params};

pub fn create_tables(db: &Connection) -> Result<()> {
    db.execute_batch(
        "ALTER TABLE issues ADD COLUMN state TEXT NOT NULL DEFAULT 'open';
         CREATE TABLE runner_run_events (
            id TEXT PRIMARY KEY,
            runner_id TEXT NOT NULL REFERENCES runners(id),
            observation TEXT NOT NULL
         );
         CREATE TABLE task_runs (
            id TEXT PRIMARY KEY,
            assignment_id TEXT NOT NULL REFERENCES runner_assignments(id),
            task_id TEXT NOT NULL REFERENCES tasks(id),
            observation TEXT NOT NULL
         );
         CREATE INDEX task_runs_by_task ON task_runs(task_id);",
    )?;
    Ok(())
}

pub fn apply(db: &Connection, runner: &str, observation: &RunObservation) -> Result<bool> {
    let original = serde_json::to_string(observation)?;
    let prior = db
        .query_row(
            "SELECT runner_id, observation FROM runner_run_events WHERE id = ?1",
            [&observation.event_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    if let Some((source, prior)) = prior {
        return Ok(source == runner && prior == original);
    }

    let authorized = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM runner_assignments a JOIN tasks t ON a.task_id = t.id
         WHERE a.id = ?1 AND a.runner_id = ?2 AND t.id = ?3 AND a.state = 'accepted'
           AND t.current_assignment = a.id)",
        params![observation.assignment_id, runner, observation.task_id],
        |row| row.get::<_, bool>(0),
    )?;
    if !authorized {
        return Ok(false);
    }

    let previous = db
        .query_row(
            "SELECT observation FROM task_runs WHERE id = ?1",
            [&observation.run_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|value| serde_json::from_str::<RunObservation>(&value))
        .transpose()?;
    if let Some(old) = &previous {
        if old.assignment_id != observation.assignment_id
            || old.thread_id != observation.thread_id
            || old.turn_id != observation.turn_id
            || old.input_ids != observation.input_ids
            || old.phase != RunPhase::Started
        {
            return Ok(false);
        }
    }
    let status = match &observation.phase {
        RunPhase::Started
            if previous.is_none()
                && observation.thread_id.is_some()
                && observation.turn_id.is_some() =>
        {
            "running"
        }
        RunPhase::Stopped {
            outcome: RunOutcome::Completed,
            waiting_for_children: true,
            ..
        } if previous.is_some() => "waiting",
        RunPhase::Stopped {
            outcome: RunOutcome::Completed,
            ..
        } if previous.is_some() => "completed",
        RunPhase::Stopped {
            outcome: RunOutcome::Failed,
            ..
        } => "failed",
        _ => return Ok(false),
    };
    let expected = if previous.is_some() {
        "running"
    } else if observation.input_ids.is_empty() {
        "accepted"
    } else {
        "waiting"
    };
    let updated = db.execute(
        "UPDATE tasks SET status = ?2 WHERE id = ?1 AND status = ?3",
        params![observation.task_id, status, expected],
    )?;
    if updated == 0 {
        return Ok(false);
    }
    db.execute(
        "INSERT INTO task_runs (id, assignment_id, task_id, observation) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(id) DO UPDATE SET observation = excluded.observation",
        params![
            observation.run_id,
            observation.assignment_id,
            observation.task_id,
            original
        ],
    )?;
    for input in &observation.input_ids {
        if db.execute(
            "UPDATE issue_inputs SET run_id = ?2 WHERE id = ?1 AND assignment_id = ?3
             AND (run_id IS NULL OR run_id = ?2)",
            params![input, observation.run_id, observation.assignment_id],
        )? != 1
        {
            return Ok(false);
        }
    }
    db.execute(
        "INSERT INTO runner_run_events (id, runner_id, observation) VALUES (?1, ?2, ?3)",
        params![observation.event_id, runner, original],
    )?;
    super::coordination::report_task_stop(db, observation)?;
    Ok(true)
}
