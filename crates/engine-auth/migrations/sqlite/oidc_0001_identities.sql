-- Wrought `oidc` feature, SQLite (same shape as the Postgres migration).
CREATE TABLE engine_oidc_identities (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id INTEGER NOT NULL REFERENCES engine_users (id) ON DELETE CASCADE,
    provider TEXT NOT NULL,
    issuer TEXT NOT NULL,
    subject TEXT NOT NULL CHECK (length(subject) BETWEEN 1 AND 255),
    email TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    last_used_at TEXT,
    UNIQUE (issuer, subject),
    UNIQUE (user_id, issuer)
);

CREATE TABLE engine_oidc_states (
    token_hash BLOB PRIMARY KEY,
    provider TEXT NOT NULL,
    state TEXT NOT NULL,
    nonce TEXT NOT NULL,
    verifier TEXT NOT NULL,
    purpose TEXT NOT NULL CHECK (purpose IN ('login', 'link')),
    user_id INTEGER REFERENCES engine_users (id) ON DELETE CASCADE,
    next TEXT NOT NULL DEFAULT '/',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    expires_at TEXT NOT NULL
);
CREATE INDEX engine_oidc_states_expires ON engine_oidc_states (expires_at);
