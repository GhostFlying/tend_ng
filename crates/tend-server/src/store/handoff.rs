use super::Store;
use anyhow::Result;
use protocol::{HandoffPhase, HandoffProof, RunnerProfile};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::{Value, json};
use std::{ops::RangeInclusive, path::Path};

const HOST_DATA_POLICY: u8 = 1;

#[derive(serde::Serialize)]
pub struct Retirement {
    host_data_policy: u8,
    runner_id: String,
    host_id: String,
    successor: Option<String>,
    drained_at: Option<i64>,
    retired_at: Option<i64>,
    artifacts: Vec<DeliveryReceipt>,
}

#[derive(serde::Serialize)]
struct DeliveryReceipt {
    id: String,
    bytes: u64,
    available: bool,
}

pub fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(
        "ALTER TABLE runners ADD COLUMN protocol INTEGER NOT NULL DEFAULT 1;
         ALTER TABLE tasks ADD COLUMN handoff TEXT;
         CREATE TABLE runner_replacements (
            predecessor TEXT PRIMARY KEY REFERENCES runners(id),
            successor TEXT NOT NULL REFERENCES runners(id), drained_at INTEGER, retired_at INTEGER
         );
         CREATE INDEX replacement_successor ON runner_replacements(successor);
         CREATE TABLE handoff_roots (root_id TEXT PRIMARY KEY REFERENCES issues(id), runner_id TEXT NOT NULL REFERENCES runners(id));
         CREATE TABLE runner_handoff_proofs (id TEXT PRIMARY KEY, predecessor TEXT NOT NULL, proof TEXT NOT NULL);",
    )?;
    Ok(())
}

pub fn migrate_slot_budget(db: &Connection) -> Result<()> {
    db.execute_batch(
        "ALTER TABLE runner_replacements ADD COLUMN slot_budget INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE runner_replacements ADD COLUMN occupied_slots INTEGER NOT NULL DEFAULT 0;
         UPDATE runner_replacements SET slot_budget = coalesce((
             SELECT json_extract(profile, '$.slot_limit') FROM runners WHERE id = predecessor
         ), 0);
         UPDATE runner_replacements SET occupied_slots = slot_budget;
         CREATE VIEW runner_handoff_limits AS
           SELECT successor, min(slot_budget - occupied_slots) AS limit_slots
           FROM runner_replacements WHERE drained_at IS NULL AND retired_at IS NULL
           GROUP BY successor;",
    )?;
    Ok(())
}

pub fn note_drain_slots(db: &Connection, runner: &str, occupied: u32) -> Result<()> {
    // A fenced predecessor cannot reserve a new Run. Replayed or older
    // snapshots cannot take slots back from the successor.
    db.execute(
        "UPDATE runner_replacements SET occupied_slots = ?2
         WHERE predecessor = ?1 AND drained_at IS NULL AND occupied_slots > ?2",
        params![runner, occupied],
    )?;
    Ok(())
}

pub fn execution_slot_limit(db: &Connection, profile: &RunnerProfile) -> Result<Option<u32>> {
    if !profile.handoff_slots {
        return Ok(None);
    }
    let shared = db
        .query_row(
            "SELECT limit_slots FROM runner_handoff_limits WHERE successor = ?1",
            [&profile.runner_id],
            |row| row.get::<_, u32>(0),
        )
        .optional()?;
    Ok(Some(profile.slot_limit.min(shared.unwrap_or(u32::MAX))))
}

pub fn drain_acknowledged(db: &Connection, runner: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM runner_replacements WHERE predecessor = ?1 AND drained_at IS NOT NULL)",
        [runner],
        |row| row.get(0),
    )?)
}

pub fn drain_target(db: &Connection, runner: &str) -> Result<Option<String>> {
    Ok(db.query_row("SELECT successor FROM runner_replacements WHERE predecessor = ?1 AND retired_at IS NULL",
                    [runner], |row| row.get(0)).optional()?)
}

