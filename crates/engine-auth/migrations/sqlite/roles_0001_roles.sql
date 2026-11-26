-- Wrought `roles` feature, SQLite (same shape as the Postgres migration).

CREATE TABLE engine_roles (
    name TEXT PRIMARY KEY CHECK (
        length(name) BETWEEN 1 AND 40 AND name GLOB '[a-z]*' AND name NOT GLOB '*[^a-z0-9_]*'),
    label TEXT NOT NULL CHECK (length(label) BETWEEN 1 AND 100),
    is_admin BOOLEAN NOT NULL DEFAULT 0,
    grants TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(grants)),
    version INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE engine_user_roles (
    user_id INTEGER NOT NULL REFERENCES engine_users (id) ON DELETE CASCADE,
    role TEXT NOT NULL REFERENCES engine_roles (name) ON DELETE CASCADE,
    PRIMARY KEY (user_id, role)
);

INSERT INTO engine_roles (name, label, is_admin, grants) VALUES
    ('admin', 'Administrator', 1, '{}'),
    ('editor', 'Editor', 0,
     '{"types": {"*": {"view": true, "create": true, "edit": true, "delete": true, "publish": true}}, "taxonomy": true}');

INSERT INTO engine_user_roles (user_id, role) SELECT id, 'admin' FROM engine_users;
