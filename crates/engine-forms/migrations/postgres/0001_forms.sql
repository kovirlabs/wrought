-- Wrought `forms` feature: forms made in the admin, stored submissions, and
-- the event log behind the spam guard's rate limits.

-- Forms defined in the admin (forms in a site's forms/*.toml are files and
-- are not stored here). `definition` has the same shape as a form file.
CREATE TABLE engine_forms (
    name text PRIMARY KEY CHECK (name ~ '^[a-z][a-z0-9_-]{0,39}$'),
    definition jsonb NOT NULL,
    version integer NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE engine_form_submissions (
    id bigserial PRIMARY KEY,
    form text NOT NULL,
    -- Field name -> submitted value (validated).
    data jsonb NOT NULL,
    -- Set for forms with `unique = "<field>"` (e.g. a newsletter's email):
    -- the same value is stored once.
    unique_key text,
    -- Signed-in submitter, when the `auth` feature is on (no foreign key:
    -- forms do not need accounts).
    user_id bigint,
    ip text,
    user_agent text,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX engine_form_submissions_form ON engine_form_submissions (form, id DESC);
CREATE INDEX engine_form_submissions_user ON engine_form_submissions (user_id, id DESC)
    WHERE user_id IS NOT NULL;
CREATE UNIQUE INDEX engine_form_submissions_unique ON engine_form_submissions (form, unique_key)
    WHERE unique_key IS NOT NULL;

-- Accepted submissions and refused spam, per form and client address.
CREATE TABLE engine_form_events (
    id bigserial PRIMARY KEY,
    form text NOT NULL,
    kind text NOT NULL CHECK (kind IN ('accepted', 'spam')),
    key text NOT NULL,
    reason text,
    at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX engine_form_events_key ON engine_form_events (form, key, at);

-- Keys the engine makes for itself (e.g. signing the time-trap stamp on
-- forms). Not for secrets that must survive a database leak: those come
-- from the environment (ENGINE_SECRET_KEY).
CREATE TABLE engine_keys (
    name text PRIMARY KEY,
    value bytea NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
