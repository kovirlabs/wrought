-- Wrought `auth` v0.5: email verification for registrations.
--
-- `email_verified_at` is set when the owner follows the link sent at
-- registration (or by an administrator). Accounts that existed before count
-- as verified: they were made by administrators or the CLI.
ALTER TABLE engine_users ADD COLUMN email_verified_at timestamptz;
UPDATE engine_users SET email_verified_at = created_at;

-- Single-use verification links (only the SHA-256 of the token is stored).
CREATE TABLE engine_email_verifications (
    token_hash bytea PRIMARY KEY,
    user_id bigint NOT NULL REFERENCES engine_users (id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    used_at timestamptz
);
CREATE INDEX engine_email_verifications_user ON engine_email_verifications (user_id);
