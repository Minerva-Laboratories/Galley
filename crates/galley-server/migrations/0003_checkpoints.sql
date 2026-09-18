-- Named checkpoints. The git tag (galley/checkpoint/<id>) is the recoverable source of truth;
-- this row carries the label, author, and timestamp for listing without walking tags.
CREATE TABLE checkpoints (
    id         TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    commit_sha TEXT NOT NULL,
    label      TEXT NOT NULL,
    author_id  TEXT,
    created_at TEXT NOT NULL
);

CREATE INDEX checkpoints_by_project ON checkpoints (project_id, created_at DESC);
