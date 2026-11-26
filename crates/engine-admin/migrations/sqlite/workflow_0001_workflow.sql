-- Wrought `workflow` feature, SQLite (same shape as the Postgres migration).
CREATE TABLE engine_workflow (
    item_type TEXT NOT NULL,
    item_id INTEGER NOT NULL,
    state TEXT NOT NULL DEFAULT 'review' CHECK (state IN ('review')),
    submitted_by INTEGER REFERENCES engine_users (id) ON DELETE SET NULL,
    submitted_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    note TEXT NOT NULL DEFAULT '' CHECK (length(note) <= 2000),
    PRIMARY KEY (item_type, item_id)
);

CREATE TABLE engine_workflow_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    item_type TEXT NOT NULL,
    item_id INTEGER NOT NULL,
    action TEXT NOT NULL CHECK (action IN ('submit', 'approve', 'reject', 'withdraw')),
    user_id INTEGER REFERENCES engine_users (id) ON DELETE SET NULL,
    note TEXT NOT NULL DEFAULT '',
    at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE INDEX engine_workflow_log_item ON engine_workflow_log (item_type, item_id, id);
