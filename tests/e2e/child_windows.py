"""Advance a selected child window only in a temporary e2e Server ledger."""

import sqlite3

from chat import wait_for


def expire_child_window(root, kind=None, *, count=1, task=None):
    def pending():
        with sqlite3.connect(root / "server.sqlite") as db:
            return db.execute(
                "SELECT DISTINCT task_id, wake_at FROM issue_inputs WHERE run_id IS NULL "
                "AND wake_at IS NOT NULL AND wake_at > 1 "
                "AND (?1 IS NULL OR json_extract(payload, '$.kind') = ?1) "
                "AND (?2 IS NULL OR task_id = ?2)", (kind, task),
            ).fetchall()

    groups = wait_for(pending, lambda groups: len(groups) >= count)
    with sqlite3.connect(root / "server.sqlite") as db:
        db.executemany(
            "UPDATE issue_inputs SET wake_at = 1 WHERE task_id = ? AND wake_at = ? AND run_id IS NULL",
            groups,
        )
