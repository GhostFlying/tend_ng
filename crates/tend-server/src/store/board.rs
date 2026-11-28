use super::{
    PAGE_SIZE, Store, artifacts,
    attention::{LIVE_TARGET, ORDINARY_ROUTED},
    pull, reports,
    work::read_list,
};
use anyhow::Result;
use protocol::{
    BoardAttention, BoardStage, BoardWait, DeliverySource, IssueAttentionReply,
    IssueAttentionReplyPage, IssueBoard, IssueBoardCursor, IssueCard, IssueDecision, IssueOverview,
    IssueOverviewArtifact, IssueOverviewChild,
};
use rusqlite::{Connection, OptionalExtension, Row, params};
use std::collections::HashMap;

mod order;
#[cfg(test)]
pub(super) use order::remove_projection_schema;
pub(super) use order::{commit, install_tracking, migrate, refresh};

const TREE: &str =
    "WITH RECURSIVE tree(root, issue) AS (
         SELECT value, value FROM json_each(?1)
         UNION ALL
         SELECT tree.root, child.id FROM tree JOIN issues child ON child.parent_issue_id = tree.issue
     )";
const REPLY_PAGE_SIZE: usize = 5;

const CARD_FIELDS: &str = "i.id, i.parent_issue_id, i.title, i.state,
    COALESCE(t.status, 'backlog'),
    COALESCE(t.repo_ids, json_extract(d.request, '$.repo_ids'), '[]'), n.number";
const CARD_JOINS: &str = "FROM issues i JOIN chat_messages source ON source.seq = i.source_seq
    JOIN issue_short_numbers n ON n.issue_id = i.id
    LEFT JOIN tasks t ON t.rowid = (
        SELECT rowid FROM tasks WHERE issue_id = i.id ORDER BY rowid DESC LIMIT 1
    )
    LEFT JOIN router_dispatches d ON d.issue_id = i.id";

#[derive(Default)]
struct Progress {
    running: u32,
    parked: bool,
    // Earliest time any task can be ready (zero for untimed readiness).
    ready_at: Option<i64>,
    waiting: bool,
    external: bool,
    reply: bool,
    children: bool,
}

impl Progress {
    fn stage(&self, state: &str, task: &str, attention: bool, now: i64) -> BoardStage {
        match state {
            "closed" => BoardStage::Done,
            "cancelled" => BoardStage::Cancelled,
            _ if self.running > 0 => BoardStage::InProgress,
            _ if self.parked || task == "backlog" => BoardStage::Backlog,
            _ if self.ready_at.is_some_and(|at| at <= now) => BoardStage::Todo,
            _ if self.waiting || attention => BoardStage::Waiting,
            _ => BoardStage::Todo,
        }
    }

    fn reason(&self, issue: &IssueCard) -> BoardWait {
        if issue.attention.is_some() || self.reply {
            BoardWait::Reply
        } else if self.external {
            BoardWait::External
        } else if self.children {
            BoardWait::Children
        } else {
            BoardWait::Continuation
        }
    }
}

impl Store {
    pub fn issue_with_reports(&self, id: &str) -> Result<Option<serde_json::Value>> {
        let tx = self.db.unchecked_transaction()?;
        let Some(issue) = self.issue(id)? else {
            return Ok(None);
        };
        let mut issue = serde_json::to_value(issue)?;
        issue["reports"] = serde_json::to_value(reports::delivery_reports(&tx, id)?)?;
        tx.commit()?;
        Ok(Some(issue))
    }

