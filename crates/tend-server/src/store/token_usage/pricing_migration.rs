//! Schema 52 replaces only the derived model cache. Keep the raw ledgers,
//! run coverage, conflicts, and Issue ownership untouched.
use super::{model_with_agent, models::ModelProjection, system};
use anyhow::Result;
use protocol::RequestMeter;
use rusqlite::{Connection, params};
use std::collections::BTreeMap;

#[derive(Default)]
pub(in crate::store) struct Pricing {
    runs: BTreeMap<String, ModelProjection>,
    direct: BTreeMap<String, ModelProjection>,
    subtree: BTreeMap<String, ModelProjection>,
    global: ModelProjection,
    sources: BTreeMap<String, ModelProjection>,
}

pub(in crate::store) fn capture(db: &Connection) -> Result<Pricing> {
    let mut result = Pricing::default();
    let mut issue_runs = BTreeMap::new();
    let mut query = db.prepare(
        "SELECT r.run_id, r.issue_id, q.last, a.meter
         FROM token_requests q JOIN token_runs r USING(run_id)
         LEFT JOIN token_agent_requests a ON a.thread=q.thread_id AND a.turn=q.turn_id
             AND a.response=q.response_id AND a.conflict IS NULL
         WHERE q.conflict IS NULL",
    )?;
    let mut rows = query.query([])?;
    while let Some(row) = rows.next()? {
        let meter: RequestMeter = serde_json::from_str(row.get_ref(2)?.as_str()?)?;
        if !meter.valid() {
            continue;
        }
        let agent = row
            .get::<_, Option<String>>(3)?
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?;
        let run: String = row.get(0)?;
        issue_runs
            .entry(run.clone())
            .or_insert(row.get::<_, String>(1)?);
        result
            .runs
            .entry(run)
            .or_default()
            .count(&model_with_agent(&meter, agent.as_ref()), 1);
    }
    for (run, issue) in issue_runs {
        result
            .direct
            .entry(issue)
            .or_default()
            .merge(&result.runs[&run], 1);
    }
    let mut query = db.prepare("SELECT descendant, ancestor FROM token_issue_tree")?;
    let mut rows = query.query([])?;
    while let Some(row) = rows.next()? {
        if let Some(projection) = result.direct.get(row.get_ref(0)?.as_str()?) {
            result
                .subtree
                .entry(row.get(1)?)
                .or_default()
                .merge(projection, 1);
        }
    }
    for projection in result.direct.values() {
        result.global.merge(projection, 1);
    }
    let mut query = db.prepare(
        "SELECT t.source, q.meter, f.selected_model
         FROM token_agent_requests q JOIN token_agent_turns t USING(thread, turn)
         LEFT JOIN token_feishu_attempt_launch f ON f.attempt = t.attempt
         WHERE q.conflict IS NULL AND q.shadowed = 0",
    )?;
    let mut rows = query.query([])?;
    while let Some(row) = rows.next()? {
        let meter: RequestMeter = serde_json::from_str(row.get_ref(1)?.as_str()?)?;
        let selected: Option<String> = row.get(2)?;
        let projected = system::with_launch_model(&meter, selected.as_deref());
        result
            .sources
            .entry(row.get(0)?)
            .or_default()
            .count(projected.as_ref().unwrap_or(&meter), 1);
    }
    Ok(result)
}

impl Pricing {
    pub(in crate::store) fn publish(self, db: &Connection) -> Result<()> {
        // The old format's quantities are not compatible with new signed
        // deltas. Clear just that field, then install the recomputed cache.
        let empty = serde_json::to_string(&ModelProjection::default())?;
        db.execute("UPDATE token_run_tallies SET tally = json_set(tally, '$.model_usage', json(?1)) WHERE json_type(tally, '$.model_usage') IS NOT NULL", [&empty])?;
        db.execute("UPDATE token_issue_rollups SET summary = json_set(summary, '$.tally.model_usage', json(?1)) WHERE json_type(summary, '$.tally.model_usage') IS NOT NULL", [&empty])?;
        db.execute(
            "UPDATE token_global_rollup SET summary = json_set(summary, '$.tally.model_usage', json(?1))",
            [serde_json::to_string(&self.global)?],
        )?;
        let mut run = db.prepare("UPDATE token_run_tallies SET tally = json_set(tally, '$.model_usage', json(?2)) WHERE run_id = ?1")?;
        for (id, projection) in self.runs {
            run.execute(params![id, serde_json::to_string(&projection)?])?;
        }
        let mut issue = db.prepare("UPDATE token_issue_rollups SET summary = json_set(summary, '$.tally.model_usage', json(?3)) WHERE issue_id = ?1 AND scope = ?2")?;
        for (scope, entries) in [("direct", self.direct), ("subtree", self.subtree)] {
            for (id, projection) in entries {
                issue.execute(params![id, scope, serde_json::to_string(&projection)?])?;
            }
        }
        system::replace_model_projections(db, self.sources)?;
        Ok(())
    }
}
