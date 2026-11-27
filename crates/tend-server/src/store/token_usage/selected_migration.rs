//! Schema 49 adds model-level quantities for already attributed requests.
//! This one-time read of the Server ledger needs no Runner or native file.
use super::{
    Summary, Tally, model_with_agent, models::ModelProjection, save_run_tally, system,
    update_rollups,
};
use anyhow::Result;
use protocol::RequestMeter;
use rusqlite::Connection;
use std::collections::BTreeMap;

#[derive(Default)]
pub(in crate::store) struct Selections {
    runs: BTreeMap<(String, String), ModelProjection>,
    sources: BTreeMap<String, ModelProjection>,
}

pub(in crate::store) fn capture(db: &Connection, with_system: bool) -> Result<Selections> {
    let mut result = Selections::default();
    let mut query = db.prepare(if with_system {
        "SELECT r.run_id, r.issue_id, q.last, a.meter
         FROM token_requests q JOIN token_runs r USING(run_id)
         LEFT JOIN token_agent_requests a ON a.thread=q.thread_id AND a.turn=q.turn_id
             AND a.response=q.response_id AND a.conflict IS NULL
         WHERE q.conflict IS NULL AND (a.meter IS NOT NULL OR json_extract(q.last, '$.model.source') = 'turn_context')"
    } else {
        "SELECT r.run_id, r.issue_id, q.last, NULL
         FROM token_requests q JOIN token_runs r USING(run_id)
         WHERE q.conflict IS NULL AND json_extract(q.last, '$.model.source') = 'turn_context'"
    })?;
    let mut rows = query.query([])?;
    while let Some(row) = rows.next()? {
        let meter: RequestMeter = serde_json::from_str(row.get_ref(2)?.as_str()?)?;
        let agent = row
            .get::<_, Option<String>>(3)?
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?;
        let projected = model_with_agent(&meter, agent.as_ref());
        let entry = result.runs.entry((row.get(0)?, row.get(1)?)).or_default();
        // Restore the selection bucket absent in the old shape, then
        // replace the Issue-only model with the combined projection.
        entry.migrate_selection(&meter);
        if meter.model != projected.model {
            entry.count(&meter, -1);
            entry.count(&projected, 1);
        }
    }
    if with_system {
        let mut query = db.prepare(
            "SELECT t.source, q.meter
             FROM token_agent_requests q JOIN token_agent_turns t USING(thread, turn)
             WHERE q.conflict IS NULL AND q.shadowed = 0
               AND json_extract(q.meter, '$.model.source') = 'turn_context'",
        )?;
        let mut rows = query.query([])?;
        while let Some(row) = rows.next()? {
            let meter = serde_json::from_str(row.get_ref(1)?.as_str()?)?;
            result
                .sources
                .entry(row.get(0)?)
                .or_default()
                .migrate_selection(&meter);
        }
    }
    Ok(result)
}

impl Selections {
    pub(in crate::store) fn publish(self, db: &Connection) -> Result<()> {
        let mut direct = BTreeMap::<String, Summary>::new();
        let mut query = db.prepare("SELECT tally FROM token_run_tallies WHERE run_id = ?1")?;
        for ((run, issue), extra) in self.runs {
            if extra == ModelProjection::default() {
                continue;
            }
            let mut tally: Tally = query.query_row([&run], |row| {
                Ok(serde_json::from_str(row.get_ref(0)?.as_str()?))
            })??;
            tally.model_usage.merge(&extra, 1);
            save_run_tally(db, &run, &tally)?;
            direct
                .entry(issue)
                .or_default()
                .tally
                .model_usage
                .merge(&extra, 1);
        }
        update_rollups(db, direct)?;
        for (source, extra) in self.sources {
            system::migrate_selections(db, &source, &extra)?;
        }
        Ok(())
    }
}
