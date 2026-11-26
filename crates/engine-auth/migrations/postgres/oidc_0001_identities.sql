-- Wrought `oidc` feature: single sign-on through OpenID Connect providers.
--
-- An identity is the provider's (issuer, subject) pair, never an email
-- address: the address a provider reports is kept for display only. One
-- identity per issuer per account.
CREATE TABLE engine_oidc_identities (
    id bigserial PRIMARY KEY,
    user_id bigint NOT NULL REFERENCES engine_users (id) ON DELETE CASCADE,
    -- The provider's `name` in site.toml when it was linked.
    provider text NOT NULL,
    issuer text NOT NULL,
    subject text NOT NULL CHECK (length(subject) BETWEEN 1 AND 255),
    email text NOT NULL DEFAULT '',
    created_at timestamptz NOT NULL DEFAULT now(),
    last_used_at timestamptz,
    UNIQUE (issuer, subject),
    UNIQUE (user_id, issuer)
);

-- Sign-ins in progress (at most ten minutes): the browser holds a random
-- token in a cookie, this table its SHA-256 with the state, nonce and PKCE
-- verifier sent to the provider. Rows are deleted when used.
CREATE TABLE engine_oidc_states (
    token_hash bytea PRIMARY KEY,
    provider text NOT NULL,
    state text NOT NULL,
    nonce text NOT NULL,
    verifier text NOT NULL,
    -- `link`: a signed-in account (user_id) adds this provider.
    purpose text NOT NULL CHECK (purpose IN ('login', 'link')),
    user_id bigint REFERENCES engine_users (id) ON DELETE CASCADE,
    next text NOT NULL DEFAULT '/',
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL
);
CREATE INDEX engine_oidc_states_expires ON engine_oidc_states (expires_at);
