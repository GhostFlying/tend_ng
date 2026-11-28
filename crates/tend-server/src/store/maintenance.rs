use super::{Store, discussion::Change, timing};
use anyhow::Result;
use protocol::{CreateDiscussion, RunObservation, RunOutcome, RunPhase};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub(super) const STALL_AFTER: i64 = 30 * 60;
const BATCH: i64 = 32;
const PAGE: usize = 24;

pub(super) fn migrate(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS maintenance_cases (
            seq INTEGER PRIMARY KEY, id TEXT NOT NULL UNIQUE,
            kind TEXT NOT NULL, subject TEXT NOT NULL, status TEXT NOT NULL,
            first_seen INTEGER NOT NULL, last_seen INTEGER NOT NULL, changed_at INTEGER NOT NULL,
            revision INTEGER NOT NULL, ignored_revision INTEGER,
            fault TEXT NOT NULL, followup TEXT,
            UNIQUE(kind,subject)
         );
         CREATE INDEX IF NOT EXISTS maintenance_cases_pending ON maintenance_cases(kind, subject)
            WHERE status!='recovered';
         CREATE INDEX IF NOT EXISTS maintenance_cases_active ON maintenance_cases(seq)
            WHERE status='active' AND ignored_revision IS NOT revision;
         CREATE INDEX IF NOT EXISTS maintenance_cases_history ON maintenance_cases(seq)
            WHERE status!='active' AND ignored_revision IS NOT revision;
         CREATE INDEX IF NOT EXISTS maintenance_cases_ignored ON maintenance_cases(seq)
            WHERE ignored_revision=revision;
         CREATE TABLE IF NOT EXISTS maintenance_scan_progress (
            owner TEXT PRIMARY KEY REFERENCES feishu_scan(owner),
            advanced_at INTEGER, completed_through INTEGER, completed_at INTEGER
         );
         CREATE TABLE IF NOT EXISTS maintenance_default_discussions (
            case_id TEXT NOT NULL REFERENCES maintenance_cases(id),
            revision INTEGER NOT NULL,
            chat_id TEXT NOT NULL UNIQUE REFERENCES discussions(chat_id),
            PRIMARY KEY(case_id,revision)
         );
         CREATE TABLE IF NOT EXISTS maintenance_notes (
            case_id TEXT NOT NULL REFERENCES maintenance_cases(id), revision INTEGER NOT NULL,
            chat_id TEXT NOT NULL, message_id TEXT NOT NULL UNIQUE REFERENCES chat_messages(id),
            PRIMARY KEY(case_id,revision,chat_id)
         );",
    )?;
    Ok(())
}

