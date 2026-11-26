-- Wrought `forms` feature, SQLite (same shape as the Postgres migration).

CREATE TABLE engine_forms (
    name TEXT PRIMARY KEY CHECK (
        length(name) BETWEEN 1 AND 40 AND name GLOB '[a-z]*' AND name NOT GLOB '*[^a-z0-9_-]*'),
    definition TEXT NOT NULL CHECK (json_valid(definition)),
    version INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE engine_form_submissions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    form TEXT NOT NULL,
    data TEXT NOT NULL CHECK (json_valid(data)),
    unique_key TEXT,
    user_id INTEGER,
    ip TEXT,
    user_agent TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE INDEX engine_form_submissions_form ON engine_form_submissions (form, id DESC);
CREATE INDEX engine_form_submissions_user ON engine_form_submissions (user_id, id DESC)
    WHERE user_id IS NOT NULL;
CREATE UNIQUE INDEX engine_form_submissions_unique ON engine_form_submissions (form, unique_key)
    WHERE unique_key IS NOT NULL;

CREATE TABLE engine_form_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    form TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('accepted', 'spam')),
    key TEXT NOT NULL,
    reason TEXT,
    at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE INDEX engine_form_events_key ON engine_form_events (form, key, at);

CREATE TABLE engine_keys (
    name TEXT PRIMARY KEY,
    value BLOB NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