    pub fn issue_overview(&self, id: &str) -> Result<Option<IssueOverview>> {
        let tx = self.db.unchecked_transaction()?;
        let Some((mut card, objective, chat_id, source_message_id)) = tx
            .query_row(
                &format!(
                    "SELECT {CARD_FIELDS}, i.objective, source.chat_id, source.id
                     {CARD_JOINS} WHERE i.id = ?1"
                ),
                [id],
                |row| {
                    Ok((
                        read_card(row)?,
                        row.get(7)?,
                        row.get::<_, String>(8)?,
                        row.get(9)?,
                    ))
                },
            )
            .optional()?
        else {
            return Ok(None);
        };
        // A new reservation can belong to a continuation or handoff. Only an
        // accepted assignment establishes or changes this Issue's named Owner.
        let owner_agent_id = super::work::accepted_owner(&tx, id)?.and_then(|(_, agent)| agent);
        let title_history = super::issue_title::history(&tx, id, i64::MAX)?;
        let title_history = (!title_history.changes.is_empty()).then_some(title_history);
        let roots = serde_json::to_string(&[id])?;
        enrich_cards(
            &tx,
            &roots,
            std::slice::from_mut(&mut card),
            true,
            order::snapshot_time(&tx)?,
        )?;
        let children = {
            let mut query = tx.prepare(
                "SELECT i.id, i.title, n.number FROM issues i JOIN issue_short_numbers n ON n.issue_id = i.id
                 WHERE i.parent_issue_id = ?1 ORDER BY i.rowid",
            )?;
            query
                .query_map([id], |row| {
                    Ok(IssueOverviewChild {
                        id: row.get(0)?,
                        title: row.get(1)?,
                        short_number: row.get(2)?,
                    })
                })?
                .collect::<rusqlite::Result<_>>()?
        };
        let files = {
            let mut query = tx.prepare(&format!(
                "{TREE}
                 SELECT a.id, a.name, a.bytes, a.available, CASE WHEN i.id != tree.root THEN i.title END,
                        i.id, i.title, n.number, a.task_id
                 FROM tree JOIN artifacts a ON a.issue_id = tree.issue
                 JOIN issues i ON i.id = a.issue_id
                 JOIN issue_short_numbers n ON n.issue_id = i.id
                 JOIN issues root ON root.id = tree.root AND root.source_seq = i.source_seq
                 ORDER BY a.rowid"
            ))?;
            query
                .query_map([&roots], |row| {
                    Ok(IssueOverviewArtifact {
                        artifact: artifacts::artifact(row)?,
                        issue_title: row.get(4)?,
                        source: Some(DeliverySource {
                            issue: IssueOverviewChild {
                                id: row.get(5)?,
                                title: row.get(6)?,
                                short_number: row.get(7)?,
                            },
                            task_id: row.get(8)?,
                        }),
                    })
                })?
                .collect::<rusqlite::Result<_>>()?
        };
        let reports = reports::delivery_reports(&tx, id)?;
        let proposals = super::proposals::overview(&tx, id)?;
        let source_chat_archived =
            Self::chat_kind(&tx, &chat_id)? == Some(protocol::ChatKind::Archived);
        let proposal_requires_chat = proposals
            .iter()
            .any(|proposal| proposal.status.decision.is_none())
            && super::proposals::requires_chat(&tx, id, source_chat_archived)?;
        let mut decisions: Vec<IssueDecision> = {
            let mut query = tx.prepare(&format!(
                "{TREE}
                 SELECT question.id, question.text,
                   CASE WHEN ({LIVE_TARGET}) AND ({ORDINARY_ROUTED})
                     AND question.chat_id = origin.chat_id
                   THEN a.id END
                 FROM tree JOIN attentions a ON a.issue_id = tree.issue
                 JOIN chat_messages question ON question.seq = a.question_seq
                 JOIN chat_messages origin ON origin.seq = a.source_seq
                 JOIN issues i ON i.id = a.issue_id
                 JOIN tasks t ON t.id = a.task_id AND t.issue_id = i.id
                 WHERE a.answer_input_id IS NULL AND question.chat_id = ?2
                   AND (a.issue_id != ?3 OR NOT EXISTS
                        (SELECT 1 FROM proposal_versions WHERE attention_id = a.id))
                 ORDER BY a.rowid"
            ))?;
            query
                .query_map(params![roots, chat_id, id], |row| {
                    let reply = match row.get(2)? {
                        Some(attention_id) => Some(IssueAttentionReply {
                            attention_id,
                            answer: None,
                        }),
                        None => None,
                    };
                    Ok(IssueDecision {
                        message_id: row.get(0)?,
                        text: row.get(1)?,
                        reply,
                    })
                })?
                .collect::<rusqlite::Result<_>>()?
        };
        let history = reply_history(&tx, &roots, &chat_id, i64::MAX)?;
        decisions.extend(history.decisions);
        tx.commit()?;
        Ok(Some(IssueOverview {
            id: card.id,
            short_number: card.short_number,
            title: card.title,
            stage: card.stage,
            owner_agent_id,
            title_history,
            objective,
            repo_ids: card.repo_ids,
            chat_id,
            source_chat_archived,
            source_message_id,
            children,
            pulls: card.pulls,
            artifacts: files,
            reports,
            decisions,
            decision_history_before: history.next_before,
            proposals,
            proposal_requires_chat,
        }))
    }

