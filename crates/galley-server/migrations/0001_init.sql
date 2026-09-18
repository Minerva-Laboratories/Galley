-- Accounts, membership, shares, comments, suggestions, sessions, audit. Scoped to M3
-- collaboration. Applied on startup, never edited after merge.

CREATE TABLE users (
    id         TEXT PRIMARY KEY,
    email      TEXT NOT NULL,
    name       TEXT NOT NULL,
    pw_hash    TEXT,               -- null for share-link guests
    oidc_sub   TEXT,
    is_admin   INTEGER NOT NULL DEFAULT 0,
    is_guest   INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
);
CREATE UNIQUE INDEX users_email ON users (email) WHERE email <> '';

CREATE TABLE members (
    project_id TEXT NOT NULL,
    user_id    TEXT NOT NULL,
    role       TEXT NOT NULL,      -- owner | editor | commenter | viewer
    created_at TEXT NOT NULL,
    PRIMARY KEY (project_id, user_id)
);
CREATE INDEX members_user ON members (user_id);

CREATE TABLE share_links (
    id         TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    role       TEXT NOT NULL,
    token_hash TEXT NOT NULL,      -- sha256(token); the token itself is never stored
    label      TEXT,
    expires_at TEXT,               -- null = never expires
    created_by TEXT NOT NULL,
    created_at TEXT NOT NULL,
    revoked    INTEGER NOT NULL DEFAULT 0
);
CREATE UNIQUE INDEX share_links_token ON share_links (token_hash);
CREATE INDEX share_links_project ON share_links (project_id);

CREATE TABLE comments (
    id          TEXT PRIMARY KEY,
    project_id  TEXT NOT NULL,
    file        TEXT NOT NULL,
    anchor      TEXT NOT NULL,     -- Yjs relative position (base64), resolved client-side
    quote       TEXT,
    author_id   TEXT NOT NULL,
    author_name TEXT NOT NULL,
    body        TEXT NOT NULL,
    resolved    INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL
);
CREATE INDEX comments_project ON comments (project_id, file);

CREATE TABLE suggestions (
    id          TEXT PRIMARY KEY,
    project_id  TEXT NOT NULL,
    file        TEXT NOT NULL,
    anchor      TEXT NOT NULL,     -- Yjs relative range start (base64)
    anchor_end  TEXT NOT NULL,     -- Yjs relative range end (base64)
    quote       TEXT NOT NULL,     -- the original text at proposal time
    replacement TEXT NOT NULL,     -- '' for a pure deletion
    author_id   TEXT NOT NULL,
    author_name TEXT NOT NULL,
    status      TEXT NOT NULL DEFAULT 'open',  -- open | accepted | rejected
    created_at  TEXT NOT NULL
);
CREATE INDEX suggestions_project ON suggestions (project_id, file);

CREATE TABLE sessions (
    token_hash TEXT PRIMARY KEY,   -- sha256(cookie value)
    user_id    TEXT NOT NULL,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
);

CREATE TABLE audit_log (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id TEXT,
    actor_id   TEXT,
    actor_name TEXT,
    action     TEXT NOT NULL,
    detail     TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX audit_project ON audit_log (project_id);

CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
