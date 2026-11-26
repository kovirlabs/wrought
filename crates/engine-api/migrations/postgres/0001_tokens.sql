-- Wrought `api` feature: API tokens. Only the SHA-256 of a token is
-- stored; `prefix` (its first characters) lets people tell tokens apart.
CREATE TABLE engine_api_tokens (
    id bigserial PRIMARY KEY,
    user_id bigint NOT NULL REFERENCES engine_users (id) ON DELETE CASCADE,
    name text NOT NULL CHECK (length(name) BETWEEN 1 AND 100),
    token_hash bytea NOT NULL UNIQUE,
    prefix text NOT NULL,
    -- ["read"], ["read", "write"], or per type: ["read:article", "write:article"].
    scopes jsonb NOT NULL DEFAULT '["read"]',
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz,
    last_used_at timestamptz,
    revoked_at timestamptz
);
CREATE INDEX engine_api_tokens_user ON engine_api_tokens (user_id, id DESC);