    pub fn issue_attention_history(
        &self,
        issue: &str,
        before: i64,
    ) -> Result<Option<IssueAttentionReplyPage>> {
        let tx = self.db.unchecked_transaction()?;
        let chat = tx.query_row(
            "SELECT source.chat_id FROM issues i JOIN chat_messages source ON source.seq = i.source_seq
             WHERE i.id = ?1",
            [issue],
            |row| row.get::<_, String>(0),
        ).optional()?;
        let result = chat
            .map(|chat| reply_history(&tx, &serde_json::to_string(&[issue])?, &chat, before))
            .transpose()?;
        tx.commit()?;
        Ok(result)
    }

    pub fn issue_board(
        &self,
        chat: Option<&str>,
        before: Option<&IssueBoardCursor>,
        roots_only: bool,
    ) -> Result<IssueBoard> {
        let tx = self.db.unchecked_transaction()?;
        let now = order::snapshot_time(&tx)?;
        let (before_ms, before_id) =
            before.map_or((i64::MAX, ""), |key| (key.entry_ms, key.issue_id.as_str()));
        let mut rows = {
            let mut query = tx.prepare_cached(&format!(
                "{}, selected AS MATERIALIZED (
                     SELECT id, entry_ms FROM keys ORDER BY entry_ms DESC, id DESC LIMIT ?6
                 )
                 SELECT {CARD_FIELDS}, selected.entry_ms
                 {CARD_JOINS} JOIN selected ON selected.id = i.id
                 ORDER BY selected.entry_ms DESC, selected.id DESC",
                order::BOARD_KEYS,
            ))?;
            query
                .query_map(
                    params![now, before_ms, before_id, roots_only, chat, PAGE_SIZE + 1],
                    |row| Ok((read_card(row)?, row.get::<_, i64>(7)?)),
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let next_before = if rows.len() > PAGE_SIZE {
            rows.pop();
            rows.last().map(|(card, entry_ms)| {
                IssueBoardCursor {
                    entry_ms: *entry_ms,
                    issue_id: card.id.clone(),
                }
                .encode()
            })
        } else {
            None
        };
        let mut page: Vec<_> = rows.into_iter().map(|(card, _)| card).collect();

        if !page.is_empty() {
            let roots = serde_json::to_string(
                &page.iter().map(|card| card.id.as_str()).collect::<Vec<_>>(),
            )?;
            enrich_cards(&tx, &roots, &mut page, roots_only, now)?;
        }
        tx.commit()?;
        Ok(IssueBoard {
            issues: page,
            next_before,
        })
    }
}

fn reply_history(
    db: &Connection,
    roots: &str,
    chat: &str,
    before: i64,
) -> Result<IssueAttentionReplyPage> {
    // Sample each owner's indexed stream before combining receipt identities.
    // Join the full bodies only after selecting the bounded tree-wide page.
    let mut statement = db.prepare(&format!(
        "{TREE}, selected(seq, attention, question) AS MATERIALIZED (
             SELECT direct.rowid, a.id, a.question_seq
             FROM tree JOIN issue_attention_replies direct ON direct.rowid IN (
                 SELECT rowid FROM issue_attention_replies
                 WHERE owner_issue_id = tree.issue AND rowid < ?3
                 ORDER BY rowid DESC LIMIT ?4
             )
             JOIN attentions a ON a.id = direct.attention_id
             JOIN chat_messages q ON q.seq = a.question_seq
             WHERE q.chat_id = ?2
             ORDER BY direct.rowid DESC LIMIT ?4
         )
         SELECT selected.seq, question.id, question.text, a.id, a.answer
         FROM selected JOIN attentions a ON a.id = selected.attention
         JOIN chat_messages question ON question.seq = selected.question
         ORDER BY selected.seq DESC"
    ))?;
    let mut rows = statement.query(params![roots, chat, before, REPLY_PAGE_SIZE + 1])?;
    let mut decisions = Vec::new();
    let mut last = None;
    while let Some(row) = rows.next()? {
        if decisions.len() == REPLY_PAGE_SIZE {
            return Ok(IssueAttentionReplyPage {
                decisions,
                next_before: last,
            });
        }
        last = Some(row.get(0)?);
        decisions.push(IssueDecision {
            message_id: row.get(1)?,
            text: row.get(2)?,
            reply: Some(IssueAttentionReply {
                attention_id: row.get(3)?,
                answer: row.get(4)?,
            }),
        });
    }
    Ok(IssueAttentionReplyPage {
        decisions,
        next_before: None,
    })
}

