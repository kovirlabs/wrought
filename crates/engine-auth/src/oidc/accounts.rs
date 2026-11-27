//! What a verified provider identity may do to accounts: sign in, make an
//! account, be linked or unlinked. The rules that keep accounts from being
//! taken over live here, apart from HTTP, so they are tested directly.

use engine_db::Timestamp;

use super::jwt::IdClaims;
use super::{Identity, ProviderConfig, sql};
use crate::accounts::{FAILURE, REGISTER, WINDOW_MINUTES};
use crate::{Auth, AuthError, User, normalize_email, password, secret, valid_email};

/// How long a sign-in at the provider may take.
pub(crate) const STATE_MINUTES: i64 = 10;

/// Why a sign-in was started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// Sign in (or make an account).
    Login,
    /// Add the provider to the signed-in account `user_id`.
    Link,
}

impl Purpose {
    fn as_str(self) -> &'static str {
        match self {
            Purpose::Login => "login",
            Purpose::Link => "link",
        }
    }
}

/// A sign-in in progress, as stored.
#[derive(Debug, Clone)]
pub struct Pending {
    pub provider: String,
    pub state: String,
    pub nonce: String,
    pub verifier: String,
    pub purpose: Purpose,
    pub user_id: Option<i64>,
    pub next: String,
}

/// The values a new sign-in sends to the provider, and the cookie token
/// that finds them again.
#[derive(Debug, Clone)]
pub struct Started {
    pub cookie: String,
    pub state: String,
    pub nonce: String,
    pub verifier: String,
}

/// Outcome of signing in with a verified identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignInOutcome {
    /// The identity is linked to this account.
    Ok(User),
    /// A new account was made for the identity.
    Created(User),
    /// The linked account is blocked.
    Blocked,
    /// Too many failures from this address or for this account.
    Limited,
    /// Nobody linked this identity and the provider does not make accounts.
    NotLinked,
    /// Nobody linked this identity, and an account already uses its email
    /// address. It is not signed in: its owner must link the provider.
    EmailInUse,
    /// A new account needs a verified email address from the provider.
    NoVerifiedEmail,
    /// The account's own address is not confirmed yet (`verify_email`).
    Unverified(User),
}

/// Outcome of linking an identity to a signed-in account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkOutcome {
    Linked,
    /// Already linked to this account.
    AlreadyLinked,
    /// Linked to another account.
    OtherAccount,
    /// This account already has another identity from this issuer.
    IssuerInUse,
}

/// Outcome of unlinking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnlinkOutcome {
    Unlinked,
    NotLinked,
    /// The account would have no password and no provider left.
    LastWay,
}

#[derive(sqlx::FromRow)]
struct IdentityRow {
    provider: String,
    issuer: String,
    subject: String,
    email: String,
    created_at: Timestamp,
    last_used_at: Option<Timestamp>,
}

impl Auth {
    // ----- sign-ins in progress ---------------------------------------------------

    /// Store a new sign-in attempt. Returns the cookie token (only its
    /// SHA-256 is stored) and the state, nonce and PKCE verifier.
    pub async fn oidc_begin(
        &self,
        provider: &str,
        purpose: Purpose,
        user_id: Option<i64>,
        next: &str,
    ) -> anyhow::Result<Started> {
        let s = Started {
            cookie: secret::token(),
            state: secret::token(),
            nonce: secret::token(),
            verifier: secret::token(),
        };
        let now = Timestamp::now();
        sql::PRUNE_STATES.bind(now).execute(self.db()).await?;
        sql::INSERT_STATE
            .bind(secret::hash_token(&s.cookie))
            .bind(provider)
            .bind(&s.state)
            .bind(&s.nonce)
            .bind(&s.verifier)
            .bind(purpose.as_str())
            .bind(user_id)
            .bind(next)
            .bind(now)
            .bind(now.plus_minutes(STATE_MINUTES))
            .execute(self.db())
            .await?;
        Ok(s)
    }

