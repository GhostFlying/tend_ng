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
        if source != runner {
            return Ok(false);
        }
        if prior == original {
            return Ok(true);
        }
        return compatible_start_receipt(&prior, &original);
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
            outcome,
            waiting_for_children: true,
            ..
        }
        | RunPhase::Stopped {
            outcome,
            waiting_for_attention: true,
            ..
        }
        | RunPhase::Stopped {
            outcome,
            waiting_for_external: true,
            ..
        }
        | RunPhase::Stopped {
            outcome,
            queued_input: true,
            ..
        } if previous.is_some() || *outcome == RunOutcome::Failed => "waiting",
        RunPhase::Stopped {
            outcome: RunOutcome::Completed,
            ..
        } if previous.is_some() => "completed",
        RunPhase::Stopped {
            outcome: RunOutcome::Failed,
            ..
        } => "failed",
        RunPhase::Stopped {
            outcome: RunOutcome::Cancelled,
            ..
        } => "cancelled",
        _ => return Ok(false),
    };
    let updated = db.execute(
        "UPDATE tasks SET status = ?2 WHERE id = ?1 AND (
            (?3 AND status = 'running') OR
            (NOT ?3 AND (status = 'accepted' OR (?4 AND status = 'waiting')))
         )",
        params![
            observation.task_id,
            status,
            previous.is_some(),
            !observation.input_ids.is_empty()
        ],
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
            "UPDATE issue_inputs SET run_id = ?2 WHERE id = ?1 AND task_id = ?3
             AND (run_id IS NULL OR run_id = ?2)",
            params![input, observation.run_id, observation.task_id],
        )? != 1
        {
            return Ok(false);
        }
    }
    if observation.phase == RunPhase::Started {
        super::issue_chat::record_run(db, &observation.run_id)?;
    }
    db.execute(
        "INSERT INTO runner_run_events (id, runner_id, observation) VALUES (?1, ?2, ?3)",
        params![observation.event_id, runner, original],
    )?;
    if matches!(status, "completed" | "failed") && observation.turn_id.is_some() {
        super::discovery::queue_automatic(db, observation)?;
    }
    super::coordination::report_task_stop(db, observation)?;
    Ok(true)
}

// An old Server or Runner omits the optional clock evidence on replay.
fn compatible_start_receipt(prior: &str, supplied: &str) -> Result<bool> {
    let mut prior: RunObservation = serde_json::from_str(prior)?;
    let mut supplied: RunObservation = serde_json::from_str(supplied)?;
    if prior.phase != RunPhase::Started
        || supplied.phase != RunPhase::Started
        || prior.started_at.is_some() == supplied.started_at.is_some()
    {
        return Ok(false);
    }
    prior.started_at = None;
    supplied.started_at = None;
    Ok(prior == supplied)
}

pub(crate) struct RunReader {
    pub origin: String,
    pub reader: String,
    pub url: Option<String>,
    pub native: protocol::NativeTurnRef,
}

impl super::Store {
    pub fn run_origin(&self, run: &str) -> Result<Option<RunReader>> {
        run_reader(&self.db, run)
    }
}