fn read_card(row: &Row<'_>) -> rusqlite::Result<IssueCard> {
    Ok(IssueCard {
        id: row.get(0)?,
        parent_issue_id: row.get(1)?,
        title: row.get(2)?,
        issue_state: row.get(3)?,
        task_state: row.get(4)?,
        repo_ids: read_list(row, 5)?,
        short_number: row.get(6)?,
        stage: BoardStage::default(),
        running: 0,
        waiting_for: None,
        attention: None,
        pulls: Vec::new(),
    })
}

fn enrich_cards(
    db: &Connection,
    roots: &str,
    page: &mut [IssueCard],
    aggregate_pulls: bool,
    now: i64,
) -> Result<()> {
    let mut progress = read_progress(db, roots)?;
    let mut attentions = read_attentions(db, roots)?;
    for card in &mut *page {
        let progress = progress.remove(&card.id).unwrap_or_default();
        card.attention = attentions.remove(&card.id);
        card.running = progress.running;
        card.stage = progress.stage(
            &card.issue_state,
            &card.task_state,
            card.attention.is_some(),
            now,
        );
        if matches!(card.stage, BoardStage::Waiting) {
            card.waiting_for = Some(progress.reason(card));
        }
    }
    attach_pulls(db, roots, page, aggregate_pulls)
}

fn read_progress(db: &Connection, roots: &str) -> Result<HashMap<String, Progress>> {
    let mut query = db.prepare_cached(&format!(
        "{TREE}, work AS (
             SELECT tree.root, t.id, t.status, COALESCE(t.workspace_key, i.parent_issue_id) AS workspace,
                    EXISTS(SELECT 1 FROM issue_parks p WHERE p.issue_id = i.id) AS parked,
                    COALESCE(json_extract(r.observation, '$.phase.waiting_for_external'), 0) AS external,
                    COALESCE(json_extract(r.observation, '$.phase.waiting_for_attention'), 0) AS reply,
                    COALESCE(json_extract(r.observation, '$.phase.waiting_for_children'), 0) AS children,
                    COALESCE(json_extract(r.observation, '$.phase.queued_input'), 0) AS queued
             FROM tree JOIN issues i ON i.id = tree.issue JOIN tasks t ON t.issue_id = tree.issue
             LEFT JOIN task_runs r ON t.status = 'waiting' AND r.rowid = (
                 SELECT rowid FROM task_runs WHERE task_id = t.id ORDER BY rowid DESC LIMIT 1
             )
         )
         SELECT w.root, SUM(w.status = 'running'),
                MIN(CASE WHEN w.status IN ('awaiting_assignment','awaiting_receipt','accepted','waiting')
                    AND NOT w.parked
                    AND NOT EXISTS (
                        SELECT 1 FROM work holder WHERE holder.root = w.root AND holder.id != w.id
                         AND holder.workspace = w.workspace AND holder.status = 'waiting' AND holder.external
                    ) THEN CASE WHEN w.status != 'waiting' OR w.queued THEN 0 ELSE (
                        SELECT COALESCE(wake_at, 0) FROM issue_inputs
                        WHERE task_id = w.id AND run_id IS NULL ORDER BY wake_at LIMIT 1
                    ) END END),
                MAX(w.status = 'waiting' AND NOT w.parked),
                MAX(w.external AND NOT w.parked), MAX(w.reply AND NOT w.parked), MAX(w.children AND NOT w.parked),
                EXISTS(SELECT 1 FROM issue_parks p WHERE p.issue_id = w.root)
         FROM work w GROUP BY w.root"
    ))?;
    Ok(query
        .query_map([roots], |row| {
            Ok((
                row.get(0)?,
                Progress {
                    running: row.get(1)?,
                    ready_at: row.get(2)?,
                    waiting: row.get(3)?,
                    external: row.get(4)?,
                    reply: row.get(5)?,
                    children: row.get(6)?,
                    parked: row.get(7)?,
                },
            ))
        })?
        .collect::<rusqlite::Result<HashMap<_, _>>>()?)
}