const SCAN: &str = "feishu_scan_stalled";
const TASK: &str = "task_execution_failed";
const COLUMNS: &str = "id,kind,status,first_seen,last_seen,changed_at,revision,
    COALESCE(ignored_revision=revision,0),fault,followup";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct Link {
    label: String,
    href: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct Fault {
    title: String,
    impact: String,
    known: String,
    unknown: String,
    links: Vec<Link>,
    // Opaque source identity for comparison, not displayed or sent to Chat.
    marker: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct Followup {
    known: String,
    link: Option<Link>,
}

#[derive(Serialize)]
pub(crate) struct Case {
    id: String,
    kind: String,
    status: String,
    first_seen: i64,
    last_seen: i64,
    changed_at: i64,
    revision: i64,
    ignored: bool,
    fault: Fault,
    followup: Option<Followup>,
}

impl Case {
    fn note(&self, db: &Connection) -> Result<String> {
        let date = |time| date(db, time);
        let mut note = format!(
            "### 维护建议 · {}\n\n**影响**　{}\n\n**已知**　{}\n\n**未知**　{}\n\n**证据**\n",
            markdown_text(&self.fault.title),
            markdown_text(&self.fault.impact),
            markdown_text(&self.fault.known),
            markdown_text(&self.fault.unknown),
        );
        for link in &self.fault.links {
            note.push_str(&format!("- [{}]({})\n", link.label, link.href));
        }
        if let Some(followup) = &self.followup {
            note.push_str(&format!(
                "\n**后续观察**　{}\n",
                markdown_text(&followup.known)
            ));
            if let Some(link) = &followup.link {
                note.push_str(&format!("- [{}]({})\n", link.label, link.href));
            }
        }
        note.push_str(&format!(
            "\n首次观察 {} · 当前证据版本 #{}（{}）\n\n[返回维护建议](/maintenance-suggestions?id={})\n\n这份说明仅提供背景，不授权调查、修复、部署或改变原工作状态。",
            date(self.first_seen)?,
            self.revision,
            date(self.changed_at)?,
            self.id,
        ));
        Ok(note)
    }
}

fn markdown_text(text: &str) -> String {
    let mut escaped = String::new();
    for ch in text.chars() {
        match ch {
            '\r' | '\n' => escaped.push(' '),
            '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '~' | '|' => {
                escaped.push('\\');
                escaped.push(ch);
            }
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn date(db: &Connection, time: i64) -> Result<String> {
    Ok(db.query_row(
        "SELECT strftime('%Y-%m-%d %H:%M:%S UTC',?1,'unixepoch')",
        [time],
        |r| r.get(0),
    )?)
}

fn decode(row: &Row<'_>) -> rusqlite::Result<Case> {
    fn parse<T: serde::de::DeserializeOwned>(index: usize, raw: String) -> rusqlite::Result<T> {
        serde_json::from_str(&raw).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                index,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
    }
    Ok(Case {
        id: row.get(0)?,
        kind: row.get(1)?,
        status: row.get(2)?,
        first_seen: row.get(3)?,
        last_seen: row.get(4)?,
        changed_at: row.get(5)?,
        revision: row.get(6)?,
        ignored: row.get(7)?,
        fault: parse(8, row.get(8)?)?,
        followup: row
            .get::<_, Option<String>>(9)?
            .map(|v| parse(9, v))
            .transpose()?,
    })
}

fn by_id(db: &Connection, id: &str) -> Result<Option<Case>> {
    Ok(db
        .query_row(
            &format!("SELECT {COLUMNS} FROM maintenance_cases WHERE id=?1"),
            [id],
            decode,
        )
        .optional()?)
}

fn by_subject(db: &Connection, kind: &str, subject: &str) -> Result<Option<Case>> {
    Ok(db
        .query_row(
            &format!("SELECT {COLUMNS} FROM maintenance_cases WHERE kind=?1 AND subject=?2"),
            [kind, subject],
            decode,
        )
        .optional()?)
}

fn observe_fault(
    db: &Connection,
    kind: &str,
    subject: &str,
    before: Option<&Case>,
    fault: &Fault,
    now: i64,
) -> Result<()> {
    match before {
        Some(before)
            if before.status == "active"
                && before.fault.marker == fault.marker
                && before.fault.known == fault.known =>
        {
            db.execute(
                "UPDATE maintenance_cases SET last_seen=MAX(last_seen,?2) WHERE id=?1 AND last_seen<?2",
                params![before.id, now],
            )?;
        }
        Some(before) => {
            db.execute(
                "UPDATE maintenance_cases SET status='active',last_seen=MAX(last_seen,?2),
                   changed_at=MAX(changed_at,?2),revision=revision+1,fault=?3,followup=NULL WHERE id=?1",
                params![before.id, now, serde_json::to_string(fault)?],
            )?;
        }
        None => {
            db.execute(
                "INSERT INTO maintenance_cases(id,kind,subject,status,first_seen,last_seen,changed_at,revision,fault)
                   VALUES(?1,?2,?3,'active',?4,?4,?4,1,?5)",
                params![protocol::new_id(),kind,subject,now,serde_json::to_string(fault)?],
            )?;
        }
    }
    Ok(())
}

fn observe_followup(
    db: &Connection,
    before: &Case,
    recovered: bool,
    followup: &Followup,
    now: i64,
) -> Result<()> {
    // Only renewed positive fault evidence may reopen a recovered case.
    let status = if recovered {
        "recovered"
    } else {
        "not_observed"
    };
    if before.status != "recovered"
        && (before.status != status || before.followup.as_ref() != Some(followup))
    {
        db.execute(
            "UPDATE maintenance_cases SET status=?2,changed_at=MAX(changed_at,?3),
               revision=revision+1,followup=?4 WHERE id=?1",
            params![before.id, status, now, serde_json::to_string(followup)?],
        )?;
    }
    Ok(())
}

/// Kept with the source window commit so a crash can't split the two facts.
pub(super) fn scan_committed(
    db: &Connection,
    owner: &str,
    previous: (Option<i64>, Option<i64>),
    through: i64,
    now: i64,
    complete: bool,
) -> Result<()> {
    let advanced = previous.0.is_none_or(|previous| through > previous);
    if advanced || complete || previous.1.is_some() {
        db.execute(
            "INSERT INTO maintenance_scan_progress(owner,advanced_at,completed_through,completed_at)
               VALUES(?1,?2,?3,?4)
             ON CONFLICT(owner) DO UPDATE SET
               advanced_at=CASE WHEN ?5 THEN excluded.advanced_at ELSE COALESCE(advanced_at,excluded.advanced_at) END,
               completed_at=CASE WHEN excluded.completed_through>=COALESCE(completed_through,0)
                                 THEN excluded.completed_at ELSE completed_at END,
               completed_through=CASE WHEN excluded.completed_through>=COALESCE(completed_through,0)
                                      THEN excluded.completed_through ELSE completed_through END
             WHERE ?5 OR (completed_through IS NULL AND excluded.completed_through IS NOT NULL)
                      OR excluded.completed_through>=completed_through OR advanced_at IS NULL",
            params![owner,if advanced { Some(now) } else { previous.1 },complete.then_some(through),complete.then_some(now),advanced],
        )?;
    }
    Ok(())
}

fn scan_link() -> Link {
    Link {
        label: "飞书扫描状态与检查点".into(),
        href: "/feishu-suggestions#scan-status".into(),
    }
}

fn scan_gap(gap: Option<&str>) -> &'static str {
    // Older scanners stored transient phases in gap as well as failures.
    // Only confirmed failures belong in the versioned maintenance evidence.
    match gap {
        Some("identity") => "最近探测无法确认固定飞书身份。",
        Some("provider_unavailable") => "最近探测未能从飞书完整取得来源。",
        Some("incomplete_search") => "最近探测记录了不完整的搜索覆盖。",
        Some("storage") => "最近探测未能提交扫描窗口。",
        _ => "",
    }
}

fn probe_scan(db: &Connection, owner: &str, now: i64) -> Result<()> {
    let evidence = db
        .query_row(
            "SELECT s.checkpoint_at,COALESCE(p.advanced_at,s.last_success_at),s.gap,
                p.completed_through,p.completed_at
         FROM feishu_scan s LEFT JOIN maintenance_scan_progress p ON p.owner=s.owner
         WHERE s.owner=?1 AND s.checkpoint_at IS NOT NULL AND s.last_success_at IS NOT NULL",
            [owner],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                ))
            },
        )
        .optional()?;
    // No observation is not proof that the scan is healthy or recovered.
    let Some((checkpoint, advanced, gap, completed, completed_at)) = evidence else {
        return Ok(());
    };
    let before = by_subject(db, SCAN, owner)?;
    if advanced <= now - STALL_AFTER {
        let fault = Fault {
            title: "飞书扫描停止推进".into(),
            impact: "新的飞书 @ 消息可能还未进入飞书建议。已有建议与原消息不受此记录影响。".into(),
            known: format!(
                "已提交的扫描检查点停在 {}，最后一次可确认推进于 {}，已至少 30 分钟没有推进。{}",
                date(db, checkpoint)?,
                date(db, advanced)?,
                scan_gap(gap.as_deref())
            ),
            unknown: "尚不能确定原因、停滞期间漏掉多少消息，或断点之后的覆盖是否完整。".into(),
            links: vec![scan_link()],
            marker: checkpoint.to_string(),
        };
        return observe_fault(db, SCAN, owner, before.as_ref(), &fault, now);
    }
    if let Some(before) = before {
        let old = before.fault.marker.parse::<i64>()?;
        let recovered = completed.is_some_and(|through| {
            through > old && through == checkpoint && through > now - STALL_AFTER
        }) && completed_at.is_some_and(|at| at > now - STALL_AFTER);
        let followup = Followup {
            known: if recovered {
                format!(
                    "已提交一轮完整扫描，检查点已越过故障位置并追到 {}。",
                    date(db, checkpoint)?
                )
            } else if checkpoint > old {
                format!(
                    "扫描检查点已推进至 {}。当前未观察到持续停滞，尚未证实追平。",
                    date(db, checkpoint)?
                )
            } else {
                "当前未观察到持续停滞，但没有检查点越过原异常位置的证据。".into()
            },
            link: Some(scan_link()),
        };
        observe_followup(db, &before, recovered, &followup, now)?;
    }
    Ok(())
}

