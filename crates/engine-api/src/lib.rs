//! Wrought's `api` feature: a JSON API for content types under `/api/v1/`.
//!
//! * **Tokens:** `Authorization: Bearer wr_...`. A token belongs to an
//!   account and acts with that account's roles and per-field rights,
//!   narrowed by the token's scopes (`read`, `write`, or per type:
//!   `read:article`, `write:article`). Only the token's SHA-256 is stored;
//!   the token is shown once, when it is made. Tokens can expire and be
//!   revoked; blocking the account stops its tokens too. Managed in the
//!   admin (`/admin/api-tokens/`) or with `engine api token`.
//! * **Reads:** `GET /api/v1/content/<type>/` (paginated: `page`,
//!   `per_page` up to 100, `status`, `order`, `term`, `q`) and
//!   `GET /api/v1/content/<type>/<id or slug>`. Fields the account may not
//!   read are left out.
//! * **Writes:** `POST` (create), `PATCH` (change the given fields) and
//!   `DELETE`. Changes and deletes must name the version they were based
//!   on (`If-Match: "3"` or `"version": 3`): without it the answer is 428,
//!   with an old one 409 and the current version (optimistic locking, the
//!   same as the admin). Writing a field the account may not write is a
//!   403 that names it; nothing is saved.
//! * **Errors** are always `{"error": {"status", "code", "message", ...}}`.
//! * **Rate limit** per token (`[api] requests_per_minute`, 120), and per
//!   client address for requests with a bad token; 429 with `Retry-After`.
//!   Counted in memory per process.
//!
//! Cookies are not accepted (no CSRF exposure) and no CORS headers are sent.

pub mod limit;
pub mod sql;
pub mod tokens;
pub mod web;

use anyhow::Result;
use engine_db::Migration;
use serde::Deserialize;

pub use tokens::{NewToken, Scope, Token, TokenStore};

/// The `api` feature's schema.
pub fn migrations() -> Vec<Migration> {
    vec![Migration::new(
        "api/0001_tokens",
        include_str!("../migrations/postgres/0001_tokens.sql"),
    )]
}

fn d_rpm() -> u32 {
    120
}
fn d_bad() -> u32 {
    30
}

/// `[api]` settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiConfig {
    /// Requests per token per minute.
    #[serde(default = "d_rpm")]
    pub requests_per_minute: u32,
    /// Requests with a missing or bad token per client address per minute.
    #[serde(default = "d_bad")]
    pub bad_tokens_per_minute: u32,
    /// Trust `X-Forwarded-For` for the client address (behind a proxy).
    #[serde(default)]
    pub behind_proxy: bool,
}

impl Default for ApiConfig {
    fn default() -> Self {
        toml::from_str("").expect("defaults")
    }
}

impl ApiConfig {
    pub fn from_site(section: Option<&toml::Table>) -> Result<Self> {
        let cfg: ApiConfig = match section {
            Some(t) => t
                .clone()
                .try_into()
                .map_err(|e| anyhow::anyhow!("[api]: {e}"))?,
            None => ApiConfig::default(),
        };
        if cfg.requests_per_minute == 0 || cfg.bad_tokens_per_minute == 0 {
            anyhow::bail!("[api] limits must be at least 1");
        }
        Ok(cfg)
    }
}
