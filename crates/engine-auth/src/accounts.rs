//! Accounts, sign-in, sessions, password resets and rate limits.

use anyhow::Context;
use engine_db::Db;
use serde::Serialize;

use crate::access::Access;
use crate::{Auth, AuthError, normalize_email, password, secret, totp, valid_email};

/// An account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct User {
    pub id: i64,
    pub email: String,
    pub name: String,
    pub active: bool,
    pub totp_enabled: bool,
    pub version: i32,
    pub created_at: String,
    pub last_login_at: Option<String>,
    /// Role names (always empty without the `roles` feature).
    pub roles: Vec<String>,
}

impl User {
    /// Name to show: the name, else the email address.
    pub fn display_name(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.email
        } else {
            &self.name
        }
    }
}

/// How far a session got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Signed in.
    Full,
    /// Password checked; the TOTP code is still due.
    Totp,
}

impl Stage {
    fn as_str(self) -> &'static str {
        match self {
            Stage::Full => "full",
            Stage::Totp => "totp",
        }
    }
}

/// A valid session with its user.
#[derive(Debug, Clone)]
pub struct Session {
    pub(crate) token_hash: Vec<u8>,
    pub user: User,
    /// Token every form of this session must carry.
    pub csrf: String,
    pub stage: Stage,
    pub access: Access,
}

impl Session {
    pub fn is_full(&self) -> bool {
        self.stage == Stage::Full
    }
}

/// Outcome of a password check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginResult {
    Ok(User),
    /// Wrong email or password (deliberately not saying which).
    Invalid,
    /// Too many failures recently; nothing was checked.
    Limited,
}

/// Changes to an account; `None` leaves a value alone.
#[derive(Debug, Clone, Default)]
pub struct UserChanges {
    pub email: Option<String>,
    pub name: Option<String>,
    pub active: Option<bool>,
    /// Replace the account's roles (`roles` feature).
    pub roles: Option<Vec<String>>,
}

const FAILURE: &str = "login_failure";
const RESET: &str = "reset_request";
const REGISTER: &str = "register";
/// Window of the rate limits.
const WINDOW_MINUTES: i32 = 15;
pub(crate) const RESET_MINUTES: u32 = 60;

fn pg(db: &Db) -> &sqlx::PgPool {
    match db {
        Db::Postgres(p) => p,
    }
}

struct UserRow {
    id: i64,
    email: String,
    name: String,
    active: bool,
    totp_secret: Option<String>,
    version: i32,
    created_at: String,
    last_login_at: Option<String>,
}

impl UserRow {
    fn into_user(self, roles: Vec<String>) -> User {
        User {
            id: self.id,
            email: self.email,
            name: self.name,
            active: self.active,
            totp_enabled: self.totp_secret.is_some(),
            version: self.version,
            created_at: self.created_at,
            last_login_at: self.last_login_at,
            roles,
        }
    }
}

fn check_name(name: &str, errs: &mut Vec<(String, String)>) {
    if name.chars().count() > 100 {
        errs.push(("name".into(), "is longer than 100 characters".into()));
    }
    if name.chars().any(char::is_control) {
        errs.push(("name".into(), "must not contain control characters".into()));
    }
}

impl Auth {
    fn pool(&self) -> &sqlx::PgPool {
        pg(self.db())
    }

    // ----- accounts ---------------------------------------------------------

    async fn roles_of(&self, user_id: i64) -> anyhow::Result<Vec<String>> {
        #[cfg(feature = "roles")]
        {
            crate::roles::user_roles(self.pool(), user_id).await
        }
        #[cfg(not(feature = "roles"))]
        {
            let _ = user_id;
            Ok(Vec::new())
        }
    }

