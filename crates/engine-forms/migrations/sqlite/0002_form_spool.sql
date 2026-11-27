-- Form posts kept in the on-disk spool while the database was unavailable
-- (DECISIONS.md, "Survive a sleeping database"). Same as PostgreSQL.
ALTER TABLE engine_form_submissions ADD COLUMN idem_key TEXT;
CREATE UNIQUE INDEX engine_form_submissions_idem ON engine_form_submissions (idem_key)
    WHERE idem_key IS NOT NULL;

CREATE TABLE engine_form_replays (
    key TEXT PRIMARY KEY,
    replayed_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
