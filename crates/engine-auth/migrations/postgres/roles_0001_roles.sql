-- Wrought `roles` feature: roles with grants, and which users hold them.
--
-- `grants` (JSON):
--   { "types": { "*": {...}, "article": {"view": true, "create": true,
--                "edit": true, "delete": false, "publish": false,
--                "fields": {"author": "read", "notes": "none"}} },
--     "taxonomy": true, "users": false }
-- A type's own entry replaces "*" for that type. Fields not listed are
-- writable by anyone who may edit the type. `is_admin` roles may do
-- everything and cannot be edited.

CREATE TABLE engine_roles (
    name text PRIMARY KEY CHECK (name ~ '^[a-z][a-z0-9_]{0,39}$'),
    label text NOT NULL CHECK (length(label) BETWEEN 1 AND 100),
    is_admin boolean NOT NULL DEFAULT false,
    grants jsonb NOT NULL DEFAULT '{}',
    version integer NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE engine_user_roles (
    user_id bigint NOT NULL REFERENCES engine_users (id) ON DELETE CASCADE,
    role text NOT NULL REFERENCES engine_roles (name) ON DELETE CASCADE,
    PRIMARY KEY (user_id, role)
);

INSERT INTO engine_roles (name, label, is_admin, grants) VALUES
    ('admin', 'Administrator', true, '{}'),
    ('editor', 'Editor', false,
     '{"types": {"*": {"view": true, "create": true, "edit": true, "delete": true, "publish": true}}, "taxonomy": true}');

-- Without `roles` every account was an administrator; keep it that way for
-- accounts that existed before roles were switched on.
INSERT INTO engine_user_roles (user_id, role) SELECT id, 'admin' FROM engine_users;
