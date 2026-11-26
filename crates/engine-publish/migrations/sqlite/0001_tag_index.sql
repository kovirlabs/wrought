-- Wrought `publish` feature, SQLite: the tag index and the change-log
-- cursor (same shape as migrations/postgres/0001_tag_index.sql).

CREATE TABLE engine_pages (
    path TEXT PRIMARY KEY,
    output TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    rendered_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE engine_page_tags (
    tag TEXT NOT NULL,
    path TEXT NOT NULL REFERENCES engine_pages (path) ON DELETE CASCADE,
    PRIMARY KEY (tag, path)
);
CREATE INDEX engine_page_tags_path ON engine_page_tags (path);

CREATE TABLE engine_publish_state (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    last_change INTEGER NOT NULL DEFAULT 0
);
INSERT INTO engine_publish_state (id) VALUES (1);
