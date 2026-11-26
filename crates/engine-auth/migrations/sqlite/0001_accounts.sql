-- Wrought `auth` feature, SQLite (same shape as the Postgres migration).

CREATE TABLE engine_users (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    email TEXT NOT NULL UNIQUE
        CHECK (email = lower(email) AND length(email) BETWEEN 3 AND 254 AND instr(email, '@') > 1),
    name TEXT NOT NULL DEFAULT '' CHECK (length(name) <= 100),
    password_hash TEXT NOT NULL,
    active BOOLEAN NOT NULL DEFAULT 1,
    totp_secret TEXT,
    totp_last_step INTEGER,
    version INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    last_login_at TEXT
);

CREATE TABLE engine_sessions (
    token_hash BLOB PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES engine_users (id) ON DELETE CASCADE,
    csrf_token TEXT NOT NULL,
    stage TEXT NOT NULL DEFAULT 'full' CHECK (stage IN ('full', 'totp')),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    last_seen_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    expires_at TEXT NOT NULL,
    ip TEXT,
    user_agent TEXT
);
CREATE INDEX engine_sessions_user ON engine_sessions (user_id);

CREATE TABLE engine_password_resets (
    token_hash BLOB PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES engine_users (id) ON DELETE CASCADE,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    expires_at TEXT NOT NULL,
    used_at TEXT
);
CREATE INDEX engine_password_resets_user ON engine_password_resets (user_id);

CREATE TABLE engine_auth_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL,
    key TEXT NOT NULL,
    at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE INDEX engine_auth_events_key ON engine_auth_events (kind, key, at);