struct Task {
    status: String,
    assignment: Option<String>,
    title: String,
    number: i64,
    source: Option<String>,
    active: bool,
    latest: Option<RunObservation>,
    later_than_fault: bool,
    can_link_run: bool,
}

fn task_evidence(db: &Connection, id: &str, original_run: Option<&str>) -> Result<Option<Task>> {
    let task = db.query_row(
        "WITH RECURSIVE parents(id,parent,state) AS (
           SELECT i.id,i.parent_issue_id,i.state FROM issues i JOIN tasks t ON t.issue_id=i.id WHERE t.id=?1
           UNION ALL SELECT i.id,i.parent_issue_id,i.state FROM issues i JOIN parents p ON i.id=p.parent
         )
         SELECT t.status,t.current_assignment,i.title,n.number,
                (SELECT m.id FROM parents p JOIN issues root ON root.id=p.id
                  JOIN chat_messages m ON m.seq=root.source_seq AND m.chat_id='main' WHERE p.parent IS NULL),
                NOT EXISTS(SELECT 1 FROM parents p WHERE p.state!='open'
                    OR EXISTS(SELECT 1 FROM issue_parks park WHERE park.issue_id=p.id))
                 AND NOT EXISTS(SELECT 1 FROM parked_tasks p WHERE p.task_id=t.id)
                 AND NOT EXISTS(SELECT 1 FROM task_controls c WHERE c.assignment_id=t.current_assignment
                      AND c.state='pending' AND c.action->>'kind'='stop')
         FROM tasks t JOIN issues i ON i.id=t.issue_id JOIN issue_short_numbers n ON n.issue_id=i.id
         WHERE t.id=?1",
        [id],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)),
    ).optional()?;
    let Some((status, assignment, title, number, source, active)) = task else {
        return Ok(None);
    };
    let latest = db.query_row(
        "SELECT r.observation,
              EXISTS(SELECT 1 FROM task_runs old WHERE old.id=?2 AND old.task_id=r.task_id AND old.rowid<r.rowid),
              EXISTS(SELECT 1 FROM issue_messages m WHERE m.id=r.id AND m.run_id=r.id)
         FROM task_runs r JOIN runner_assignments a ON a.id=r.assignment_id AND a.state='accepted'
         WHERE r.task_id=?1 AND r.assignment_id=?3 ORDER BY r.rowid DESC LIMIT 1",
        params![id,original_run,assignment],
        |r| Ok((Some(r.get::<_,String>(0)?),r.get(1)?,r.get(2)?)),
    ).optional()?.unwrap_or((None,false,false));
    Ok(Some(Task {
        status,
        assignment,
        title,
        number,
        source,
        active,
        latest: latest.0.map(|raw| serde_json::from_str(&raw)).transpose()?,
        later_than_fault: latest.1,
        can_link_run: latest.2,
    }))
}