fn read_attentions(db: &Connection, roots: &str) -> Result<HashMap<String, BoardAttention>> {
    let mut query = db.prepare_cached(&format!(
        "{TREE}
         SELECT tree.root, m.chat_id, m.id FROM tree JOIN attentions a ON a.issue_id = tree.issue
         JOIN chat_messages m ON m.seq = a.question_seq
         WHERE a.question_seq IS NOT NULL AND a.answer_input_id IS NULL
         ORDER BY a.rowid"
    ))?;
    let mut rows = query.query([roots])?;
    let mut result = HashMap::new();
    while let Some(row) = rows.next()? {
        result.entry(row.get(0)?).or_insert(BoardAttention {
            chat_id: row.get(1)?,
            question_message_id: row.get(2)?,
        });
    }
    Ok(result)
}

fn attach_pulls(
    db: &Connection,
    roots: &str,
    page: &mut [IssueCard],
    roots_only: bool,
) -> Result<()> {
    let mut indices: HashMap<_, _> = page
        .iter()
        .enumerate()
        .map(|(index, card)| (card.id.clone(), index))
        .collect();
    if roots_only {
        let mut query = db.prepare_cached(&format!(
            "{TREE} SELECT root, issue FROM tree WHERE root != issue"
        ))?;
        let mut rows = query.query([roots])?;
        while let Some(row) = rows.next()? {
            let root: String = row.get(0)?;
            indices.insert(row.get(1)?, indices[&root]);
        }
    }
    let ids: Vec<_> = indices.keys().map(String::as_str).collect();
    for (id, pull) in pull::pulls(db, &ids)? {
        let links = &mut page[indices[&id]].pulls;
        if !links.iter().any(|link| link.url == pull.url) {
            links.push(pull);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn issue_overview_keeps_the_last_accepted_owner_without_a_run() -> Result<()> {
        let store = Store::initialize(Path::new(":memory:"))?;
        store.db.execute_batch(
            r#"INSERT INTO chat_messages (id, chat_id, text) VALUES ('source', 'main', 'Fixture');
               INSERT INTO issues (id, source_seq, title, objective, parent_issue_id) VALUES
                   ('parent', 1, 'Parent', '', NULL), ('child', 1, 'Child', '', 'parent');
               INSERT INTO tasks (id, issue_id, category, status, repo_ids, required_capabilities) VALUES
                   ('original', 'parent', 'issue-router', 'completed', '[]', '[]'),
                   ('continuation', 'parent', 'issue-router', 'awaiting_receipt', '[]', '[]'),
                   ('child-task', 'child', 'implementation', 'reserved', '[]', '[]');
               INSERT INTO runners (id, host_id, profile, last_seen) VALUES ('runner', 'host', '{}', 0);
               INSERT INTO runner_assignments (id, task_id, runner_id, contract, state, expires_at) VALUES
                   ('original-claim', 'original', 'runner', '{"placement":{"agent":{"id":"prior"}}}', 'accepted', 0),
                   ('next-claim', 'continuation', 'runner', '{"placement":{"agent":{"id":"next"}}}', 'issued', 0),
                   ('child-claim', 'child-task', 'runner', '{"placement":{"agent":{"id":"worker"}}}', 'reserved', 0);
               UPDATE tasks SET current_assignment = 'next-claim' WHERE id = 'continuation';
               UPDATE tasks SET current_assignment = 'child-claim' WHERE id = 'child-task';"#,
        )?;
        let owner = |issue| -> Result<Option<String>> {
            Ok(store.issue_overview(issue)?.unwrap().owner_agent_id)
        };
        assert_eq!(owner("parent")?.as_deref(), Some("prior"));
        assert_eq!(owner("child")?, None);
        store.db.execute_batch(
            "UPDATE runner_assignments SET state = 'accepted' WHERE id IN ('next-claim', 'child-claim');
             UPDATE tasks SET status = 'accepted' WHERE id IN ('continuation', 'child-task');",
        )?;
        assert_eq!(owner("parent")?.as_deref(), Some("next"));
        assert_eq!(owner("child")?.as_deref(), Some("worker"));
        Ok(())
    }
}
