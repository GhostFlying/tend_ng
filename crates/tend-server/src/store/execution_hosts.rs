use anyhow::Result;
use rusqlite::{Connection, Row, params};
use serde::Serialize;
use std::collections::BTreeMap;

const CURRENT_RUNS: &str = "
WITH RECURSIVE running AS MATERIALIZED (
    SELECT record.id, record.task_id, a.runner_id, t.category, t.issue_id,
           json_extract(a.contract, '$.placement.agent.id') AS agent,
           json_extract(record.observation, '$.started_at') AS started_at
    FROM task_runs record
    JOIN tasks t ON t.id = record.task_id AND t.current_assignment = record.assignment_id
    JOIN runner_assignments a ON a.id = record.assignment_id AND a.state = 'accepted'
    JOIN runners r ON r.id = a.runner_id
    LEFT JOIN runner_replacements p ON p.predecessor = r.id
    WHERE json_extract(record.observation, '$.phase.state') = 'started'
      AND t.status = 'running' AND p.retired_at IS NULL AND p.drained_at IS NULL
      AND r.last_seen > ?1 AND json_extract(r.profile, '$.running_slots') > 0
), ancestors(run_id, issue_id, parent_id) AS (
    SELECT run.id, i.id, i.parent_issue_id FROM running run JOIN issues i ON i.id = run.issue_id
    UNION ALL
    SELECT path.run_id, i.id, i.parent_issue_id
    FROM ancestors path JOIN issues i ON i.id = path.parent_id
)
SELECT run.runner_id, run.id, run.task_id, run.category, run.agent, run.started_at,
       child.id, child.title, child_number.number, root.id, root.title, root_number.number
FROM running run
JOIN issues child ON child.id = run.issue_id
JOIN ancestors path ON path.run_id = run.id AND path.parent_id IS NULL
JOIN issues root ON root.id = path.issue_id
LEFT JOIN issue_short_numbers child_number ON child_number.issue_id = child.id
LEFT JOIN issue_short_numbers root_number ON root_number.issue_id = root.id
ORDER BY root_number.number, child_number.number, run.started_at, run.id
";

pub(super) fn install_index(db: &Connection) -> Result<()> {
    // A scan is bounded by outstanding Started evidence, not the Run history.
    db.execute_batch(
        "CREATE INDEX IF NOT EXISTS task_runs_active ON task_runs(task_id)
         WHERE json_extract(observation, '$.phase.state') = 'started';",
    )?;
    Ok(())
}

#[derive(Serialize)]
pub(crate) struct ExecutionHosts {
    observed_at: i64,
    hosts: Vec<Host>,
}

#[derive(Serialize, Default)]
struct Host {
    host_id: String,
    running_slots: u64,
    slot_limit: u64,
    runners: Vec<Runner>,
}

#[derive(Serialize)]
struct Runner {
    runner_id: String,
    online: bool,
    mode: Mode,
    running_slots: Option<u32>,
    slot_limit: u32,
    runs: Vec<Running>,
}

#[derive(Serialize, Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "snake_case")]
enum Mode {
    Serving,
    Draining,
    Standby,
    Drained,
}

#[derive(Serialize)]
struct Running {
    run_id: String,
    task_id: String,
    category: String,
    agent_id: String,
    started_at: Option<i64>,
    issue: Issue,
    root_issue: Issue,
}

#[derive(Serialize)]
struct Issue {
    id: String,
    title: String,
    short_number: Option<u64>,
}

fn issue(row: &Row<'_>, index: usize) -> rusqlite::Result<Issue> {
    Ok(Issue {
        id: row.get(index)?,
        title: row.get(index + 1)?,
        short_number: row.get(index + 2)?,
    })
}