impl Task {
    fn link(&self, run: Option<&RunObservation>) -> Link {
        let mut href = format!("/issues/{}", self.number);
        if let Some(run) = run.filter(|r| self.can_link_run && protocol::valid_id(&r.run_id)) {
            href.push_str(&format!("#message-{}", run.run_id));
        }
        Link {
            label: format!("Issue #{} · 执行证据", self.number),
            href,
        }
    }
}

fn probe_task(db: &Connection, id: &str, now: i64) -> Result<()> {
    let before = by_subject(db, TASK, id)?;
    let Some(task) = task_evidence(db, id, before.as_ref().map(|v| v.fault.marker.as_str()))?
    else {
        return Ok(());
    };
    let run = task
        .latest
        .as_ref()
        .filter(|run| run.task_id == id && Some(&run.assignment_id) == task.assignment.as_ref());
    if task.active && task.status == "failed" {
        let Some(run) = run.filter(|r| {
            matches!(
                r.phase,
                RunPhase::Stopped {
                    outcome: RunOutcome::Failed,
                    ..
                }
            )
        }) else {
            // No accepted stop fact for the current failure.
            return Ok(());
        };
        let mut links = vec![task.link(Some(run))];
        if let Some(source) = &task.source
            && protocol::valid_id(source)
        {
            links.push(Link {
                label: "原主 Chat 话题".into(),
                href: format!("/#message-{source}"),
            });
        }
        let fault = Fault {
            title: format!(
                "Task 执行失败 · #{} {}",
                task.number,
                task.title.chars().take(120).collect::<String>()
            ),
            impact: "这个 Task 的本次执行没有完成，原 Issue 的交付可能仍待推进。".into(),
            known: format!(
                "Server 已接受 Task {id}、Run {} 的停止证据，结果为失败。",
                run.run_id
            ),
            unknown: "不能从停止结果确定根因、工作是否已在别处完成，或是否应该重试。".into(),
            links,
            marker: run.run_id.clone(),
        };
        return observe_fault(db, TASK, id, before.as_ref(), &fault, now);
    }
    if let Some(before) = before {
        let recovered = task.status == "completed"
            && task.later_than_fault
            && run.is_some_and(|r| {
                matches!(
                    r.phase,
                    RunPhase::Stopped {
                        outcome: RunOutcome::Completed,
                        ..
                    }
                )
            });
        let followup = Followup {
            known: if recovered {
                format!(
                    "原 Task 的后续 Run {} 已确认完成。此证据不代表 Issue 已结案或交付已验收。",
                    run.expect("recovery requires a run").run_id
                )
            } else if !task.active {
                "原 Issue 不再处于可推进范围或已有停止安排。当前不再提示执行失败，未证实工作恢复。"
                    .into()
            } else {
                "原 Task 不再呈现已确认的执行失败。当前状态不构成原工作恢复的证据。".into()
            },
            link: Some(task.link(if recovered { run } else { None })),
        };
        observe_followup(db, &before, recovered, &followup, now)?;
    }
    Ok(())
}