pub fn note_drained(db: &Connection, runner: &str) -> Result<()> {
    db.execute(
        "UPDATE runner_replacements SET drained_at = unixepoch()
         WHERE predecessor = ?1 AND drained_at IS NULL
           AND NOT EXISTS(SELECT 1 FROM artifacts WHERE runner_id = ?1 AND available = 0)
           AND NOT EXISTS(SELECT 1 FROM workspace_reclaims WHERE runner_id = ?1 AND receipt IS NULL)
           AND NOT EXISTS(SELECT 1 FROM runner_assignments WHERE runner_id = ?1 AND state = 'issued')
           AND NOT EXISTS(SELECT 1 FROM tasks t JOIN runner_assignments a ON a.id = t.current_assignment
                  WHERE a.runner_id = ?1 AND t.status NOT IN ('completed','failed','cancelled'))",
        [runner],
    )?;
    db.execute(
        "UPDATE handoff_roots SET runner_id = (SELECT successor FROM runner_replacements WHERE predecessor = ?1)
         WHERE runner_id = ?1 AND EXISTS(
             SELECT 1 FROM runner_replacements WHERE predecessor = ?1 AND drained_at IS NOT NULL
         )",
        [runner],
    )?;
    Ok(())
}

/// An accepted assignment can only move after its exact Runner supplied a
/// stop/handoff proof. The Issue tree can have moved more than once since a
/// completed review was last active.
pub(super) fn review_released_to(db: &Connection, task: &str, runner: &str) -> Result<bool> {
    Ok(db.query_row(
        "WITH RECURSIVE prior AS (
             SELECT id, runner_id FROM runner_assignments WHERE task_id = ?1 AND state = 'accepted'
             ORDER BY rowid DESC LIMIT 1
         ), route(id) AS (
             SELECT json_extract(p.proof, '$.successor') FROM prior a JOIN runner_handoff_proofs p
               ON p.predecessor = a.runner_id AND json_extract(p.proof, '$.assignment_id') = a.id
             UNION SELECT r.successor FROM runner_replacements r JOIN route p ON r.predecessor = p.id
         ) SELECT NOT EXISTS(SELECT 1 FROM prior WHERE runner_id != ?2)
               OR EXISTS(SELECT 1 FROM route WHERE id = ?2)",
        params![task, runner], |row| row.get(0),
    )?)
}

#[cfg(test)]
mod review_tests {
    use super::*;

