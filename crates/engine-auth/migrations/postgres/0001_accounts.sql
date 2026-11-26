-- Wrought `auth` feature: accounts, server-side sessions, password resets
-- and the event log behind login rate limits.

CREATE TABLE engine_users (
    id bigserial PRIMARY KEY,
    -- Stored lower-case; the login form lower-cases what it is given.
    email text NOT NULL UNIQUE
        CHECK (email = lower(email) AND length(email) BETWEEN 3 AND 254 AND position('@' in email) > 1),
    name text NOT NULL DEFAULT '' CHECK (length(name) <= 100),
    -- Argon2id, PHC string format.
    password_hash text NOT NULL,
    active boolean NOT NULL DEFAULT true,
    -- Base32 TOTP secret when two-factor sign-in is on.
    totp_secret text,
    -- Last accepted TOTP time step, so a code cannot be replayed.
    totp_last_step bigint,
    version integer NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    last_login_at timestamptz
);

-- The cookie holds a random token; only its SHA-256 is stored, so a copy of
-- this table cannot be used to sign in.
CREATE TABLE engine_sessions (
    token_hash bytea PRIMARY KEY,
    user_id bigint NOT NULL REFERENCES engine_users (id) ON DELETE CASCADE,
    csrf_token text NOT NULL,
    -- `totp`: password checked, second factor still due.
    stage text NOT NULL DEFAULT 'full' CHECK (stage IN ('full', 'totp')),
    created_at timestamptz NOT NULL DEFAULT now(),
    last_seen_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    ip text,
    user_agent text
);
CREATE INDEX engine_sessions_user ON engine_sessions (user_id);

CREATE TABLE engine_password_resets (
    token_hash bytea PRIMARY KEY,
    user_id bigint NOT NULL REFERENCES engine_users (id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    used_at timestamptz
);
CREATE INDEX engine_password_resets_user ON engine_password_resets (user_id);

-- Failed sign-ins and reset requests, by account and by client address.
CREATE TABLE engine_auth_events (
    id bigserial PRIMARY KEY,
    kind text NOT NULL,
    key text NOT NULL,
    at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX engine_auth_events_key ON engine_auth_events (kind, key, at);
