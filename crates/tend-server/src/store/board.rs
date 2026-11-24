use super::{PAGE_SIZE, Store, pull, work::read_list};
use anyhow::Result;
use protocol::{BoardAttention, BoardStage, BoardWait, IssueBoard, IssueCard};
use rusqlite::{Connection, params};
use std::collections::HashMap;

const TREE: &str =
    "WITH RECURSIVE tree(root, issue) AS (
         SELECT value, value FROM json_each(?1)
         UNION ALL
         SELECT tree.root, child.id FROM tree JOIN issues child ON child.parent_issue_id = tree.issue
     )";

#[derive(Default)]
struct Progress {
    running: u32,
    ready: bool,
    waiting: bool,
    external: bool,
    reply: bool,
    children: bool,
}

impl Progress {
    fn stage(&self, issue: &IssueCard) -> BoardStage {
        match issue.issue_state.as_str() {
            "closed" => BoardStage::Done,
            "cancelled" => BoardStage::Cancelled,
            _ if self.running > 0 => BoardStage::InProgress,
            _ if issue.task_state == "backlog" => BoardStage::Backlog,
            _ if self.ready => BoardStage::Todo,
            _ if self.waiting || issue.attention.is_some() => BoardStage::Waiting,
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
    pub fn issue_board(
        &self,
        chat: Option<&str>,
        before: i64,
        roots_only: bool,
    ) -> Result<IssueBoard> {
        let tx = self.db.unchecked_transaction()?;
        let mut page = {
            let mut query = tx.prepare(
                "SELECT i.rowid, i.id, i.parent_issue_id, i.title, i.state,
                        COALESCE(t.status, 'backlog'),
                        COALESCE(t.repo_ids, json_extract(d.request, '$.repo_ids'), '[]')
                 FROM issues i JOIN chat_messages source ON source.seq = i.source_seq
                 LEFT JOIN tasks t ON t.rowid = (
                    SELECT rowid FROM tasks WHERE issue_id = i.id ORDER BY rowid DESC LIMIT 1
                 )
                 LEFT JOIN router_dispatches d ON d.issue_id = i.id
                 WHERE (?1 IS NULL OR source.chat_id = ?1) AND i.rowid < ?2
                   AND (NOT ?4 OR i.parent_issue_id IS NULL)
                 ORDER BY i.rowid DESC LIMIT ?3",
            )?;
            query
                .query_map(params![chat, before, PAGE_SIZE + 1, roots_only], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        IssueCard {
                            id: row.get(1)?,
                            parent_issue_id: row.get(2)?,
                            title: row.get(3)?,
                            issue_state: row.get(4)?,
                            task_state: row.get(5)?,
                            repo_ids: read_list(row, 6)?,
                            stage: BoardStage::default(),
                            running: 0,
                            waiting_for: None,
                            attention: None,
                            pulls: Vec::new(),
                        },
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let next_before = if page.len() > PAGE_SIZE {
            page.pop();
            page.last().map(|(seq, _)| *seq)
        } else {
            None
        };

        if !page.is_empty() {
            let roots = serde_json::to_string(
                &page
                    .iter()
                    .map(|(_, card)| card.id.as_str())
                    .collect::<Vec<_>>(),
            )?;
            let mut progress = read_progress(&tx, &roots)?;
            let mut attentions = read_attentions(&tx, &roots)?;
            for (_, card) in &mut page {
                let progress = progress.remove(&card.id).unwrap_or_default();
                card.attention = attentions.remove(&card.id);
                card.running = progress.running;
                card.stage = progress.stage(card);
                if matches!(card.stage, BoardStage::Waiting) {
                    card.waiting_for = Some(progress.reason(card));
                }
            }
            attach_pulls(&tx, &roots, &mut page, roots_only)?;
        }
        tx.commit()?;
        Ok(IssueBoard {
            issues: page.into_iter().map(|(_, card)| card).collect(),
            next_before,
        })
    }
}

fn read_progress(db: &Connection, roots: &str) -> Result<HashMap<String, Progress>> {
    let mut query = db.prepare(&format!(
        "{TREE}, work AS (
             SELECT tree.root, t.id, t.status, COALESCE(t.workspace_key, i.parent_issue_id) AS workspace,
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
                MAX(
                    (w.status IN ('awaiting_assignment','awaiting_receipt','accepted') OR (
                        w.status = 'waiting' AND (
                            EXISTS (SELECT 1 FROM issue_inputs WHERE task_id = w.id AND run_id IS NULL)
                            OR w.queued
                        )
                    )) AND NOT EXISTS (
                        SELECT 1 FROM work holder WHERE holder.root = w.root AND holder.id != w.id
                         AND holder.workspace = w.workspace AND holder.status = 'waiting' AND holder.external
                    )
                ),
                MAX(w.status = 'waiting'),
                MAX(w.external), MAX(w.reply), MAX(w.children)
         FROM work w GROUP BY w.root"
    ))?;
    Ok(query
        .query_map([roots], |row| {
            Ok((
                row.get(0)?,
                Progress {
                    running: row.get(1)?,
                    ready: row.get(2)?,
                    waiting: row.get(3)?,
                    external: row.get(4)?,
                    reply: row.get(5)?,
                    children: row.get(6)?,
                },
            ))
        })?
        .collect::<rusqlite::Result<HashMap<_, _>>>()?)
}

fn read_attentions(db: &Connection, roots: &str) -> Result<HashMap<String, BoardAttention>> {
    let mut query = db.prepare(&format!(
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
    page: &mut [(i64, IssueCard)],
    roots_only: bool,
) -> Result<()> {
    let mut indices: HashMap<_, _> = page
        .iter()
        .enumerate()
        .map(|(index, (_, card))| (card.id.clone(), index))
        .collect();
    if roots_only {
        let mut query = db.prepare(&format!(
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
        let links = &mut page[indices[&id]].1.pulls;
        if !links.iter().any(|link| link.url == pull.url) {
            links.push(pull);
        }
    }
    Ok(())
}