    /// Take the attempt a cookie token names: it is deleted as it is read,
    /// so it works once, and only within [`STATE_MINUTES`].
    pub async fn oidc_claim(&self, cookie: &str) -> anyhow::Result<Option<Pending>> {
        if !secret::is_token(cookie) {
            return Ok(None);
        }
        type Row = (String, String, String, String, String, Option<i64>, String);
        let row: Option<Row> = sql::CLAIM_STATE
            .bind(secret::hash_token(cookie))
            .bind(Timestamp::now())
            .fetch_optional(self.db())
            .await?;
        Ok(row.map(
            |(provider, state, nonce, verifier, purpose, user_id, next)| Pending {
                provider,
                state,
                nonce,
                verifier,
                purpose: if purpose == "link" {
                    Purpose::Link
                } else {
                    Purpose::Login
                },
                user_id,
                next,
            },
        ))
    }

    // ----- rate limits ----------------------------------------------------------------

    /// Whether sign-ins from `ip` are refused for now (failed password and
    /// provider sign-ins count alike).
    pub(crate) async fn ip_limited(&self, ip: Option<&str>) -> anyhow::Result<bool> {
        let Some(ip) = ip else { return Ok(false) };
        Ok(self
            .recent(FAILURE, &format!("ip:{ip}"), WINDOW_MINUTES)
            .await?
            >= self.config().max_failures_per_address)
    }

    /// Count a failed provider sign-in against the client's address.
    pub(crate) async fn oidc_failure(&self, ip: Option<&str>) -> anyhow::Result<()> {
        match ip {
            Some(ip) => self.record(FAILURE, &[format!("ip:{ip}")]).await,
            None => Ok(()),
        }
    }

    // ----- identities -------------------------------------------------------------------

