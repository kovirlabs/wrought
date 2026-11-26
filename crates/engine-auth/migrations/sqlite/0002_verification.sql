-- Wrought `auth` v0.5: email verification (see the Postgres migration).
ALTER TABLE engine_users ADD COLUMN email_verified_at TEXT;
UPDATE engine_users SET email_verified_at = created_at;

CREATE TABLE engine_email_verifications (
    token_hash BLOB PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES engine_users (id) ON DELETE CASCADE,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    expires_at TEXT NOT NULL,
    used_at TEXT
);
CREATE INDEX engine_email_verifications_user ON engine_email_verifications (user_id);
