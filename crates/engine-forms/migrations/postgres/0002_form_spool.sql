-- Form posts kept in the on-disk spool while the database was unavailable
-- (DECISIONS.md, "Survive a sleeping database").

-- Every post carries a random key; a post written to the spool after an
-- ambiguous failure, whose first insert did commit, is found by it.
ALTER TABLE engine_form_submissions ADD COLUMN idem_key text;
CREATE UNIQUE INDEX engine_form_submissions_idem ON engine_form_submissions (idem_key)
    WHERE idem_key IS NOT NULL;

-- Spooled posts already replayed: claimed in the same transaction as the
-- submission and its queued mail, so a replay runs once.
CREATE TABLE engine_form_replays (
    key text PRIMARY KEY,
    replayed_at timestamptz NOT NULL DEFAULT now()
);
