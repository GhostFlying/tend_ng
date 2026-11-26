-- Projection tables have no incoming foreign keys. Keep identical shadow
-- tables so publication can rotate names without rewriting the rows.
CREATE TABLE IF NOT EXISTS token_run_tallies (
    run_id TEXT NOT NULL PRIMARY KEY REFERENCES token_runs(run_id),
    tally TEXT NOT NULL, valid_records INTEGER NOT NULL
) WITHOUT ROWID;
INSERT INTO token_run_tallies (run_id, tally, valid_records)
SELECT run_id, tally, valid_records FROM token_runs WHERE true ON CONFLICT DO NOTHING;
CREATE TABLE IF NOT EXISTS token_rebuild_runs (
    run_id TEXT NOT NULL PRIMARY KEY REFERENCES token_runs(run_id),
    tally TEXT NOT NULL, valid_records INTEGER NOT NULL
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS token_rebuild_issues (
    issue_id TEXT NOT NULL REFERENCES issues(id),
    scope TEXT NOT NULL CHECK (scope IN ('direct','subtree')),
    summary TEXT NOT NULL,
    PRIMARY KEY (issue_id, scope)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS token_rebuild_state (
    id INTEGER PRIMARY KEY CHECK(id = 1), collecting INTEGER NOT NULL
);
INSERT INTO token_rebuild_state VALUES (1, 0) ON CONFLICT DO NOTHING;
CREATE TABLE IF NOT EXISTS token_rebuild_dirty (
    run_id TEXT PRIMARY KEY REFERENCES token_runs(run_id),
    revision INTEGER NOT NULL
) WITHOUT ROWID;
CREATE TRIGGER IF NOT EXISTS token_rebuild_track_start AFTER INSERT ON token_runs
WHEN (SELECT collecting FROM token_rebuild_state WHERE id = 1)
BEGIN
    INSERT INTO token_rebuild_dirty VALUES (NEW.run_id, 1)
    ON CONFLICT(run_id) DO UPDATE SET revision = revision + 1;
END;
CREATE TRIGGER IF NOT EXISTS token_rebuild_track_insert AFTER INSERT ON token_requests
WHEN (SELECT collecting FROM token_rebuild_state WHERE id = 1)
BEGIN
    INSERT INTO token_rebuild_dirty VALUES (NEW.run_id, 1)
    ON CONFLICT(run_id) DO UPDATE SET revision = revision + 1;
END;
CREATE TRIGGER IF NOT EXISTS token_rebuild_track_update AFTER UPDATE OF last, conflict ON token_requests
WHEN (SELECT collecting FROM token_rebuild_state WHERE id = 1)
BEGIN
    INSERT INTO token_rebuild_dirty VALUES (NEW.run_id, 1)
    ON CONFLICT(run_id) DO UPDATE SET revision = revision + 1;
END;
CREATE TRIGGER IF NOT EXISTS token_rebuild_track_delete AFTER DELETE ON token_requests
WHEN (SELECT collecting FROM token_rebuild_state WHERE id = 1)
BEGIN
    INSERT INTO token_rebuild_dirty VALUES (OLD.run_id, 1)
    ON CONFLICT(run_id) DO UPDATE SET revision = revision + 1;
END;
