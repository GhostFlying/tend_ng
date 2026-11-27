CREATE TABLE token_agent_attempts (
    id TEXT PRIMARY KEY NOT NULL,
    source TEXT NOT NULL,
    thread TEXT NOT NULL,
    turn TEXT,
    rollout TEXT,
    process_pid INTEGER,
    process_start TEXT,
    legacy INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE token_agent_turns (
    thread TEXT NOT NULL,
    turn TEXT NOT NULL,
    source TEXT NOT NULL,
    attempt TEXT NOT NULL REFERENCES token_agent_attempts(id),
    observed INTEGER NOT NULL DEFAULT 0,
    legacy INTEGER NOT NULL,
    PRIMARY KEY (thread, turn)
) WITHOUT ROWID;
CREATE INDEX token_agent_turns_attempt ON token_agent_turns(attempt);
CREATE INDEX token_backfill_keys_run ON token_backfill_keys(run_id);
CREATE TABLE token_agent_requests (
    thread TEXT NOT NULL,
    turn TEXT NOT NULL,
    response TEXT NOT NULL,
    meter TEXT NOT NULL,
    conflict TEXT,
    shadowed INTEGER NOT NULL,
    PRIMARY KEY (thread, turn, response),
    FOREIGN KEY (thread, turn) REFERENCES token_agent_turns(thread, turn)
) WITHOUT ROWID;
CREATE TABLE token_agent_rollups (
    source TEXT PRIMARY KEY NOT NULL,
    stats TEXT NOT NULL
) WITHOUT ROWID;
CREATE TABLE token_agent_backfill (
    job TEXT NOT NULL REFERENCES token_backfill_jobs(id),
    attempt TEXT NOT NULL REFERENCES token_agent_attempts(id),
    state TEXT NOT NULL DEFAULT 'pending',
    detail TEXT,
    responses INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (job, attempt)
) WITHOUT ROWID;
CREATE INDEX token_agent_backfill_pending ON token_agent_backfill(job, attempt) WHERE state = 'pending';