    #[test]
    fn replacement_chain_does_not_substitute_for_the_exact_review_stop() -> Result<()> {
        let db = Connection::open_in_memory()?;
        db.execute_batch(
            "CREATE TABLE runner_assignments (id TEXT, task_id TEXT, runner_id TEXT, state TEXT);
             CREATE TABLE runner_handoff_proofs (predecessor TEXT, proof TEXT);
             CREATE TABLE runner_replacements (predecessor TEXT, successor TEXT);
             INSERT INTO runner_assignments VALUES ('first', 'review', 'prior', 'accepted');
             INSERT INTO runner_replacements VALUES ('prior','next'), ('next','later');
             INSERT INTO runner_handoff_proofs VALUES ('prior','{\"assignment_id\":\"unrelated\",\"successor\":\"next\"}');",
        )?;
        assert!(review_released_to(&db, "review", "prior")?);
        assert!(!review_released_to(&db, "review", "next")?);
        assert!(!review_released_to(&db, "review", "later")?);
        db.execute_batch(
            "INSERT INTO runner_handoff_proofs VALUES ('prior','{\"assignment_id\":\"first\",\"successor\":\"next\"}');",
        )?;
        assert!(review_released_to(&db, "review", "later")?);
        db.execute_batch(
            "INSERT INTO runner_assignments VALUES ('second', 'review', 'next', 'accepted');",
        )?;
        assert!(!review_released_to(&db, "review", "later")?);
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ProofReceipt {
    Accepted,
    AwaitingOriginals,
    Rejected,
}

fn verified_native_thread(db: &Connection, proof: &HandoffProof, contract: &str) -> Result<bool> {
    let Some(native) = proof.context.native_thread.as_ref() else {
        return Ok(false);
    };
    let assignment: protocol::TaskAssignment = serde_json::from_str(contract)?;
    if assignment.placement.host_id != native.execution.host_id
        || proof.context.workspace_stamp.as_ref() != Some(&native.execution.workspace)
        || native.thread_id.is_empty()
        || native.turn_id.is_empty()
    {
        return Ok(false);
    }
    let latest = db
        .query_row(
            "SELECT observation FROM task_runs WHERE task_id = ?1 AND assignment_id = ?2
         ORDER BY rowid DESC LIMIT 1",
            params![proof.task_id, proof.assignment_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let Some(latest) = latest else {
        // An intermediate generation which never launched can relay exactly
        // what the Server already placed in its accepted assignment.
        return Ok(assignment
            .handoff
            .as_ref()
            .and_then(|handoff| handoff.native_thread.as_ref())
            == Some(native));
    };
    let latest: protocol::RunObservation = serde_json::from_str(&latest)?;
    Ok(matches!(latest.phase, protocol::RunPhase::Stopped { .. })
        && latest.run_id == native.run_id
        && latest.thread_id.as_ref() == Some(&native.thread_id)
        && latest.turn_id.as_ref() == Some(&native.turn_id))
}

pub fn apply(db: &Connection, runner: &str, proof: &HandoffProof) -> Result<ProofReceipt> {
    use ProofReceipt::*;
    let raw = serde_json::to_string(proof)?;
    let prior = db
        .query_row(
            "SELECT predecessor, proof FROM runner_handoff_proofs WHERE id = ?1",
            [&proof.id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    if let Some((predecessor, saved)) = prior {
        if predecessor != runner {
            return Ok(Rejected);
        }
        if saved == raw {
            return Ok(Accepted);
        }
        // An older Server may have applied this exact proof but ignored the
        // optional native field. ACK it after an upgrade without upgrading
        // the already applied transfer or changing its successor.
        let saved: HandoffProof = serde_json::from_str(&saved)?;
        let mut comparable: HandoffProof = serde_json::from_str(&raw)?;
        if saved.context.native_thread.is_none() {
            comparable.context.native_thread = None;
        }
        return Ok(if saved == comparable {
            Accepted
        } else {
            Rejected
        });
    }
    if drain_target(db, runner)?.as_ref() != Some(&proof.successor) {
        return Ok(Rejected);
    }
    let original = db.query_row(
        "SELECT coalesce(i.parent_issue_id, i.id), t.status, t.current_assignment,
                json_extract(a.contract, '$.placement.agent.id'), a.contract
           FROM runner_assignments a JOIN tasks t ON t.id = a.task_id JOIN issues i ON i.id = t.issue_id
           WHERE a.id = ?1 AND a.runner_id = ?2 AND a.task_id = ?3 AND a.state = 'accepted'",
        params![proof.assignment_id, runner, proof.task_id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                  row.get::<_, Option<String>>(2)?, row.get::<_, String>(3)?, row.get::<_, String>(4)?)),
    ).optional()?;
    let Some((root, status, current, owner, contract)) = original else {
        return Ok(Rejected);
    };
    let is_current = current.as_deref() == Some(&proof.assignment_id);
    if status == "running" && is_current {
        return Ok(Rejected);
    }
    let next_review_round = is_current
        && (proof.context.phase == HandoffPhase::Finished || proof.context.native_thread.is_some())
        && db.query_row(
            "SELECT EXISTS(SELECT 1 FROM issue_inputs WHERE task_id = ?1 AND run_id IS NULL
                        AND json_extract(payload, '$.kind') = 'review_requested')",
            [&proof.task_id],
            |row| row.get::<_, bool>(0),
        )?;
    let requeue_finished_review =
        proof.context.phase == HandoffPhase::Finished && status == "waiting" && next_review_round;
    let transfer = is_current
        && !matches!(status.as_str(), "completed" | "failed" | "cancelled")
        && (proof.context.phase != HandoffPhase::Finished || requeue_finished_review);
    // An explicit next review round keeps the original Task and history,
    // but the cross-generation review contract does not carry its thread.
    let native_verified =
        transfer && !next_review_round && verified_native_thread(db, proof, &contract)?;
    let stopping = super::control::settle_proven_stops(db, runner, &proof.assignment_id)?;
    if !stopping
        && native_verified
        && db.query_row(
            "SELECT EXISTS(SELECT 1 FROM artifacts WHERE task_id = ?1 AND available = 0)",
            [&proof.task_id],
            |row| row.get::<_, bool>(0),
        )?
    {
        // Do not ACK the handoff or issue a new assignment. Still return the
        // upload requests in this sync so the original instance can finish.
        return Ok(AwaitingOriginals);
    }
    db.execute(
        "INSERT INTO handoff_roots VALUES (?1, ?2)
           ON CONFLICT(root_id) DO UPDATE SET runner_id = excluded.runner_id
           WHERE handoff_roots.runner_id = ?3",
        params![root, proof.successor, runner],
    )?;
    if !stopping && transfer {
        let mut context = serde_json::to_value(&proof.context)?;
        if requeue_finished_review {
            // The Server has a new round, but the predecessor had not yet
            // received its input when it stopped. The queued input follows.
            context["phase"] = json!(HandoffPhase::Queued);
        }
        if !native_verified && proof.context.native_thread.is_some() {
            context
                .as_object_mut()
                .expect("handoff context object")
                .remove("native_thread");
            let reason = if next_review_round {
                "explicit new review round"
            } else {
                "unverified origin"
            };
            eprintln!(
                "Omitting native thread binding for Task {} from assignment {}: {reason}",
                proof.task_id, proof.assignment_id
            );
        }
        db.execute(
            "UPDATE tasks SET current_assignment = NULL, status = 'awaiting_assignment', pinned_runner_id = ?2,
                              pinned_agent_id = ?3, handoff = ?4
             WHERE id = ?1 AND status NOT IN ('completed','failed','cancelled')",
            params![proof.task_id, proof.successor, owner, serde_json::to_string(&context)?],
        )?;
    }
    db.execute(
        "INSERT INTO runner_handoff_proofs VALUES (?1, ?2, ?3)",
        params![proof.id, runner, raw],
    )?;
    Ok(Accepted)
}

#[cfg(test)]
mod native_tests {
    use super::*;

    fn proof() -> Result<HandoffProof> {
        let workspace = json!({"path": "/workspace", "device": 1, "inode": 2});
        Ok(serde_json::from_value(json!({
            "id":"proof", "assignment_id":"original", "task_id":"task", "successor":"next",
            "context":{
                "phase":"waiting_external", "operations":[], "workspace_stamp":workspace,
                "native_thread":{
                    "run_id":"run", "thread_id":"thread", "turn_id":"turn",
                    "execution":{"host_id":"host", "workspace":workspace,
                        "codex_home":{"path":"/home", "device":1, "inode":3}, "compatibility":"v1:test"}
                }
            }
        }))?)
    }

    fn evidence() -> Result<Connection> {
        let db = Connection::open_in_memory()?;
        db.execute_batch(
            "CREATE TABLE task_runs (assignment_id TEXT, task_id TEXT, observation TEXT);
             CREATE TABLE runner_handoff_proofs (id TEXT, predecessor TEXT, proof TEXT);",
        )?;
        Ok(db)
    }

    #[test]
    fn only_latest_stopped_origin_or_exact_inherited_binding_can_transfer() -> Result<()> {
        let db = evidence()?;
        let proof = proof()?;
        let mut contract = json!({
            "placement":{"assignment_id":"original", "runner_id":"old", "host_id":"host",
                         "agent":{"id":"named", "category":"implementation", "revision":"1"}},
            "task_id":"task", "issue_id":"issue", "source_message_id":"source", "chat_id":"main",
            "work":{"title":"Fixture", "objective":"Fixture"}
        });
        assert!(!verified_native_thread(&db, &proof, &contract.to_string())?);
        contract["handoff"] = serde_json::to_value(&proof.context)?;
        assert!(verified_native_thread(&db, &proof, &contract.to_string())?);
        contract["handoff"]["native_thread"]["turn_id"] = json!("different");
        assert!(!verified_native_thread(&db, &proof, &contract.to_string())?);
        let mut observation = json!({
            "event_id":"event","assignment_id":"original","task_id":"task",
            "run_id":"run","thread_id":"thread","turn_id":"turn","phase":{"state":"started"}
        });
        let save = |raw: &Value| {
            db.execute(
                "INSERT INTO task_runs VALUES ('original', 'task', ?1)",
                [raw.to_string()],
            )
        };
        save(&observation)?;
        assert!(!verified_native_thread(&db, &proof, &contract.to_string())?);
        observation["phase"] = json!({"state":"stopped", "outcome":"completed"});
        save(&observation)?;
        assert!(verified_native_thread(&db, &proof, &contract.to_string())?);
        let mut wrong = proof;
        wrong
            .context
            .native_thread
            .as_mut()
            .unwrap()
            .execution
            .host_id = "other".into();
        assert!(!verified_native_thread(&db, &wrong, &contract.to_string())?);
        wrong
            .context
            .native_thread
            .as_mut()
            .unwrap()
            .execution
            .host_id = "host".into();
        observation["run_id"] = json!("newer");
        save(&observation)?;
        assert!(!verified_native_thread(&db, &wrong, &contract.to_string())?);
        Ok(())
    }

    #[test]
    fn lost_legacy_ack_is_accepted_without_mutating_the_original_proof() -> Result<()> {
        let db = evidence()?;
        let mut old = proof()?;
        old.context.native_thread = None;
        let saved = serde_json::to_string(&old)?;
        db.execute(
            "INSERT INTO runner_handoff_proofs VALUES ('proof', 'old', ?1)",
            [&saved],
        )?;
        let mut replay = proof()?;
        assert_eq!(apply(&db, "old", &replay)?, ProofReceipt::Accepted);
        assert_eq!(apply(&db, "other", &replay)?, ProofReceipt::Rejected);
        replay.successor = "different".into();
        assert_eq!(apply(&db, "old", &replay)?, ProofReceipt::Rejected);
        assert_eq!(
            db.query_row("SELECT proof FROM runner_handoff_proofs", [], |row| row
                .get::<_, String>(
                0
            ))?,
            saved
        );
        Ok(())
    }
}

impl Store {
    pub fn replace_runner(
        &mut self,
        old: &str,
        new: &str,
        predecessor_workspace_root: Option<&str>,
    ) -> Result<Option<Value>> {
        if old == new {
            return Ok(None);
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let profile = |id: &str| -> Result<Option<(RunnerProfile, u32)>> {
            let profile: Option<(String, u32)> = tx
                .query_row(
                    "SELECT profile, protocol FROM runners WHERE id = ?1 AND last_seen > unixepoch() - 15",
                    [id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            profile
                .map(|(raw, protocol)| Ok((serde_json::from_str(&raw)?, protocol)))
                .transpose()
        };
        let existing: Option<String> = tx
            .query_row(
                "SELECT successor FROM runner_replacements WHERE predecessor = ?1",
                [old],
                |row| row.get(0),
            )
            .optional()?;
        if existing.is_some() {
            return Ok((existing.as_deref() == Some(new))
                .then(|| json!({"predecessor":old,"successor":new})));
        }
        let (Some((predecessor, old_protocol)), Some((successor, new_protocol))) =
            (profile(old)?, profile(new)?)
        else {
            return Ok(None);
        };
        let workspace = predecessor
            .workspace_root
            .as_deref()
            .or(predecessor_workspace_root);
        if new_protocol < old_protocol
            || predecessor.host_id != successor.host_id
            || !workspace.is_some_and(|path| Path::new(path).is_absolute())
            || workspace != successor.workspace_root.as_deref()
            || !successor.direct_issue_owners && tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM handoff_roots h JOIN issues i ON i.id = h.root_id
                   JOIN tasks t ON t.issue_id = i.id
                   WHERE h.runner_id = ?1 AND t.workspace_key = i.id AND t.category != 'issue-router')
                 OR EXISTS(SELECT 1 FROM runner_assignments a JOIN tasks t ON t.current_assignment = a.id
                   JOIN issues i ON i.id = t.issue_id
                   WHERE a.runner_id = ?1 AND a.state IN ('issued', 'accepted')
                     AND i.parent_issue_id IS NULL AND t.workspace_key = i.id AND t.category != 'issue-router')",
                [old], |row| row.get::<_, bool>(0),
            )?
            || tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM runner_replacements
                     WHERE predecessor = ?2 OR successor = ?2 OR (successor = ?1 AND retired_at IS NULL))
                 OR EXISTS(SELECT 1 FROM runner_assignments a
                     WHERE a.runner_id = ?2 AND a.state IN ('issued','accepted'))
                 OR ?3 > 0",
                params![old, new, successor.running_slots],
                |row| row.get::<_, bool>(0),
            )?
        {
            return Ok(None);
        }
        tx.execute(
            "INSERT INTO runner_replacements(predecessor, successor, slot_budget, occupied_slots)
             VALUES (?1, ?2, ?3, ?3)",
            params![old, new, predecessor.slot_limit],
        )?;
        tx.execute(
            "UPDATE tasks SET current_assignment = NULL, status = 'awaiting_assignment'
             WHERE current_assignment IN (SELECT id FROM runner_assignments WHERE runner_id IN (?1, ?2) AND state = 'reserved')",
            params![old, new],
        )?;
        tx.execute("UPDATE runner_assignments SET state = 'revoked' WHERE runner_id IN (?1, ?2) AND state = 'reserved'", params![old, new])?;
        super::board::commit(tx)?;
        Ok(Some(json!({"predecessor":old,"successor":new})))
    }

    pub fn runner_instances(&self) -> Result<Vec<Value>> {
        let mut query = self.db.prepare(
            "SELECT r.profile, r.protocol, r.last_seen, p.successor, p.drained_at, p.retired_at,
                    r.last_seen > unixepoch() - ?1, l.limit_slots
             FROM runners r LEFT JOIN runner_replacements p ON p.predecessor = r.id
             LEFT JOIN runner_handoff_limits l ON l.successor = r.id ORDER BY r.host_id, r.id",
        )?;
        query.query_map([super::runner::FRESH_SECONDS], |row| Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?, row.get::<_, i64>(2)?,
            row.get::<_, Option<String>>(3)?, row.get::<_, Option<i64>>(4)?, row.get::<_, Option<i64>>(5)?,
            row.get::<_, bool>(6)?, row.get::<_, Option<u32>>(7)?)))?
            .map(|entry| {
                let (profile, protocol, last_seen, successor, drained_at, retired_at, online, shared) = entry?;
                let profile: Value = serde_json::from_str(&profile)?;
                let slot_limit = profile["slot_limit"].as_u64().unwrap_or(0);
                let execution_slot_limit = match shared {
                    Some(limit) if profile["handoff_slots"] == true => slot_limit.min(u64::from(limit)),
                    Some(_) => 0,
                    None => slot_limit,
                };
                Ok(json!({"profile":profile, "protocol":protocol, "host_data_policy":HOST_DATA_POLICY,
                          "last_seen":last_seen,"online":online,"successor":successor,"drained_at":drained_at,"retired_at":retired_at,
                          "execution_slot_limit":execution_slot_limit}))
            }).collect()
    }

    pub fn agent_directory(&self, router: &Value) -> Result<Value> {
        let tx = self.db.unchecked_transaction()?;
        let runners = self.runner_instances()?;
        let activity = {
            let mut query = tx.prepare(
                "SELECT a.runner_id, json_extract(a.contract, '$.placement.agent.id'),
                        json_extract(a.contract, '$.placement.agent.revision'), count(*)
                 FROM tasks t JOIN runner_assignments a ON a.id = t.current_assignment
                 WHERE t.status = 'running'
                 GROUP BY 1, 2, 3",
            )?;
            query.query_map([], |row| {
                Ok(json!({"runner_id": row.get::<_, String>(0)?, "agent_id": row.get::<_, String>(1)?,
                          "revision": row.get::<_, String>(2)?, "running": row.get::<_, u32>(3)?}))
            })?.collect::<rusqlite::Result<Vec<_>>>()?
        };
        tx.commit()?;
        Ok(json!({"router": router, "runners": runners, "activity": activity}))
    }

    pub fn runner_retirement(&self, runner: &str, root: &Path) -> Result<Option<Retirement>> {
        let Some(mut fact) = self
            .db
            .query_row(
                "SELECT r.host_id, p.successor, p.drained_at, p.retired_at FROM runners r
             LEFT JOIN runner_replacements p ON p.predecessor = r.id WHERE r.id = ?1",
                [runner],
                |row| {
                    Ok(Retirement {
                        host_data_policy: HOST_DATA_POLICY,
                        runner_id: runner.to_owned(),
                        host_id: row.get(0)?,
                        successor: row.get(1)?,
                        drained_at: row.get(2)?,
                        retired_at: row.get(3)?,
                        artifacts: Vec::new(),
                    })
                },
            )
            .optional()?
        else {
            return Ok(None);
        };
        let mut query = self.db.prepare(
            "SELECT id, bytes, available FROM artifacts WHERE runner_id = ?1 ORDER BY id",
        )?;
        fact.artifacts = query
            .query_map([runner], |row| {
                Ok(DeliveryReceipt {
                    id: row.get(0)?,
                    bytes: row.get(1)?,
                    available: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        for file in &mut fact.artifacts {
            if file.available {
                let path = root.join(&file.id);
                if !super::artifacts::complete_file(&path, file.bytes)? {
                    // Restore the durable upload request if a previously received file was lost.
                    self.mark_artifact_missing(&file.id, &path)?;
                    file.available = false;
                }
            }
        }
        Ok(Some(fact))
    }

    pub fn retire_runner(&self, runner: &str, root: &Path) -> Result<bool> {
        let Some(fact) = self.runner_retirement(runner, root)? else {
            return Ok(false);
        };
        if fact.retired_at.is_some() {
            return Ok(true);
        }
        if fact.drained_at.is_none() || fact.artifacts.iter().any(|file| !file.available) {
            return Ok(false);
        }
        Ok(self.db.execute(
            "UPDATE runner_replacements SET retired_at = coalesce(retired_at, unixepoch())
               WHERE predecessor = ?1 AND drained_at IS NOT NULL AND NOT EXISTS (
                   SELECT 1 FROM artifacts WHERE runner_id = ?1 AND available = 0
               )",
            [runner],
        )? == 1)
    }

    pub fn upgrade_blockers(&self, compatible: &RangeInclusive<u32>) -> Result<Vec<Value>> {
        let mut query = self.db.prepare(
            "SELECT r.id, r.host_id, r.protocol FROM runners r LEFT JOIN runner_replacements p ON p.predecessor = r.id
             WHERE p.retired_at IS NULL AND (r.protocol < ?1 OR r.protocol > ?2) ORDER BY r.id",
        )?;
        Ok(query
            .query_map(params![compatible.start(), compatible.end()], |row| {
                Ok(json!({
                    "runner_id": row.get::<_, String>(0)?,
                    "host_id": row.get::<_, String>(1)?,
                    "protocol": row.get::<_, u32>(2)?
                }))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;

    fn register(store: &Store, id: &str, slots: u32, standby: bool) -> Result<RunnerProfile> {
        let profile = RunnerProfile {
            runner_id: id.into(),
            host_id: "host".into(),
            repos: Vec::new(),
            agents: Vec::new(),
            slot_limit: slots,
            running_slots: 0,
            available_for_assignment: slots,
            inspect_url: None,
            workspace_root: Some("/workspace".into()),
            standby,
            shared_reviews: true,
            handoff_slots: true,
            codebase_review_comments: false,
            direct_issue_owners: false,
            dynamic_categories: false,
        };
        store.db.execute(
            "INSERT INTO runners (id, host_id, profile, last_seen, protocol)
             VALUES (?1, 'host', ?2, unixepoch(), 2)",
            params![id, serde_json::to_string(&profile)?],
        )?;
        Ok(profile)
    }

    #[test]
    fn retirement_requires_the_producers_received_originals() -> Result<()> {
        let store = Store {
            db: Connection::open_in_memory()?,
        };
        store.db.execute_batch(
            "CREATE TABLE runners(id TEXT PRIMARY KEY, host_id TEXT);
             CREATE TABLE runner_replacements(predecessor TEXT, successor TEXT, drained_at INTEGER, retired_at INTEGER);
             CREATE TABLE artifacts(id TEXT PRIMARY KEY, runner_id TEXT, bytes INTEGER, available INTEGER);
             INSERT INTO runners VALUES ('old','host'), ('new','host');
             INSERT INTO runner_replacements VALUES ('old','new',1,NULL);
             INSERT INTO artifacts VALUES ('report','old',5,1), ('unrelated','new',20,0);",
        )?;
        let root = std::env::temp_dir().join(protocol::new_id());
        std::fs::create_dir(&root)?;
        std::fs::write(root.join("report"), b"bad")?;
        assert!(store.runner_retirement("unknown", &root)?.is_none());
        let fact = store.runner_retirement("old", &root)?.unwrap();
        assert_eq!(fact.artifacts.len(), 1);
        assert_eq!(fact.artifacts[0].id, "report");
        assert!(!fact.artifacts[0].available);
        assert!(!store.retire_runner("old", &root)?);
        assert_eq!(
            store.db.query_row(
                "SELECT available FROM artifacts WHERE id='report'",
                [],
                |r| r.get::<_, i32>(0)
            )?,
            0
        );
        std::fs::write(root.join("report"), b"fixed")?;
        store
            .db
            .execute("UPDATE artifacts SET available=1 WHERE id='report'", [])?;
        assert!(store.retire_runner("old", &root)?);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn budget_is_frozen_and_released_monotonically_until_drain() -> Result<()> {
        let mut store = Store::initialize(Path::new(":memory:"))?;
        register(&store, "old", 3, false)?;
        let new = register(&store, "new", 5, true)?;
        register(&store, "third", 5, true)?;
        assert!(store.replace_runner("old", "new", None)?.is_some());
        assert!(store.replace_runner("old", "new", None)?.is_some());
        assert!(store.replace_runner("old", "third", None)?.is_none());
        assert!(store.replace_runner("new", "third", None)?.is_none());
        assert!(store.replace_runner("third", "new", None)?.is_none());
        assert!(!store.retire_runner("old", Path::new("."))?);
        assert_eq!(execution_slot_limit(&store.db, &new)?, Some(0));
        store.db.execute(
            "UPDATE runners SET profile = json_set(profile,'$.slot_limit',12) WHERE id = 'old'",
            [],
        )?;
        note_drain_slots(&store.db, "old", 1)?;
        assert_eq!(execution_slot_limit(&store.db, &new)?, Some(2));
        note_drain_slots(&store.db, "old", 2)?;
        assert_eq!(execution_slot_limit(&store.db, &new)?, Some(2));
        note_drain_slots(&store.db, "old", 0)?;
        assert_eq!(execution_slot_limit(&store.db, &new)?, Some(3));
        assert!(!drain_acknowledged(&store.db, "old")?);
        note_drained(&store.db, "old")?;
        assert_eq!(execution_slot_limit(&store.db, &new)?, Some(5));
        assert!(drain_acknowledged(&store.db, "old")?);
        assert!(store.retire_runner("old", Path::new("."))?);
        assert!(store.retire_runner("old", Path::new("."))?);
        assert!(store.replace_runner("old", "new", None)?.is_some());
        assert!(store.replace_runner("new", "third", None)?.is_some());
        Ok(())
    }

    #[test]
    fn legacy_budget_migration_reserves_unknown_occupancy() -> Result<()> {
        let db = Connection::open_in_memory()?;
        db.execute_batch(
            "CREATE TABLE runners(id TEXT, profile TEXT);
             CREATE TABLE runner_replacements(predecessor TEXT, successor TEXT, drained_at INTEGER, retired_at INTEGER);
             INSERT INTO runners VALUES ('old', '{\"slot_limit\":3}'), ('prior', '{\"slot_limit\":4}');
             INSERT INTO runner_replacements VALUES ('old','new',NULL,NULL), ('prior','old',1,2);",
        )?;
        migrate_slot_budget(&db)?;
        assert_eq!(
            db.query_row(
                "SELECT slot_budget, occupied_slots FROM runner_replacements WHERE predecessor = 'old'",
                [],
                |row| Ok((row.get::<_, u32>(0)?, row.get::<_, u32>(1)?)),
            )?,
            (3, 3)
        );
        assert_eq!(
            db.query_row(
                "SELECT successor, limit_slots FROM runner_handoff_limits",
                [],
                |row| { Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?)) }
            )?,
            ("new".to_owned(), 0)
        );
        Ok(())
    }
}
