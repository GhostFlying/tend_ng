CREATE TABLE token_issue_tree (
    ancestor TEXT NOT NULL REFERENCES issues(id),
    descendant TEXT NOT NULL REFERENCES issues(id),
    PRIMARY KEY (ancestor, descendant)
) WITHOUT ROWID;
CREATE INDEX token_issue_ancestors ON token_issue_tree(descendant, ancestor);
WITH RECURSIVE edges(ancestor, descendant) AS (
    SELECT id, id FROM issues UNION
    SELECT i.parent_issue_id, e.descendant FROM edges e
    JOIN issues i ON i.id = e.ancestor WHERE i.parent_issue_id IS NOT NULL
)
INSERT INTO token_issue_tree SELECT * FROM edges;
CREATE TRIGGER token_issue_tree_insert AFTER INSERT ON issues BEGIN
    INSERT INTO token_issue_tree (ancestor, descendant)
    SELECT NEW.id, NEW.id UNION ALL
    SELECT ancestor, NEW.id FROM token_issue_tree WHERE descendant = NEW.parent_issue_id;
END;
CREATE TABLE token_issue_rollups (
    issue_id TEXT NOT NULL REFERENCES issues(id),
    scope TEXT NOT NULL CHECK (scope IN ('direct', 'subtree')),
    summary TEXT NOT NULL,
    PRIMARY KEY (issue_id, scope)
) WITHOUT ROWID;
CREATE TABLE token_runs (
    run_id TEXT NOT NULL PRIMARY KEY REFERENCES task_runs(id),
    assignment_id TEXT NOT NULL REFERENCES runner_assignments(id),
    issue_id TEXT NOT NULL REFERENCES issues(id),
    thread_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    legacy INTEGER NOT NULL,
    tally TEXT NOT NULL,
    valid_records INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE token_requests (
    thread_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    response_id TEXT NOT NULL,
    run_id TEXT NOT NULL REFERENCES token_runs(run_id),
    last TEXT NOT NULL,
    observed_at INTEGER NOT NULL,
    conflict TEXT,
    PRIMARY KEY (thread_id, turn_id, response_id)
) WITHOUT ROWID;
CREATE INDEX token_request_run ON token_requests(run_id);
CREATE TABLE token_usage_pending (
    event_id TEXT NOT NULL PRIMARY KEY,
    run_id TEXT NOT NULL,
    runner_id TEXT NOT NULL REFERENCES runners(id),
    payload TEXT NOT NULL
);
CREATE INDEX token_pending_run ON token_usage_pending(run_id);
