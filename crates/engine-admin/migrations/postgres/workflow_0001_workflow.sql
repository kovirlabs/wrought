-- Wrought `workflow` feature: draft -> review -> published.
--
-- An item under review is still a draft (`status = 'draft'` in its content
-- table); this table marks it as waiting for a reviewer, so the generated
-- content tables need no change.
CREATE TABLE engine_workflow (
    item_type text NOT NULL,
    item_id bigint NOT NULL,
    state text NOT NULL DEFAULT 'review' CHECK (state IN ('review')),
    submitted_by bigint REFERENCES engine_users (id) ON DELETE SET NULL,
    submitted_at timestamptz NOT NULL DEFAULT now(),
    note text NOT NULL DEFAULT '' CHECK (length(note) <= 2000),
    PRIMARY KEY (item_type, item_id)
);

-- Who did what, for the item's history and notifications.
CREATE TABLE engine_workflow_log (
    id bigserial PRIMARY KEY,
    item_type text NOT NULL,
    item_id bigint NOT NULL,
    action text NOT NULL CHECK (action IN ('submit', 'approve', 'reject', 'withdraw')),
    user_id bigint REFERENCES engine_users (id) ON DELETE SET NULL,
    note text NOT NULL DEFAULT '',
    at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX engine_workflow_log_item ON engine_workflow_log (item_type, item_id, id);
