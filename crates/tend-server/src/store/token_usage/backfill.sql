CREATE TABLE IF NOT EXISTS token_backfill_jobs (
    id TEXT PRIMARY KEY,
    request TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE TABLE IF NOT EXISTS token_backfill_readers (
    runner_id TEXT PRIMARY KEY REFERENCES runners(id)
);
CREATE TABLE IF NOT EXISTS token_backfill_items (
    id INTEGER PRIMARY KEY,
    job_id TEXT NOT NULL REFERENCES token_backfill_jobs(id),
    run_id TEXT NOT NULL REFERENCES token_runs(run_id),
    reader_id TEXT REFERENCES runners(id),
    offset INTEGER NOT NULL DEFAULT 0,
    snapshot TEXT,
    last_response TEXT,
    state TEXT NOT NULL DEFAULT 'pending',
    gap TEXT,
    UNIQUE (job_id, run_id)
);
CREATE INDEX IF NOT EXISTS token_backfill_original_run ON token_backfill_items(run_id);
CREATE INDEX IF NOT EXISTS token_backfill_pending ON token_backfill_items(reader_id, id) WHERE state = 'pending';
CREATE TABLE IF NOT EXISTS token_backfill_seen (
    item INTEGER NOT NULL REFERENCES token_backfill_items(id),
    response_id TEXT NOT NULL,
    result TEXT NOT NULL,
    PRIMARY KEY (item, response_id)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS token_backfill_keys (
    thread_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    response_id TEXT NOT NULL,
    run_id TEXT NOT NULL REFERENCES token_runs(run_id),
    job_id TEXT NOT NULL REFERENCES token_backfill_jobs(id),
    evidence TEXT NOT NULL,
    PRIMARY KEY (thread_id, turn_id, response_id)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS token_backfill_audit (
    id INTEGER PRIMARY KEY,
    job_id TEXT NOT NULL REFERENCES token_backfill_jobs(id),
    run_id TEXT NOT NULL REFERENCES token_runs(run_id),
    response_id TEXT NOT NULL,
    source TEXT NOT NULL,
    evidence TEXT NOT NULL,
    UNIQUE (job_id, run_id, response_id, source, evidence)
);
CREATE INDEX IF NOT EXISTS token_backfill_audit_page ON token_backfill_audit(job_id, id);
CREATE TRIGGER IF NOT EXISTS token_backfill_reroute AFTER UPDATE OF retired_at ON runner_replacements
WHEN NEW.retired_at IS NOT NULL AND OLD.retired_at IS NULL
BEGIN
    UPDATE token_backfill_items SET reader_id = NEW.successor
    WHERE reader_id = NEW.predecessor AND state = 'pending'
      AND EXISTS (
          SELECT 1 FROM runners p JOIN runners s ON s.host_id = p.host_id
          WHERE p.id = NEW.predecessor AND s.id = NEW.successor
      );
END;
CREATE INDEX IF NOT EXISTS token_runs_issue ON token_runs(issue_id);
CREATE INDEX IF NOT EXISTS token_runs_assignment ON token_runs(assignment_id);
CREATE INDEX IF NOT EXISTS token_runs_native ON token_runs(thread_id, turn_id);
CREATE TABLE IF NOT EXISTS token_backfill_ambiguous (
    thread_id TEXT NOT NULL, turn_id TEXT NOT NULL, PRIMARY KEY(thread_id, turn_id)
) WITHOUT ROWID;
