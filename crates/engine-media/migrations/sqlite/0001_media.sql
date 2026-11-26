-- Wrought `media` feature, SQLite (same shape as the Postgres migration).
CREATE TABLE engine_media (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL CHECK (
        length(name) BETWEEN 1 AND 60 AND name NOT GLOB '*[^a-z0-9-]*'
        AND name NOT GLOB '-*' AND name NOT GLOB '*-' AND name NOT GLOB '*--*'),
    ext TEXT NOT NULL CHECK (ext IN ('jpg', 'png', 'gif', 'webp', 'pdf')),
    mime TEXT NOT NULL,
    original_name TEXT NOT NULL DEFAULT '' CHECK (length(original_name) <= 255),
    bytes INTEGER NOT NULL,
    width INTEGER,
    height INTEGER,
    variants TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(variants)),
    sha256 TEXT NOT NULL,
    alt TEXT NOT NULL DEFAULT '' CHECK (length(alt) <= 500),
    private BOOLEAN NOT NULL DEFAULT 0,
    uploaded_by INTEGER,
    version INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE INDEX engine_media_list ON engine_media (private, id DESC);
