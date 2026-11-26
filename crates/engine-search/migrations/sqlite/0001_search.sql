-- Wrought `search` feature, SQLite: the documents, and an FTS5 index over
-- them (external content, kept in step by triggers). Porter stemming
-- (English) on Unicode words, accents folded.
CREATE TABLE engine_search (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    content_type TEXT NOT NULL,
    item_id INTEGER NOT NULL,
    title TEXT NOT NULL DEFAULT '',
    body TEXT NOT NULL DEFAULT '',
    path TEXT,
    published_at TEXT,
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (content_type, item_id)
);

CREATE VIRTUAL TABLE engine_search_fts USING fts5(
    title, body,
    content = 'engine_search', content_rowid = 'id',
    tokenize = 'porter unicode61 remove_diacritics 2'
);

CREATE TRIGGER engine_search_ai AFTER INSERT ON engine_search BEGIN
    INSERT INTO engine_search_fts (rowid, title, body) VALUES (NEW.id, NEW.title, NEW.body);
END;
CREATE TRIGGER engine_search_ad AFTER DELETE ON engine_search BEGIN
    INSERT INTO engine_search_fts (engine_search_fts, rowid, title, body)
        VALUES ('delete', OLD.id, OLD.title, OLD.body);
END;
CREATE TRIGGER engine_search_au AFTER UPDATE ON engine_search BEGIN
    INSERT INTO engine_search_fts (engine_search_fts, rowid, title, body)
        VALUES ('delete', OLD.id, OLD.title, OLD.body);
    INSERT INTO engine_search_fts (rowid, title, body) VALUES (NEW.id, NEW.title, NEW.body);
END;