#[derive(Default)]
pub(crate) struct TaskCursors {
    failed: String,
    tracked: String,
}

impl Store {
    pub(crate) fn maintenance_probe_scan(&mut self, owner: Option<&str>, now: i64) -> Result<()> {
        let (tx, clock) = timing::begin(&mut self.db, "maintenance_scan")?;
        let inactive = tx
            .prepare_cached(&format!(
                "SELECT {COLUMNS} FROM maintenance_cases WHERE kind='feishu_scan_stalled'
                AND status='active' AND subject IS NOT ?1",
            ))?
            .query_map([owner], decode)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for case in inactive {
            observe_followup(
                &tx,
                &case,
                false,
                &Followup {
                    known: "此扫描身份已不在当前采集配置内。停止观测不代表扫描恢复。".into(),
                    link: None,
                },
                now,
            )?;
        }
        if let Some(owner) = owner {
            probe_scan(&tx, owner, now)?;
        }
        Ok(timing::commit(tx, clock)?)
    }

    pub(crate) fn maintenance_probe_tasks(
        &mut self,
        cursors: &mut TaskCursors,
        now: i64,
    ) -> Result<bool> {
        let (tx, clock) = timing::begin(&mut self.db, "maintenance_tasks")?;
        let ids = |sql, after: &str| -> Result<Vec<String>> {
            Ok(tx
                .prepare_cached(sql)?
                .query_map(params![after, BATCH], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?)
        };
        let failed = ids(
            "SELECT id FROM tasks WHERE status='failed' AND id>?1 ORDER BY id LIMIT ?2",
            &cursors.failed,
        )?;
        let tracked = ids(
            "SELECT subject FROM maintenance_cases
             WHERE kind='task_execution_failed' AND status!='recovered' AND subject>?1 ORDER BY subject LIMIT ?2",
            &cursors.tracked,
        )?;
        for id in &failed {
            probe_task(&tx, id, now)?;
        }
        for id in &tracked {
            if failed.binary_search(id).is_err() {
                probe_task(&tx, id, now)?;
            }
        }
        timing::commit(tx, clock)?;
        let next = |ids: Vec<String>| {
            if ids.len() == BATCH as usize {
                ids.into_iter().last().unwrap_or_default()
            } else {
                String::new()
            }
        };
        cursors.failed = next(failed);
        cursors.tracked = next(tracked);
        Ok(!cursors.failed.is_empty() || !cursors.tracked.is_empty())
    }

    pub(crate) fn maintenance_list(
        &self,
        view: &str,
        before: i64,
        configured: bool,
    ) -> Result<Value> {
        let filter = match view {
            "active" => "AND status='active' AND ignored_revision IS NOT revision",
            "ignored" => "AND ignored_revision=revision",
            "history" => "AND status!='active' AND ignored_revision IS NOT revision",
            _ => unreachable!("validated maintenance view"),
        };
        let mut st = self.db.prepare_cached(&format!(
            "SELECT {COLUMNS},seq FROM maintenance_cases WHERE seq<?1 {filter}
               ORDER BY seq DESC LIMIT ?2",
        ))?;
        let mut cases = st
            .query_map(params![before, PAGE + 1], |r| {
                Ok((decode(r)?, r.get::<_, i64>(10)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let next = if cases.len() > PAGE {
            cases.pop();
            cases.last().map(|c| c.1)
        } else {
            None
        };
        Ok(
            json!({"suggestions":cases.into_iter().map(|c| c.0).collect::<Vec<_>>(),
            "next_before":next.map(|seq| seq.to_string()),"feishu_configured":configured}),
        )
    }

    pub(crate) fn maintenance_detail(&self, id: &str) -> Result<Option<Value>> {
        by_id(&self.db, id)?
            .map(|case| {
                let note = case.note(&self.db)?;
                Ok(json!({"suggestion":case,"note":note}))
            })
            .transpose()
    }

    pub(crate) fn maintenance_ignore(
        &mut self,
        id: &str,
        revision: i64,
        ignored: bool,
    ) -> Result<Change<()>> {
        let (tx, clock) = timing::begin(&mut self.db, "maintenance_ignore")?;
        let Some(case) = by_id(&tx, id)? else {
            return Ok(Change::Missing);
        };
        if case.revision != revision {
            return Ok(Change::Conflict);
        }
        tx.execute(
            "UPDATE maintenance_cases SET ignored_revision=?2 WHERE id=?1 AND ignored_revision IS NOT ?2",
            params![id,ignored.then_some(revision)],
        )?;
        timing::commit(tx, clock)?;
        Ok(Change::Ok(()))
    }

    pub(crate) fn maintenance_share(
        &mut self,
        id: &str,
        revision: i64,
        target: Destination<'_>,
    ) -> Result<Change<Value>> {
        let (tx, clock) = timing::begin(&mut self.db, "maintenance_share")?;
        let Some(case) = by_id(&tx, id)? else {
            return Ok(Change::Missing);
        };
        let chat = match target {
            Destination::Main => Some("main".to_owned()),
            Destination::Existing(id) => Some(id.to_owned()),
            Destination::New => tx.query_row(
                "SELECT chat_id FROM maintenance_default_discussions WHERE case_id=?1 AND revision=?2",
                params![id,revision], |r| r.get(0),
            ).optional()?,
        };
        if let Some(chat) = &chat
            && let Some(message) = note_id(&tx, id, revision, chat)?
        {
            return Ok(Change::Ok(note_receipt(chat, &message)));
        }
        if case.revision != revision {
            return Ok(Change::Conflict);
        }
        let chat = match (chat, target) {
            (Some(chat), Destination::Existing(_)) => {
                let exists = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM discussions d WHERE d.chat_id=?1 AND d.archived=0
                       AND NOT EXISTS(SELECT 1 FROM feishu_handoff_bridges b WHERE b.chat_id=d.chat_id))",
                    [&chat], |r| r.get::<_,bool>(0),
                )?;
                if !exists {
                    return Ok(Change::Missing);
                }
                chat
            }
            (Some(chat), _) => chat,
            (None, _) => {
                let chat = protocol::new_id();
                let request = CreateDiscussion {
                    operation_id: protocol::new_id(),
                    title: Some(case.fault.title.clone()),
                    origin: None,
                };
                tx.execute(
                    "INSERT INTO discussions(chat_id,operation_id,creation,title,title_source)
                       VALUES(?1,?2,?3,?4,'manual')",
                    params![
                        chat,
                        request.operation_id,
                        serde_json::to_string(&request)?,
                        case.fault.title
                    ],
                )?;
                tx.execute(
                    "INSERT INTO maintenance_default_discussions VALUES(?1,?2,?3)",
                    params![id, revision, chat],
                )?;
                chat
            }
        };
        let message = protocol::new_id();
        tx.execute(
            "INSERT INTO chat_messages(id,chat_id,text) VALUES(?1,?2,?3)",
            params![message, chat, case.note(&tx)?],
        )?;
        tx.execute(
            "INSERT INTO maintenance_notes VALUES(?1,?2,?3,?4)",
            params![id, revision, chat, message],
        )?;
        if chat != "main" {
            tx.execute(
                "UPDATE discussions SET updated_at=unixepoch() WHERE chat_id=?1",
                [&chat],
            )?;
        }
        // Deliberately no router_inputs row. The user can reply explicitly.
        timing::commit(tx, clock)?;
        Ok(Change::Ok(note_receipt(&chat, &message)))
    }
}

pub(crate) enum Destination<'a> {
    Main,
    New,
    Existing(&'a str),
}

fn note_id(db: &Connection, id: &str, revision: i64, chat: &str) -> Result<Option<String>> {
    Ok(db.query_row(
        "SELECT message_id FROM maintenance_notes WHERE case_id=?1 AND revision=?2 AND chat_id=?3",
        params![id,revision,chat], |r| r.get(0),
    ).optional()?)
}

fn note_receipt(chat: &str, message: &str) -> Value {
    let path = if chat == "main" {
        "/".into()
    } else {
        format!("/discussions?chat={chat}")
    };
    json!({"chat_id":chat,"message_id":message,"href":format!("{path}#message-{message}")})
}

#[cfg(test)]
mod tests;
