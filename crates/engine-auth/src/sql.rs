//! The `auth` and `roles` features' statements (see `engine_db::sql`).
//! Times are bound by the caller: `now` and cut-offs are computed in Rust.

engine_db::statements! {
    // ----- accounts ------------------------------------------------------------
    USER_BY_ID = "SELECT id, email, name, active, totp_secret, version, created_at,
            last_login_at, email_verified_at
        FROM engine_users WHERE id = $1";
    USER_ID_BY_EMAIL = "SELECT id FROM engine_users WHERE email = $1";
    USERS = "SELECT id, email, name, active, totp_secret, version, created_at,
            last_login_at, email_verified_at
        FROM engine_users ORDER BY email";
    USER_COUNT = "SELECT count(*) FROM engine_users";
    ACTIVE_COUNT = "SELECT count(*) FROM engine_users WHERE active";
    INSERT_USER = "INSERT INTO engine_users (email, name, password_hash, email_verified_at)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (email) DO NOTHING RETURNING id";
    OTHER_WITH_EMAIL = "SELECT id FROM engine_users WHERE email = $1 AND id <> $2";
    UPDATE_USER = "UPDATE engine_users SET
            email = COALESCE($3, email), name = COALESCE($4, name),
            active = COALESCE($5, active),
            version = version + 1, updated_at = $6
        WHERE id = $1 AND version = $2 RETURNING version";
    USER_VERSION = "SELECT version FROM engine_users WHERE id = $1";
    DELETE_SESSIONS_OF = "DELETE FROM engine_sessions WHERE user_id = $1";
    SET_PASSWORD = "UPDATE engine_users SET password_hash = $2, updated_at = $3 WHERE id = $1";
    DELETE_OTHER_SESSIONS = "DELETE FROM engine_sessions WHERE user_id = $1 AND token_hash <> $2";
    VOID_RESETS = "UPDATE engine_password_resets SET used_at = $2
        WHERE user_id = $1 AND used_at IS NULL";
    PASSWORD_HASH = "SELECT password_hash FROM engine_users WHERE id = $1";

    // ----- rate limits -----------------------------------------------------------
    RECENT_EVENTS = "SELECT count(*) FROM engine_auth_events
        WHERE kind = $1 AND key = $2 AND at > $3";
    INSERT_EVENT = "INSERT INTO engine_auth_events (kind, key, at) VALUES ($1, $2, $3)";
    PRUNE_EVENTS = "DELETE FROM engine_auth_events WHERE at < $1";
    CLEAR_EVENTS = "DELETE FROM engine_auth_events WHERE kind = $1 AND key = $2";

    // ----- sign-in -----------------------------------------------------------------
    LOGIN_ROW = "SELECT id, password_hash, active FROM engine_users WHERE email = $1";
    TOTP_ROW = "SELECT totp_secret, totp_last_step FROM engine_users WHERE id = $1";
    CLAIM_TOTP_STEP = "UPDATE engine_users SET totp_last_step = $2
        WHERE id = $1 AND (totp_last_step IS NULL OR totp_last_step < $2)";
    ENABLE_TOTP = "UPDATE engine_users SET totp_secret = $2, totp_last_step = $3,
            version = version + 1, updated_at = $4 WHERE id = $1";
    TOTP_SECRETS = "SELECT id, totp_secret FROM engine_users WHERE totp_secret IS NOT NULL";
    SEAL_TOTP = "UPDATE engine_users SET totp_secret = $2 WHERE id = $1 AND totp_secret = $3";
    DISABLE_TOTP = "UPDATE engine_users SET totp_secret = NULL, totp_last_step = NULL,
            version = version + 1, updated_at = $2 WHERE id = $1";

    // ----- sessions ------------------------------------------------------------------
    INSERT_SESSION = "INSERT INTO engine_sessions
            (token_hash, user_id, csrf_token, stage, created_at, last_seen_at, expires_at, ip, user_agent)
        VALUES ($1, $2, $3, $4, $5, $5, $6, $7, $8)";
    TOUCH_LOGIN = "UPDATE engine_users SET last_login_at = $2 WHERE id = $1";
    PRUNE_SESSIONS = "DELETE FROM engine_sessions WHERE expires_at < $1";
    SESSION = "SELECT s.user_id, s.csrf_token, s.stage, (s.last_seen_at < $4) AS stale
        FROM engine_sessions s JOIN engine_users u ON u.id = s.user_id
        WHERE s.token_hash = $1 AND s.expires_at > $2 AND u.active
          AND s.last_seen_at > $3";
    TOUCH_SESSION = "UPDATE engine_sessions SET last_seen_at = $2 WHERE token_hash = $1";
    DELETE_SESSION = "DELETE FROM engine_sessions WHERE token_hash = $1";

    // ----- password reset ------------------------------------------------------------
    INSERT_RESET = "INSERT INTO engine_password_resets (token_hash, user_id, created_at, expires_at)
        VALUES ($1, $2, $3, $4)";
    RESET_USER = "SELECT r.user_id FROM engine_password_resets r
        JOIN engine_users u ON u.id = r.user_id
        WHERE r.token_hash = $1 AND r.used_at IS NULL AND r.expires_at > $2 AND u.active";
    CLAIM_RESET = "UPDATE engine_password_resets SET used_at = $2
        WHERE token_hash = $1 AND used_at IS NULL AND expires_at > $2";

    // ----- email verification ----------------------------------------------------------
    INSERT_VERIFICATION = "INSERT INTO engine_email_verifications
            (token_hash, user_id, created_at, expires_at) VALUES ($1, $2, $3, $4)";
    CLAIM_VERIFICATION = "UPDATE engine_email_verifications SET used_at = $2
        WHERE token_hash = $1 AND used_at IS NULL AND expires_at > $2
        RETURNING user_id";
    MARK_VERIFIED = "UPDATE engine_users SET email_verified_at = $2, version = version + 1,
            updated_at = $2
        WHERE id = $1 AND email_verified_at IS NULL";
    RECENT_VERIFICATIONS = "SELECT count(*) FROM engine_email_verifications
        WHERE user_id = $1 AND created_at > $2";

    // ----- roles -------------------------------------------------------------------------
    USER_ROLES = "SELECT role FROM engine_user_roles WHERE user_id = $1 ORDER BY role";
    KNOWN_ROLES = {
        postgres: "SELECT name FROM engine_roles WHERE name = ANY($1)",
        sqlite: "SELECT name FROM engine_roles WHERE name IN (SELECT value FROM json_each($1))",
    };
    CLEAR_USER_ROLES = "DELETE FROM engine_user_roles WHERE user_id = $1";
    INSERT_USER_ROLES = {
        postgres: "INSERT INTO engine_user_roles (user_id, role) SELECT $1, unnest($2::text[])",
        sqlite: "INSERT INTO engine_user_roles (user_id, role) SELECT $1, value FROM json_each($2)",
    };
    ACTIVE_ADMINS = "SELECT count(DISTINCT u.id) FROM engine_users u
        JOIN engine_user_roles ur ON ur.user_id = u.id
        JOIN engine_roles r ON r.name = ur.role
        WHERE u.active AND r.is_admin";
    ROLES_OF_USER = "SELECT r.name, r.is_admin, r.grants FROM engine_roles r
        JOIN engine_user_roles ur ON ur.role = r.name
        WHERE ur.user_id = $1 ORDER BY r.name";
    ROLES = "SELECT r.name, r.label, r.is_admin, r.grants, r.version,
            (SELECT count(*) FROM engine_user_roles ur WHERE ur.role = r.name) AS users
        FROM engine_roles r ORDER BY r.is_admin DESC, r.name";
    INSERT_ROLE = "INSERT INTO engine_roles (name, label, grants) VALUES ($1, $2, $3)
        ON CONFLICT (name) DO NOTHING";
    UPDATE_ROLE = "UPDATE engine_roles SET label = $3, grants = $4, version = version + 1,
            updated_at = $5
        WHERE name = $1 AND version = $2 AND NOT is_admin RETURNING version";
    DELETE_ROLE = "DELETE FROM engine_roles WHERE name = $1 AND version = $2 AND NOT is_admin";
}
