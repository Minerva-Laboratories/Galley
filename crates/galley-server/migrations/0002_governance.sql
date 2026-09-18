-- Rename the top project role owner -> admin, and add per-project governance: a mode plus
-- pending role-change requests that admins vote on.

UPDATE members SET role = 'admin' WHERE role = 'owner';
UPDATE share_links SET role = 'admin' WHERE role = 'owner';

CREATE TABLE project_config (
    project_id TEXT PRIMARY KEY,
    governance TEXT NOT NULL DEFAULT 'solo'   -- solo | majority | unanimous
);

-- A proposed change awaiting approval. `payload` holds the specifics as JSON.
CREATE TABLE role_requests (
    id           TEXT PRIMARY KEY,
    project_id   TEXT NOT NULL,
    kind         TEXT NOT NULL,   -- set_role | remove_member | invite | set_governance
    target_id    TEXT,            -- affected user (null for set_governance)
    payload      TEXT NOT NULL,   -- {"role":"editor"} | {"governance":"unanimous"} | {"email":"..."}
    summary      TEXT NOT NULL,   -- human-readable description for the UI
    proposer_id  TEXT NOT NULL,
    proposer_name TEXT NOT NULL,
    status       TEXT NOT NULL DEFAULT 'open',  -- open | applied | rejected | cancelled
    created_at   TEXT NOT NULL,
    resolved_at  TEXT
);
CREATE INDEX role_requests_project ON role_requests (project_id, status);

CREATE TABLE role_request_votes (
    request_id TEXT NOT NULL,
    admin_id   TEXT NOT NULL,
    vote       TEXT NOT NULL,     -- approve | reject
    created_at TEXT NOT NULL,
    PRIMARY KEY (request_id, admin_id)
);
