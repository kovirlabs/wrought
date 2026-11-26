-- Wrought `api` feature, SQLite (same shape as the Postgres migration).
CREATE TABLE engine_api_tokens (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id INTEGER NOT NULL REFERENCES engine_users (id) ON DELETE CASCADE,
    name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 100),
    token_hash BLOB NOT NULL UNIQUE,
    prefix TEXT NOT NULL,
    scopes TEXT NOT NULL DEFAULT '["read"]' CHECK (json_valid(scopes)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    expires_at TEXT,
    last_used_at TEXT,
    revoked_at TEXT
);
CREATE INDEX engine_api_tokens_user ON engine_api_tokens (user_id, id DESC);
