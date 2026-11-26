-- Wrought `content` feature, SQLite: tables shared by every content type.
-- Same shape as migrations/postgres/0001_base.sql. Times are ISO-8601 text
-- in UTC with millisecond precision (sorts like time). There is no NOTIFY:
-- the publisher is woken in-process, by watching the database file, and by
-- polling engine_changes (see engine-publish's listener).

CREATE TABLE engine_revisions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    content_type TEXT NOT NULL,
    item_id INTEGER NOT NULL,
    version INTEGER NOT NULL,
    op TEXT NOT NULL CHECK (op IN ('create', 'update', 'delete')),
    data TEXT NOT NULL CHECK (json_valid(data)),
    author TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (content_type, item_id, version)
);

CREATE TABLE engine_terms (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    vocabulary TEXT NOT NULL,
    slug TEXT NOT NULL CHECK (
        length(slug) BETWEEN 1 AND 80 AND slug NOT GLOB '*[^a-z0-9-]*'
        AND slug NOT GLOB '-*' AND slug NOT GLOB '*-' AND slug NOT GLOB '*--*'),
    name TEXT NOT NULL,
    version INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (vocabulary, slug)
);

CREATE TABLE engine_item_terms (
    content_type TEXT NOT NULL,
    item_id INTEGER NOT NULL,
    term_id INTEGER NOT NULL REFERENCES engine_terms (id) ON DELETE CASCADE,
    PRIMARY KEY (content_type, item_id, term_id)
);
CREATE INDEX engine_item_terms_term ON engine_item_terms (term_id);

-- Change log, written by triggers (so edits made with the sqlite3 shell
-- count too). AUTOINCREMENT keeps ids increasing even after pruning, which
-- the publisher's cursor relies on.
CREATE TABLE engine_changes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    content_type TEXT NOT NULL,
    item_id INTEGER NOT NULL,
    op TEXT NOT NULL,
    at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TRIGGER engine_terms_ins AFTER INSERT ON engine_terms BEGIN
    INSERT INTO engine_changes (content_type, item_id, op) VALUES (NEW.vocabulary, NEW.id, 'INSERT');
END;
CREATE TRIGGER engine_terms_upd AFTER UPDATE ON engine_terms BEGIN
    INSERT INTO engine_changes (content_type, item_id, op) VALUES (NEW.vocabulary, NEW.id, 'UPDATE');
END;
CREATE TRIGGER engine_terms_del AFTER DELETE ON engine_terms BEGIN
    INSERT INTO engine_changes (content_type, item_id, op) VALUES (OLD.vocabulary, OLD.id, 'DELETE');
END;

-- Term assignments change the item they belong to.
CREATE TRIGGER engine_item_terms_ins AFTER INSERT ON engine_item_terms BEGIN
    INSERT INTO engine_changes (content_type, item_id, op) VALUES (NEW.content_type, NEW.item_id, 'UPDATE');
END;
CREATE TRIGGER engine_item_terms_del AFTER DELETE ON engine_item_terms BEGIN
    INSERT INTO engine_changes (content_type, item_id, op) VALUES (OLD.content_type, OLD.item_id, 'UPDATE');
END;