fn running(db: &Connection, cutoff: i64) -> Result<BTreeMap<String, Vec<Running>>> {
    let mut statement = db.prepare_cached(CURRENT_RUNS)?;
    let rows = statement.query_map([cutoff], |row| {
        Ok((
            row.get::<_, String>(0)?,
            Running {
                run_id: row.get(1)?,
                task_id: row.get(2)?,
                category: row.get(3)?,
                agent_id: row.get(4)?,
                started_at: row.get(5)?,
                issue: issue(row, 6)?,
                root_issue: issue(row, 9)?,
            },
        ))
    })?;
    let mut result = BTreeMap::<_, Vec<_>>::new();
    for row in rows {
        let (runner, data) = row?;
        result.entry(runner).or_default().push(data);
    }
    Ok(result)
}

impl super::Store {
    pub fn execution_hosts(&self) -> Result<ExecutionHosts> {
        let tx = self.db.unchecked_transaction()?;
        let observed_at: i64 = tx.query_row("SELECT unixepoch()", [], |row| row.get(0))?;
        let cutoff = observed_at - super::runner::FRESH_SECONDS;
        let mut runs = running(&tx, cutoff)?;
        let mut by_host = BTreeMap::<_, Host>::new();
        {
            let mut query = tx.prepare_cached(
                "SELECT r.host_id, r.id, r.last_seen > ?1,
                        json_extract(r.profile, '$.running_slots'),
                        json_extract(r.profile, '$.slot_limit'),
                        CASE WHEN p.drained_at IS NOT NULL THEN 3
                             WHEN p.successor IS NOT NULL THEN 1
                             WHEN coalesce(json_extract(r.profile,'$.standby'), 0) = 1
                               AND NOT EXISTS (SELECT 1 FROM runner_replacements
                                               WHERE successor = r.id) THEN 2
                             ELSE 0 END
                 FROM runners r LEFT JOIN runner_replacements p ON p.predecessor = r.id
                 WHERE p.retired_at IS NULL",
            )?;
            let roster = query.query_map(params![cutoff], |row| {
                let mode = match row.get::<_, u8>(5)? {
                    1 => Mode::Draining,
                    2 => Mode::Standby,
                    3 => Mode::Drained,
                    _ => Mode::Serving,
                };
                let online: bool = row.get(2)?;
                let used: u32 = row.get(3)?;
                Ok((
                    row.get::<_, String>(0)?,
                    Runner {
                        runner_id: row.get(1)?,
                        online,
                        mode,
                        running_slots: online.then_some(if mode == Mode::Drained {
                            0
                        } else {
                            used
                        }),
                        slot_limit: row.get(4)?,
                        runs: Vec::new(),
                    },
                ))
            })?;
            for row in roster {
                let (host, mut runner) = row?;
                runner.runs = runs.remove(&runner.runner_id).unwrap_or_default();
                let data = by_host.entry(host.clone()).or_insert_with(|| Host {
                    host_id: host,
                    ..Host::default()
                });
                data.running_slots += u64::from(runner.running_slots.unwrap_or(0));
                if runner.mode == Mode::Serving {
                    data.slot_limit += u64::from(runner.slot_limit);
                }
                data.runners.push(runner);
            }
        }
        tx.commit()?;
        let mut hosts: Vec<_> = by_host.into_values().collect();
        for host in &mut hosts {
            host.runners
                .sort_by(|a, b| (a.mode, &a.runner_id).cmp(&(b.mode, &b.runner_id)));
        }
        Ok(ExecutionHosts { observed_at, hosts })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn runner_activity_uses_the_active_partial_index_not_run_history() -> Result<()> {
        let store = crate::store::Store::initialize(Path::new(":memory:"))?;
        let query = format!("EXPLAIN QUERY PLAN {CURRENT_RUNS}");
        let mut plan = store.db.prepare(&query)?;
        let lines = plan
            .query_map([0], |row| row.get::<_, String>(3))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        assert!(
            lines.iter().any(|line| line.contains("task_runs_active")),
            "{lines:?}"
        );
        assert!(!lines.iter().any(|line| line == "SCAN record"), "{lines:?}");
        Ok(())
    }
}