fn run_reader(db: &Connection, run: &str) -> Result<Option<RunReader>> {
    Ok(db.query_row(
        "WITH RECURSIVE readers(origin, reader, host) AS (
             SELECT a.runner_id, r.id, r.host_id
             FROM task_runs t JOIN runner_assignments a ON a.id = t.assignment_id
             JOIN runners r ON r.id = a.runner_id WHERE t.id = ?1
             UNION
             SELECT readers.origin, r.id, r.host_id FROM readers
             JOIN runner_replacements p ON p.predecessor = readers.reader AND p.retired_at IS NOT NULL
             JOIN runners r ON r.id = p.successor AND r.host_id = readers.host
         )
         SELECT readers.origin, readers.reader, json_extract(r.profile, '$.inspect_url'),
                json_extract(t.observation, '$.thread_id'), json_extract(t.observation, '$.turn_id')
         FROM readers JOIN runners r ON r.id = readers.reader JOIN task_runs t ON t.id = ?1
         WHERE NOT EXISTS(SELECT 1 FROM runner_replacements p WHERE p.predecessor = r.id AND p.retired_at IS NOT NULL)
           AND json_extract(t.observation, '$.thread_id') IS NOT NULL
           AND json_extract(t.observation, '$.turn_id') IS NOT NULL",
        [run],
        |row| Ok(RunReader {
            origin: row.get(0)?, reader: row.get(1)?, url: row.get(2)?,
            native: protocol::NativeTurnRef { thread_id: row.get(3)?, turn_id: row.get(4)? },
        }),
    ).optional()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optional_start_clock_does_not_change_an_existing_receipt_identity() -> Result<()> {
        let mut run = RunObservation {
            event_id: "event".to_owned(),
            assignment_id: "assignment".to_owned(),
            task_id: "task".to_owned(),
            run_id: "run".to_owned(),
            thread_id: Some("thread".to_owned()),
            turn_id: Some("turn".to_owned()),
            input_ids: vec![],
            branches: vec![],
            started_at: None,
            phase: RunPhase::Started,
        };
        let legacy = serde_json::to_string(&run)?;
        assert!(!legacy.contains("started_at"));
        run.started_at = Some(10);
        let current = serde_json::to_string(&run)?;
        assert!(compatible_start_receipt(&legacy, &current)?);
        assert!(compatible_start_receipt(&current, &legacy)?);
        run.started_at = Some(11);
        assert!(!compatible_start_receipt(
            &current,
            &serde_json::to_string(&run)?
        )?);
        run.task_id = "another-task".to_owned();
        assert!(!compatible_start_receipt(
            &legacy,
            &serde_json::to_string(&run)?
        )?);
        Ok(())
    }

    #[test]
    fn retired_run_reader_follows_only_confirmed_same_host_replacements() -> Result<()> {
        let db = Connection::open_in_memory()?;
        db.execute_batch(
            r#"CREATE TABLE runners(id TEXT PRIMARY KEY,host_id TEXT,profile TEXT);
            CREATE TABLE runner_replacements(predecessor TEXT PRIMARY KEY,successor TEXT,retired_at INTEGER);
            CREATE TABLE runner_assignments(id TEXT PRIMARY KEY,runner_id TEXT);
            CREATE TABLE task_runs(id TEXT PRIMARY KEY,assignment_id TEXT,observation TEXT);
            INSERT INTO runners VALUES('old','dev','{"inspect_url":"http://a"}'),
                                      ('middle','dev','{"inspect_url":"http://b"}'),
                                      ('new','dev','{"inspect_url":"http://c"}');
            INSERT INTO runner_assignments VALUES('assignment','old');
            INSERT INTO task_runs VALUES('run','assignment','{"thread_id":"thread","turn_id":"turn"}');
            INSERT INTO runner_replacements VALUES('old','middle',1),('middle','new',NULL);"#,
        )?;
        let during = run_reader(&db, "run")?.unwrap();
        assert_eq!(during.reader, "middle");
        db.execute(
            "UPDATE runner_replacements SET retired_at=2 WHERE predecessor='middle'",
            [],
        )?;
        let after = run_reader(&db, "run")?.unwrap();
        assert_eq!(
            (
                after.origin.as_str(),
                after.reader.as_str(),
                after.url.as_deref()
            ),
            ("old", "new", Some("http://c"))
        );
        assert_eq!(
            (
                after.native.thread_id.as_str(),
                after.native.turn_id.as_str()
            ),
            ("thread", "turn")
        );
        db.execute("UPDATE runners SET host_id='other' WHERE id='new'", [])?;
        assert!(run_reader(&db, "run")?.is_none());
        Ok(())
    }
}
