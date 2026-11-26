//! The `api` feature's statements (see `engine_db::sql`).

engine_db::statements! {
    INSERT = "INSERT INTO engine_api_tokens (user_id, name, token_hash, prefix, scopes, created_at, expires_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id";
    BY_HASH = "SELECT t.id, t.user_id, t.name, t.prefix, t.scopes, t.created_at, t.expires_at,
            t.last_used_at, t.revoked_at
        FROM engine_api_tokens t JOIN engine_users u ON u.id = t.user_id
        WHERE t.token_hash = $1 AND t.revoked_at IS NULL
          AND (t.expires_at IS NULL OR t.expires_at > $2) AND u.active";
    OF_USER = "SELECT id, user_id, name, prefix, scopes, created_at, expires_at, last_used_at,
            revoked_at
        FROM engine_api_tokens WHERE user_id = $1 ORDER BY id DESC";
    EVERY = "SELECT id, user_id, name, prefix, scopes, created_at, expires_at, last_used_at,
            revoked_at
        FROM engine_api_tokens ORDER BY id DESC LIMIT $1";
    GET = "SELECT id, user_id, name, prefix, scopes, created_at, expires_at, last_used_at,
            revoked_at
        FROM engine_api_tokens WHERE id = $1";
    REVOKE = "UPDATE engine_api_tokens SET revoked_at = $2 WHERE id = $1 AND revoked_at IS NULL";
    TOUCH = "UPDATE engine_api_tokens SET last_used_at = $2
        WHERE id = $1 AND (last_used_at IS NULL OR last_used_at < $3)";
}