    pub(crate) async fn oidc_identities(&self, user_id: i64) -> anyhow::Result<Vec<Identity>> {
        let rows: Vec<IdentityRow> = sql::IDENTITIES_OF
            .bind(user_id)
            .fetch_all(self.db())
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| Identity {
                provider: r.provider,
                issuer: r.issuer,
                subject: r.subject,
                email: r.email,
                created_at: r.created_at.iso(),
                last_used_at: r.last_used_at.map(|t| t.iso()),
            })
            .collect())
    }

    async fn identity_user(&self, claims: &IdClaims) -> anyhow::Result<Option<i64>> {
        Ok(sql::IDENTITY_USER
            .bind(&claims.issuer)
            .bind(&claims.subject)
            .scalar_opt(self.db())
            .await?)
    }

    /// Sign in with a verified identity. Never finds an account by email
    /// address: an identity nobody linked either gets a new account (when
    /// the provider allows it and its address is verified and unused) or
    /// nothing.
    pub async fn oidc_sign_in(
        &self,
        provider: &ProviderConfig,
        claims: &IdClaims,
        ip: Option<&str>,
    ) -> anyhow::Result<SignInOutcome> {
        if self.ip_limited(ip).await? {
            return Ok(SignInOutcome::Limited);
        }
        if let Some(id) = self.identity_user(claims).await? {
            let user = self
                .user(id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("account {id} vanished"))?;
            if !user.active {
                self.oidc_failure(ip).await?;
                return Ok(SignInOutcome::Blocked);
            }
            if self.login_limited(&user.email, ip).await? {
                return Ok(SignInOutcome::Limited);
            }
            sql::TOUCH_IDENTITY
                .bind(&claims.issuer)
                .bind(&claims.subject)
                .bind(Timestamp::now())
                .bind(claims.email.clone().unwrap_or_default())
                .execute(self.db())
                .await?;
            if !user.email_verified && self.config().verify_email {
                return Ok(SignInOutcome::Unverified(user));
            }
            return Ok(SignInOutcome::Ok(user));
        }
        if !provider.creates_accounts() {
            self.oidc_failure(ip).await?;
            return Ok(SignInOutcome::NotLinked);
        }
        let email = match &claims.email {
            Some(e) if claims.email_verified && valid_email(&normalize_email(e)) => {
                normalize_email(e)
            }
            _ => {
                self.oidc_failure(ip).await?;
                return Ok(SignInOutcome::NoVerifiedEmail);
            }
        };
        // The takeover rule: an existing account is never entered through
        // an identity it has not linked, whatever address the provider
        // vouches for.
        if self.user_by_email(&email).await?.is_some() {
            self.oidc_failure(ip).await?;
            return Ok(SignInOutcome::EmailInUse);
        }
        if let Some(ip) = ip {
            let key = format!("ip:{ip}");
            if self.recent(REGISTER, &key, 60).await? >= self.config().max_registrations_per_address
            {
                return Ok(SignInOutcome::Limited);
            }
            self.record(REGISTER, &[key]).await?;
        }
        let roles = self.config().default_roles.clone();
        let name = claims.name.clone().unwrap_or_default();
        // No password: the account signs in through the provider (or sets
        // one with a reset link sent to its verified address).
        let user = match self.create_account(&email, &name, None, &roles, true).await {
            Ok(u) => u,
            // Made by someone else in the meantime: same rule as above.
            Err(AuthError::Invalid(e)) if e.iter().any(|(f, _)| f == "email") => {
                return Ok(SignInOutcome::EmailInUse);
            }
            Err(e) => return Err(e.into_anyhow()),
        };
        let inserted: Option<i64> = sql::INSERT_IDENTITY
            .bind(user.id)
            .bind(&provider.name)
            .bind(&claims.issuer)
            .bind(&claims.subject)
            .bind(&email)
            .bind(Timestamp::now())
            .scalar_opt(self.db())
            .await?;
        if inserted.is_none() {
            // The same identity made an account in a parallel request;
            // drop ours and use that one.
            sql::DELETE_USER.bind(user.id).execute(self.db()).await?;
            return Box::pin(self.oidc_sign_in(provider, claims, ip)).await;
        }
        let msg = crate::notify::Message {
            to: user.email.clone(),
            to_name: user.name.clone(),
            template: "welcome".into(),
            data: serde_json::json!({ "provider": provider.label }),
        };
        if let Err(e) = self.notifier().send(msg).await {
            eprintln!("auth: could not send the welcome message: {e:#}");
        }
        Ok(SignInOutcome::Created(user))
    }

    /// Link a verified identity to the signed-in account `user_id`.
    pub async fn oidc_link(
        &self,
        user_id: i64,
        provider: &ProviderConfig,
        claims: &IdClaims,
    ) -> anyhow::Result<LinkOutcome> {
        match self.identity_user(claims).await? {
            Some(id) if id == user_id => return Ok(LinkOutcome::AlreadyLinked),
            Some(_) => return Ok(LinkOutcome::OtherAccount),
            None => {}
        }
        let has: i64 = sql::ISSUER_LINKED
            .bind(user_id)
            .bind(&claims.issuer)
            .scalar(self.db())
            .await?;
        if has > 0 {
            return Ok(LinkOutcome::IssuerInUse);
        }
        let inserted: Option<i64> = sql::INSERT_IDENTITY
            .bind(user_id)
            .bind(&provider.name)
            .bind(&claims.issuer)
            .bind(&claims.subject)
            .bind(claims.email.clone().unwrap_or_default())
            .bind(Timestamp::now())
            .scalar_opt(self.db())
            .await?;
        Ok(match inserted {
            Some(_) => LinkOutcome::Linked,
            None => match self.identity_user(claims).await? {
                Some(id) if id == user_id => LinkOutcome::AlreadyLinked,
                Some(_) => LinkOutcome::OtherAccount,
                None => LinkOutcome::IssuerInUse,
            },
        })
    }

    /// Unlink the account's identity from `issuer`, unless that leaves the
    /// account without a password and without a configured provider.
    pub async fn oidc_unlink(&self, user_id: i64, issuer: &str) -> anyhow::Result<UnlinkOutcome> {
        let configured: Vec<&str> = self
            .oidc_providers()
            .iter()
            .map(|p| p.issuer.as_str())
            .collect();
        let mut tx = self.db().begin().await?;
        sql::LOCK_USER.bind(user_id).execute(&mut tx).await?;
        let rows: Vec<IdentityRow> = sql::IDENTITIES_OF.bind(user_id).fetch_all(&mut tx).await?;
        if !rows.iter().any(|r| r.issuer == issuer) {
            return Ok(UnlinkOutcome::NotLinked);
        }
        let hash: String = crate::sql::PASSWORD_HASH
            .bind(user_id)
            .scalar_opt(&mut tx)
            .await?
            .unwrap_or_default();
        let others = rows
            .iter()
            .filter(|r| r.issuer != issuer && configured.contains(&r.issuer.as_str()))
            .count();
        if !password::is_usable(&hash) && others == 0 {
            return Ok(UnlinkOutcome::LastWay);
        }
        sql::DELETE_IDENTITY
            .bind(user_id)
            .bind(issuer)
            .execute(&mut tx)
            .await?;
        tx.commit().await?;
        Ok(UnlinkOutcome::Unlinked)
    }
}
