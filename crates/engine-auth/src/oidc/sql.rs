//! The `oidc` feature's statements (see `engine_db::sql`).

engine_db::statements! {
    // ----- sign-in attempts in progress ------------------------------------------
    INSERT_STATE = "INSERT INTO engine_oidc_states
            (token_hash, provider, state, nonce, verifier, purpose, user_id, next,
             created_at, expires_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)";
    PRUNE_STATES = "DELETE FROM engine_oidc_states WHERE expires_at < $1";
    /// Single use: the row is gone once read.
    CLAIM_STATE = "DELETE FROM engine_oidc_states WHERE token_hash = $1 AND expires_at > $2
        RETURNING provider, state, nonce, verifier, purpose, user_id, next";

    // ----- linked identities ---------------------------------------------------------
    IDENTITY_USER = "SELECT user_id FROM engine_oidc_identities
        WHERE issuer = $1 AND subject = $2";
    INSERT_IDENTITY = "INSERT INTO engine_oidc_identities
            (user_id, provider, issuer, subject, email, created_at, last_used_at)
        VALUES ($1, $2, $3, $4, $5, $6, $6)
        ON CONFLICT DO NOTHING RETURNING id";
    TOUCH_IDENTITY = "UPDATE engine_oidc_identities SET last_used_at = $3, email = $4
        WHERE issuer = $1 AND subject = $2";
    IDENTITIES_OF = "SELECT provider, issuer, subject, email, created_at, last_used_at
        FROM engine_oidc_identities WHERE user_id = $1 ORDER BY provider, issuer";
    ISSUER_LINKED = "SELECT count(*) FROM engine_oidc_identities
        WHERE user_id = $1 AND issuer = $2";
    DELETE_IDENTITY = "DELETE FROM engine_oidc_identities WHERE user_id = $1 AND issuer = $2";
    /// Takes the account's row lock (Postgres) so two unlinks of one
    /// account run one after the other.
    LOCK_USER = "UPDATE engine_users SET updated_at = updated_at WHERE id = $1";
    DELETE_USER = "DELETE FROM engine_users WHERE id = $1";
}
