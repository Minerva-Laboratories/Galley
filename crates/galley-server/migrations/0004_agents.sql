-- Device tokens let an AI client (MCP) or a local runner act as a member without a browser
-- session. Only the SHA-256 of the token is stored; each is labelled and individually revocable.
CREATE TABLE device_tokens (
    token_hash   TEXT PRIMARY KEY,
    user_id      TEXT NOT NULL,
    label        TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    last_used_at TEXT,
    revoked      INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX device_tokens_by_user ON device_tokens (user_id, created_at DESC);

-- Every agent run that produced a proposal, for attribution and the AI-disclosure report.
CREATE TABLE agent_runs (
    id         TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    user_id    TEXT NOT NULL,
    agent      TEXT NOT NULL,
    model      TEXT,
    summary    TEXT NOT NULL,
    edits      INTEGER NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX agent_runs_by_project ON agent_runs (project_id, created_at DESC);