    async fn user_row(&self, id: i64) -> anyhow::Result<Option<UserRow>> {
        Ok(sqlx::query_as!(
            UserRow,
            r#"SELECT id, email, name, active, totp_secret, version,
                      to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS "created_at!",
                      to_char(last_login_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS last_login_at
               FROM engine_users WHERE id = $1"#,
            id
        )
        .fetch_optional(self.pool())
        .await?)
    }

    pub async fn user(&self, id: i64) -> anyhow::Result<Option<User>> {
        match self.user_row(id).await? {
            Some(r) => {
                let roles = self.roles_of(r.id).await?;
                Ok(Some(r.into_user(roles)))
            }
            None => Ok(None),
        }
    }

    pub async fn user_by_email(&self, email: &str) -> anyhow::Result<Option<User>> {
        let id = sqlx::query_scalar!(
            "SELECT id FROM engine_users WHERE email = $1",
            normalize_email(email)
        )
        .fetch_optional(self.pool())
        .await?;
        match id {
            Some(id) => self.user(id).await,
            None => Ok(None),
        }
    }

    /// Every account, by email.
    pub async fn users(&self) -> anyhow::Result<Vec<User>> {
        let rows = sqlx::query_as!(
            UserRow,
            r#"SELECT id, email, name, active, totp_secret, version,
                      to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS "created_at!",
                      to_char(last_login_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS last_login_at
               FROM engine_users ORDER BY email"#
        )
        .fetch_all(self.pool())
        .await?;
        let mut out = Vec::new();
        for r in rows {
            let roles = self.roles_of(r.id).await?;
            out.push(r.into_user(roles));
        }
        Ok(out)
    }

    pub async fn user_count(&self) -> anyhow::Result<i64> {
        Ok(
            sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM engine_users"#)
                .fetch_one(self.pool())
                .await?,
        )
    }

    /// Active accounts with administrator rights.
    pub async fn admin_count(&self) -> anyhow::Result<i64> {
        #[cfg(feature = "roles")]
        {
            crate::roles::active_admin_count(self.pool()).await
        }
        #[cfg(not(feature = "roles"))]
        {
            Ok(
                sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM engine_users WHERE active"#)
                    .fetch_one(self.pool())
                    .await?,
            )
        }
    }

    /// Create an account. Without a password it gets an unusable one, and
    /// its owner sets one through a reset link.
    pub async fn create_user(
        &self,
        email: &str,
        name: &str,
        password: Option<&str>,
        roles: &[String],
    ) -> Result<User, AuthError> {
        let email = normalize_email(email);
        let name = name.trim().to_string();
        let mut errs = Vec::new();
        if !valid_email(&email) {
            errs.push(("email".into(), "is not a valid email address".into()));
        }
        check_name(&name, &mut errs);
        if let Some(p) = password
            && let Some(m) = password::check_policy(p, &email)
        {
            errs.push(("password".into(), m));
        }
        #[cfg(not(feature = "roles"))]
        if !roles.is_empty() {
            errs.push((
                "roles".into(),
                "need the `roles` feature, which is not in this build".into(),
            ));
        }
        if !errs.is_empty() {
            return Err(AuthError::Invalid(errs));
        }
        let hash = match password {
            Some(p) => {
                let p = p.to_string();
                tokio::task::spawn_blocking(move || password::hash(&p))
                    .await
                    .context("hashing")??
            }
            None => password::unusable_hash(),
        };
        let mut tx = self.pool().begin().await?;
        let id = sqlx::query_scalar!(
            r#"INSERT INTO engine_users (email, name, password_hash) VALUES ($1, $2, $3)
               ON CONFLICT (email) DO NOTHING RETURNING id"#,
            email,
            name,
            hash
        )
        .fetch_optional(&mut *tx)
        .await?;
        let Some(id) = id else {
            return Err(AuthError::invalid(
                "email",
                "is already used by another account",
            ));
        };
        #[cfg(feature = "roles")]
        crate::roles::set_user_roles(&mut tx, id, roles).await?;
        #[cfg(not(feature = "roles"))]
        let _ = roles;
        tx.commit().await?;
        self.user(id).await?.ok_or(AuthError::NotFound)
    }

    /// Registration (`[auth] registration = "open"`): create an account with
    /// the configured default roles and send the `welcome` message. Limited
    /// per client address (`max_registrations_per_address` per hour).
    pub async fn register(
        &self,
        email: &str,
        name: &str,
        password: &str,
        ip: Option<&str>,
    ) -> Result<User, AuthError> {
        if self.config().registration != crate::Registration::Open {
            return Err(AuthError::invalid("email", "registration is closed"));
        }
        if let Some(ip) = ip {
            let key = format!("ip:{ip}");
            if self.recent(REGISTER, &key, 60).await? >= self.config().max_registrations_per_address
            {
                return Err(AuthError::Limited);
            }
            self.record(REGISTER, &[key]).await?;
        }
        let roles = self.config().default_roles.clone();
        let user = self
            .create_user(email, name, Some(password), &roles)
            .await?;
        let msg = crate::notify::Message {
            to: user.email.clone(),
            to_name: user.name.clone(),
            template: "welcome".into(),
            data: serde_json::json!({}),
        };
        if let Err(e) = self.notifier().send(msg).await {
            eprintln!("auth: could not send the welcome message: {e:#}");
        }
        Ok(user)
    }

    /// Change an account the caller read at `expected_version`. Refuses to
    /// leave the site without an active administrator.
    pub async fn update_user(
        &self,
        id: i64,
        expected_version: i32,
        ch: &UserChanges,
    ) -> Result<User, AuthError> {
        let mut errs = Vec::new();
        let email = ch.email.as_deref().map(normalize_email);
        if let Some(e) = &email
            && !valid_email(e)
        {
            errs.push(("email".into(), "is not a valid email address".into()));
        }
        let name = ch.name.as_deref().map(|n| n.trim().to_string());
        if let Some(n) = &name {
            check_name(n, &mut errs);
        }
        #[cfg(not(feature = "roles"))]
        if ch.roles.is_some() {
            errs.push((
                "roles".into(),
                "need the `roles` feature, which is not in this build".into(),
            ));
        }
        if !errs.is_empty() {
            return Err(AuthError::Invalid(errs));
        }
        let mut tx = self.pool().begin().await?;
        if let Some(e) = &email {
            let other = sqlx::query_scalar!(
                "SELECT id FROM engine_users WHERE email = $1 AND id <> $2",
                e,
                id
            )
            .fetch_optional(&mut *tx)
            .await?;
            if other.is_some() {
                return Err(AuthError::invalid(
                    "email",
                    "is already used by another account",
                ));
            }
        }
        let updated = sqlx::query_scalar!(
            r#"UPDATE engine_users SET
                 email = COALESCE($3, email), name = COALESCE($4, name),
                 active = COALESCE($5, active),
                 version = version + 1, updated_at = now()
               WHERE id = $1 AND version = $2 RETURNING version"#,
            id,
            expected_version,
            email,
            name,
            ch.active
        )
        .fetch_optional(&mut *tx)
        .await?;
        if updated.is_none() {
            let current = sqlx::query_scalar!("SELECT version FROM engine_users WHERE id = $1", id)
                .fetch_optional(&mut *tx)
                .await?;
            return Err(match current {
                Some(current) => AuthError::Conflict {
                    expected: expected_version,
                    current,
                },
                None => AuthError::NotFound,
            });
        }
        #[cfg(feature = "roles")]
        if let Some(roles) = &ch.roles {
            crate::roles::set_user_roles(&mut tx, id, roles).await?;
        }
        if ch.active == Some(false) {
            sqlx::query!("DELETE FROM engine_sessions WHERE user_id = $1", id)
                .execute(&mut *tx)
                .await?;
        }
        // Checked inside the transaction, after the change.
        let admins: i64 = {
            #[cfg(feature = "roles")]
            {
                crate::roles::active_admin_count(&mut *tx).await?
            }
            #[cfg(not(feature = "roles"))]
            {
                sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM engine_users WHERE active"#)
                    .fetch_one(&mut *tx)
                    .await?
            }
        };
        if admins == 0 {
            return Err(AuthError::invalid(
                "roles",
                "would leave the site without an active administrator",
            ));
        }
        tx.commit().await?;
        self.user(id).await?.ok_or(AuthError::NotFound)
    }

    /// Set a new password (policy checked) and sign the account out
    /// everywhere except `keep_session`.
    pub async fn set_password(
        &self,
        id: i64,
        new_password: &str,
        keep_session: Option<&Session>,
    ) -> Result<(), AuthError> {
        let user = self.user(id).await?.ok_or(AuthError::NotFound)?;
        if let Some(m) = password::check_policy(new_password, &user.email) {
            return Err(AuthError::invalid("password", m));
        }
        let p = new_password.to_string();
        let hash = tokio::task::spawn_blocking(move || password::hash(&p))
            .await
            .context("hashing")??;
        let keep = keep_session
            .map(|s| s.token_hash.clone())
            .unwrap_or_default();
        let mut tx = self.pool().begin().await?;
        sqlx::query!(
            "UPDATE engine_users SET password_hash = $2, updated_at = now() WHERE id = $1",
            id,
            hash
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query!(
            "DELETE FROM engine_sessions WHERE user_id = $1 AND token_hash <> $2",
            id,
            keep
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query!(
            "UPDATE engine_password_resets SET used_at = now() WHERE user_id = $1 AND used_at IS NULL",
            id
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Check a password without signing in (re-authentication).
    pub async fn check_password(&self, id: i64, given: &str) -> anyhow::Result<bool> {
        let stored =
            sqlx::query_scalar!("SELECT password_hash FROM engine_users WHERE id = $1", id)
                .fetch_optional(self.pool())
                .await?
                .unwrap_or_default();
        let given = given.to_string();
        Ok(tokio::task::spawn_blocking(move || password::verify(&given, &stored)).await?)
    }

    pub async fn access(&self, user: &User) -> anyhow::Result<Access> {
        if !user.active {
            return Ok(Access::none());
        }
        #[cfg(feature = "roles")]
        {
            crate::roles::access_for(self.pool(), user.id).await
        }
        #[cfg(not(feature = "roles"))]
        {
            Ok(Access::full())
        }
    }

    // ----- rate limits -------------------------------------------------------

    async fn recent(&self, kind: &str, key: &str, minutes: i32) -> anyhow::Result<i64> {
        Ok(sqlx::query_scalar!(
            r#"SELECT count(*) AS "n!" FROM engine_auth_events
               WHERE kind = $1 AND key = $2 AND at > now() - make_interval(mins => $3)"#,
            kind,
            key,
            minutes
        )
        .fetch_one(self.pool())
        .await?)
    }

    async fn record(&self, kind: &str, keys: &[String]) -> anyhow::Result<()> {
        for k in keys {
            sqlx::query!(
                "INSERT INTO engine_auth_events (kind, key) VALUES ($1, $2)",
                kind,
                k
            )
            .execute(self.pool())
            .await?;
        }
        // Keep the table small.
        sqlx::query!("DELETE FROM engine_auth_events WHERE at < now() - interval '1 day'")
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Whether sign-in for `email` from `ip` is currently refused.
    pub async fn login_limited(&self, email: &str, ip: Option<&str>) -> anyhow::Result<bool> {
        let cfg = self.config();
        if self
            .recent(
                FAILURE,
                &format!("email:{}", normalize_email(email)),
                WINDOW_MINUTES,
            )
            .await?
            >= cfg.max_failures_per_account
        {
            return Ok(true);
        }
        if let Some(ip) = ip
            && self
                .recent(FAILURE, &format!("ip:{ip}"), WINDOW_MINUTES)
                .await?
                >= cfg.max_failures_per_address
        {
            return Ok(true);
        }
        Ok(false)
    }

    async fn record_failure(&self, email: &str, ip: Option<&str>) -> anyhow::Result<()> {
        let mut keys = vec![format!("email:{}", normalize_email(email))];
        if let Some(ip) = ip {
            keys.push(format!("ip:{ip}"));
        }
        self.record(FAILURE, &keys).await
    }

    // ----- sign-in -----------------------------------------------------------

    /// Check an email and password. Failures (and checks of unknown or
    /// disabled accounts) count towards the rate limits and take the same
    /// time as a real check.
    pub async fn login(
        &self,
        email: &str,
        given: &str,
        ip: Option<&str>,
    ) -> anyhow::Result<LoginResult> {
        let email = normalize_email(email);
        if self.login_limited(&email, ip).await? {
            return Ok(LoginResult::Limited);
        }
        let row = sqlx::query!(
            "SELECT id, password_hash, active FROM engine_users WHERE email = $1",
            email
        )
        .fetch_optional(self.pool())
        .await?;
        let (id, stored, active) = match row {
            Some(r) => (Some(r.id), r.password_hash, r.active),
            None => (None, password::dummy_hash().to_string(), false),
        };
        let given = given.to_string();
        let ok = tokio::task::spawn_blocking(move || password::verify(&given, &stored)).await?;
        match id {
            Some(id) if ok && active => {
                sqlx::query!(
                    "DELETE FROM engine_auth_events WHERE kind = $1 AND key = $2",
                    FAILURE,
                    format!("email:{email}")
                )
                .execute(self.pool())
                .await?;
                Ok(LoginResult::Ok(
                    self.user(id).await?.context("user vanished")?,
                ))
            }
            _ => {
                self.record_failure(&email, ip).await?;
                Ok(LoginResult::Invalid)
            }
        }
    }

    /// Check the TOTP code of a half-signed-in session. Wrong codes count as
    /// failed sign-ins.
    pub async fn check_totp(
        &self,
        session: &Session,
        code: &str,
        ip: Option<&str>,
    ) -> anyhow::Result<LoginResult> {
        let user = &session.user;
        if self.login_limited(&user.email, ip).await? {
            return Ok(LoginResult::Limited);
        }
        let row = sqlx::query!(
            "SELECT totp_secret, totp_last_step FROM engine_users WHERE id = $1",
            user.id
        )
        .fetch_one(self.pool())
        .await?;
        let plain = match row.totp_secret.as_deref() {
            Some(stored) => Some(self.totp_plain(user.id, stored)?),
            None => None,
        };
        let step = plain
            .as_deref()
            .and_then(|s| totp::verify(s, code, totp::current_step(), row.totp_last_step));
        let Some(step) = step else {
            self.record_failure(&user.email, ip).await?;
            return Ok(LoginResult::Invalid);
        };
        // Accept each step once, even under concurrent requests.
        let claimed = sqlx::query!(
            r#"UPDATE engine_users SET totp_last_step = $2
               WHERE id = $1 AND (totp_last_step IS NULL OR totp_last_step < $2)"#,
            user.id,
            step as i64
        )
        .execute(self.pool())
        .await?;
        if claimed.rows_affected() != 1 {
            self.record_failure(&user.email, ip).await?;
            return Ok(LoginResult::Invalid);
        }
        Ok(LoginResult::Ok(user.clone()))
    }

    /// Whether two-factor sign-in can be turned on: TOTP secrets are only
    /// stored encrypted, which needs the site key.
    pub fn totp_available(&self) -> bool {
        self.config().site_key.is_some()
    }

    /// A stored TOTP secret in plain base32.
    fn totp_plain(&self, user_id: i64, stored: &str) -> anyhow::Result<String> {
        if !crate::sitekey::is_sealed(stored) {
            // Stored before v0.4; encrypted by `encrypt_legacy_totp`.
            return Ok(stored.to_string());
        }
        match &self.config().site_key {
            Some(k) => k.open(user_id, stored),
            None => anyhow::bail!(
                "two-factor secrets are encrypted but no site key is set: set {} or [auth] \
                 secret_key_file",
                crate::sitekey::KEY_ENV
            ),
        }
    }

    /// Turn on two-factor sign-in with `secret` once `code` proves the
    /// authenticator has it. The secret is stored encrypted with the site
    /// key; without one this fails with a message saying how to set it.
    pub async fn enable_totp(
        &self,
        user_id: i64,
        secret_b32: &str,
        code: &str,
    ) -> anyhow::Result<bool> {
        let Some(key) = &self.config().site_key else {
            anyhow::bail!(
                "two-factor sign-in needs a site key to encrypt its secrets: set {} (32+ random \
                 characters) or [auth] secret_key_file, and restart",
                crate::sitekey::KEY_ENV
            );
        };
        let Some(step) = totp::verify(secret_b32, code, totp::current_step(), None) else {
            return Ok(false);
        };
        let sealed = key.seal(user_id, secret_b32)?;
        sqlx::query!(
            r#"UPDATE engine_users SET totp_secret = $2, totp_last_step = $3,
                 version = version + 1, updated_at = now() WHERE id = $1"#,
            user_id,
            sealed,
            step as i64
        )
        .execute(self.pool())
        .await?;
        Ok(true)
    }

    /// At start: check that two-factor secrets can be read, and encrypt
    /// any stored in plain text by earlier versions. Fails with a clear
    /// message when accounts use two-factor sign-in but no key is set.
    /// Returns how many secrets it encrypted.
    pub async fn check_totp_storage(&self) -> anyhow::Result<usize> {
        let rows = sqlx::query!(
            "SELECT id, totp_secret AS \"totp_secret!\" FROM engine_users WHERE totp_secret IS NOT NULL"
        )
        .fetch_all(self.pool())
        .await?;
        if rows.is_empty() {
            return Ok(0);
        }
        let Some(key) = &self.config().site_key else {
            anyhow::bail!(
                "{} account(s) use two-factor sign-in, but no site key is set to protect their \
                 secrets: set {} (32+ random characters, e.g. `openssl rand -base64 48`) or \
                 [auth] secret_key_file",
                rows.len(),
                crate::sitekey::KEY_ENV
            );
        };
        let mut n = 0;
        for r in rows {
            if crate::sitekey::is_sealed(&r.totp_secret) {
                key.open(r.id, &r.totp_secret)?;
                continue;
            }
            let sealed = key.seal(r.id, &r.totp_secret)?;
            sqlx::query!(
                "UPDATE engine_users SET totp_secret = $2 WHERE id = $1 AND totp_secret = $3",
                r.id,
                sealed,
                r.totp_secret
            )
            .execute(self.pool())
            .await?;
            n += 1;
        }
        Ok(n)
    }

    pub async fn disable_totp(&self, user_id: i64) -> anyhow::Result<()> {
        sqlx::query!(
            r#"UPDATE engine_users SET totp_secret = NULL, totp_last_step = NULL,
                 version = version + 1, updated_at = now() WHERE id = $1"#,
            user_id
        )
        .execute(self.pool())
        .await?;
        Ok(())
    }

    // ----- sessions ----------------------------------------------------------

    /// Start a session; returns the cookie token. Sign-in always starts a
    /// new session (no fixation).
    pub async fn start_session(
        &self,
        user_id: i64,
        stage: Stage,
        ip: Option<&str>,
        user_agent: &str,
    ) -> anyhow::Result<String> {
        let token = secret::token();
        let ua: String = user_agent.chars().take(300).collect();
        let hours = if stage == Stage::Full {
            self.config().session_hours as i32
        } else {
            0
        };
        // A half-signed-in session lives 10 minutes.
        let minutes = if stage == Stage::Full { 0 } else { 10 };
        sqlx::query!(
            r#"INSERT INTO engine_sessions (token_hash, user_id, csrf_token, stage, expires_at, ip, user_agent)
               VALUES ($1, $2, $3, $4, now() + make_interval(hours => $5, mins => $6), $7, $8)"#,
            secret::hash_token(&token),
            user_id,
            secret::token(),
            stage.as_str(),
            hours,
            minutes,
            ip,
            ua
        )
        .execute(self.pool())
        .await?;
        if stage == Stage::Full {
            sqlx::query!(
                "UPDATE engine_users SET last_login_at = now() WHERE id = $1",
                user_id
            )
            .execute(self.pool())
            .await?;
        }
        // Expired sessions are useless; drop them now and then.
        sqlx::query!("DELETE FROM engine_sessions WHERE expires_at < now()")
            .execute(self.pool())
            .await?;
        Ok(token)
    }

    /// The session for a cookie token, if it is valid (not expired, not
    /// idle too long, account active). Refreshes its last-seen time.
    pub async fn session(&self, token: &str) -> anyhow::Result<Option<Session>> {
        if !secret::is_token(token) {
            return Ok(None);
        }
        let hash = secret::hash_token(token);
        let row = sqlx::query!(
            r#"SELECT s.user_id, s.csrf_token, s.stage,
                      (s.last_seen_at < now() - interval '1 minute') AS "stale!"
               FROM engine_sessions s JOIN engine_users u ON u.id = s.user_id
               WHERE s.token_hash = $1 AND s.expires_at > now() AND u.active
                 AND s.last_seen_at > now() - make_interval(mins => $2)"#,
            hash,
            self.config().idle_minutes as i32
        )
        .fetch_optional(self.pool())
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        if row.stale {
            sqlx::query!(
                "UPDATE engine_sessions SET last_seen_at = now() WHERE token_hash = $1",
                hash
            )
            .execute(self.pool())
            .await?;
        }
        let Some(user) = self.user(row.user_id).await? else {
            return Ok(None);
        };
        let access = self.access(&user).await?;
        Ok(Some(Session {
            token_hash: hash,
            user,
            csrf: row.csrf_token,
            stage: if row.stage == "full" {
                Stage::Full
            } else {
                Stage::Totp
            },
            access,
        }))
    }

    pub async fn end_session(&self, session: &Session) -> anyhow::Result<()> {
        sqlx::query!(
            "DELETE FROM engine_sessions WHERE token_hash = $1",
            session.token_hash
        )
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Sign an account out everywhere.
    pub async fn end_all_sessions(&self, user_id: i64) -> anyhow::Result<()> {
        sqlx::query!("DELETE FROM engine_sessions WHERE user_id = $1", user_id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    // ----- password reset ----------------------------------------------------

    /// Make a single-use reset link for an account (CLI, admin, and
    /// [`Self::request_reset`]).
    pub async fn reset_link(&self, user_id: i64) -> anyhow::Result<String> {
        let token = secret::token();
        sqlx::query!(
            r#"INSERT INTO engine_password_resets (token_hash, user_id, expires_at)
               VALUES ($1, $2, now() + make_interval(mins => $3))"#,
            secret::hash_token(&token),
            user_id,
            RESET_MINUTES as i32
        )
        .execute(self.pool())
        .await?;
        Ok(format!(
            "{}/account/reset/confirm/?token={token}",
            self.public_url()
        ))
    }

    /// Send a reset link to `email`'s owner, if the account exists and is
    /// active. Answers the same either way (no account enumeration), and
    /// quietly drops requests over the limits (3 per account and 10 per
    /// address per hour).
    pub async fn request_reset(&self, email: &str, ip: Option<&str>) -> anyhow::Result<()> {
        let email = normalize_email(email);
        let key = format!("email:{email}");
        if self.recent(RESET, &key, 60).await? >= 3 {
            return Ok(());
        }
        if let Some(ip) = ip
            && self.recent(RESET, &format!("ip:{ip}"), 60).await? >= 10
        {
            return Ok(());
        }
        let mut keys = vec![key];
        if let Some(ip) = ip {
            keys.push(format!("ip:{ip}"));
        }
        self.record(RESET, &keys).await?;
        let Some(user) = self.user_by_email(&email).await?.filter(|u| u.active) else {
            return Ok(());
        };
        let link = self.reset_link(user.id).await?;
        let msg = crate::notify::Message {
            to: user.email.clone(),
            to_name: user.name.clone(),
            template: "password_reset".into(),
            data: serde_json::json!({ "link": link, "valid_minutes": RESET_MINUTES }),
        };
        if let Err(e) = self.notifier().send(msg).await {
            eprintln!("auth: could not send the password reset message: {e:#}");
        }
        Ok(())
    }

    /// The account a reset token belongs to, if it is unused and unexpired.
    pub async fn reset_user(&self, token: &str) -> anyhow::Result<Option<User>> {
        if !secret::is_token(token) {
            return Ok(None);
        }
        let id = sqlx::query_scalar!(
            r#"SELECT r.user_id FROM engine_password_resets r JOIN engine_users u ON u.id = r.user_id
               WHERE r.token_hash = $1 AND r.used_at IS NULL AND r.expires_at > now() AND u.active"#,
            secret::hash_token(token)
        )
        .fetch_optional(self.pool())
        .await?;
        match id {
            Some(id) => self.user(id).await,
            None => Ok(None),
        }
    }

    /// Set a new password with a reset token. The token is used up, other
    /// tokens of the account are voided, and every session ends.
    pub async fn reset_password(&self, token: &str, new_password: &str) -> Result<User, AuthError> {
        let user = self
            .reset_user(token)
            .await?
            .ok_or_else(|| AuthError::invalid("token", "is invalid, used or expired"))?;
        if let Some(m) = password::check_policy(new_password, &user.email) {
            return Err(AuthError::invalid("password", m));
        }
        // Claim the token first, so it works once even under races.
        let claimed = sqlx::query!(
            r#"UPDATE engine_password_resets SET used_at = now()
               WHERE token_hash = $1 AND used_at IS NULL AND expires_at > now()"#,
            secret::hash_token(token)
        )
        .execute(self.pool())
        .await?;
        if claimed.rows_affected() != 1 {
            return Err(AuthError::invalid("token", "is invalid, used or expired"));
        }
        self.set_password(user.id, new_password, None).await?;
        sqlx::query!(
            "DELETE FROM engine_auth_events WHERE kind = $1 AND key = $2",
            FAILURE,
            format!("email:{}", user.email)
        )
        .execute(self.pool())
        .await?;
        Ok(user)
    }
}
