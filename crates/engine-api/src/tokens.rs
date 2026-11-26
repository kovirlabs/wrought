//! API tokens.

use base64::Engine as _;
use engine_auth::{Access, Action};
use engine_db::{Db, Timestamp};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::sql;

/// What a token may do, besides what its account may do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Scope {
    /// `read` or `write` (write includes read).
    pub write: bool,
    /// `None`: every type.
    pub type_name: Option<String>,
}

impl TryFrom<String> for Scope {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        Scope::parse(&s).ok_or_else(|| format!("bad scope `{s}`"))
    }
}

impl From<Scope> for String {
    fn from(s: Scope) -> String {
        s.to_string()
    }
}

impl std::fmt::Display for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let w = if self.write { "write" } else { "read" };
        match &self.type_name {
            Some(t) => write!(f, "{w}:{t}"),
            None => f.write_str(w),
        }
    }
}

impl Scope {
    /// `read`, `write`, `read:<type>`, `write:<type>`.
    pub fn parse(s: &str) -> Option<Scope> {
        let (w, t) = match s.split_once(':') {
            Some((w, t)) => (w, Some(t)),
            None => (s, None),
        };
        let write = match w {
            "read" => false,
            "write" => true,
            _ => return None,
        };
        if let Some(t) = t
            && !engine_content::types::valid_name(t)
        {
            return None;
        }
        Some(Scope {
            write,
            type_name: t.map(str::to_string),
        })
    }

    fn covers(&self, type_name: &str, write: bool) -> bool {
        (self.write || !write) && self.type_name.as_deref().is_none_or(|t| t == type_name)
    }
}

/// A stored token (never its secret).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Token {
    pub id: i64,
    pub user_id: i64,
    pub name: String,
    /// The first characters, to tell tokens apart (`wr_AbCdEf…`).
    pub prefix: String,
    pub scopes: Vec<Scope>,
    pub created_at: String,
    pub expires_at: Option<String>,
    pub last_used_at: Option<String>,
    pub revoked_at: Option<String>,
}

impl Token {
    /// Whether the token's scopes allow `action` on `type_name` (the
    /// account's rights are checked separately).
    pub fn allows(&self, type_name: &str, action: Action) -> bool {
        let write = !matches!(action, Action::View);
        self.scopes.iter().any(|s| s.covers(type_name, write))
    }

    pub fn active(&self) -> bool {
        self.revoked_at.is_none()
    }
}

/// What [`TokenStore::create`] returns: the stored token and its secret
/// (show it once; it cannot be read again).
#[derive(Debug)]
pub struct NewToken {
    pub token: Token,
    pub secret: String,
}

#[derive(sqlx::FromRow)]
struct Row {
    id: i64,
    user_id: i64,
    name: String,
    prefix: String,
    scopes: sqlx::types::Json<Vec<Scope>>,
    created_at: Timestamp,
    expires_at: Option<Timestamp>,
    last_used_at: Option<Timestamp>,
    revoked_at: Option<Timestamp>,
}

impl From<Row> for Token {
    fn from(r: Row) -> Token {
        Token {
            id: r.id,
            user_id: r.user_id,
            name: r.name,
            prefix: r.prefix,
            scopes: r.scopes.0,
            created_at: r.created_at.iso(),
            expires_at: r.expires_at.map(|t| t.iso()),
            last_used_at: r.last_used_at.map(|t| t.iso()),
            revoked_at: r.revoked_at.map(|t| t.iso()),
        }
    }
}

const PREFIX: &str = "wr_";

fn hash(secret: &str) -> Vec<u8> {
    Sha256::digest(secret.as_bytes()).to_vec()
}

/// Whether `s` has the shape of a token (cheap check before hashing).
pub fn looks_like_token(s: &str) -> bool {
    s.len() == PREFIX.len() + 43
        && s.starts_with(PREFIX)
        && s[PREFIX.len()..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[derive(Clone, Debug)]
pub struct TokenStore {
    db: Db,
}

impl TokenStore {
    pub fn new(db: Db) -> Self {
        TokenStore { db }
    }

    /// Make a token for `user_id`. `days`: lifetime (`None`: until
    /// revoked).
    pub async fn create(
        &self,
        user_id: i64,
        name: &str,
        scopes: &[Scope],
        days: Option<u32>,
    ) -> Result<NewToken, String> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 100 || name.chars().any(char::is_control) {
            return Err("the name must be 1 to 100 characters".into());
        }
        if scopes.is_empty() {
            return Err("choose at least one scope".into());
        }
        if days.is_some_and(|d| d == 0 || d > 3650) {
            return Err("the lifetime must be 1 to 3650 days".into());
        }
        let raw = engine_core::web::secret::random_bytes(32);
        let secret = format!(
            "{PREFIX}{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
        );
        let prefix: String = secret.chars().take(10).collect();
        let now = Timestamp::now();
        let id: i64 = sql::INSERT
            .bind(user_id)
            .bind(name)
            .bind(hash(&secret))
            .bind(prefix.as_str())
            .bind(serde_json::to_value(scopes).map_err(|e| e.to_string())?)
            .bind(now)
            .bind(days.map(|d| now.plus_seconds(i64::from(d) * 86_400)))
            .scalar(&self.db)
            .await
            .map_err(|e| e.to_string())?;
        let token = self
            .get(id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("the token vanished")?;
        Ok(NewToken { token, secret })
    }

    /// The live token for a secret (not revoked, not expired, account
    /// active). Marks it used (at most once a minute).
    pub async fn verify(&self, secret: &str) -> anyhow::Result<Option<Token>> {
        if !looks_like_token(secret) {
            return Ok(None);
        }
        let now = Timestamp::now();
        let row: Option<Row> = sql::BY_HASH
            .bind(hash(secret))
            .bind(now)
            .fetch_optional(&self.db)
            .await?;
        let Some(t) = row.map(Token::from) else {
            return Ok(None);
        };
        sql::TOUCH
            .bind(t.id)
            .bind(now)
            .bind(now.minus_seconds(60))
            .execute(&self.db)
            .await?;
        Ok(Some(t))
    }

    pub async fn get(&self, id: i64) -> anyhow::Result<Option<Token>> {
        let row: Option<Row> = sql::GET.bind(id).fetch_optional(&self.db).await?;
        Ok(row.map(Into::into))
    }

    pub async fn of_user(&self, user_id: i64) -> anyhow::Result<Vec<Token>> {
        let rows: Vec<Row> = sql::OF_USER.bind(user_id).fetch_all(&self.db).await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Every token (administrators), newest first.
    pub async fn all(&self, limit: i64) -> anyhow::Result<Vec<Token>> {
        let rows: Vec<Row> = sql::EVERY.bind(limit).fetch_all(&self.db).await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Revoke a token. `false` if it was already revoked or does not exist.
    pub async fn revoke(&self, id: i64) -> anyhow::Result<bool> {
        Ok(sql::REVOKE
            .bind(id)
            .bind(Timestamp::now())
            .execute(&self.db)
            .await?
            == 1)
    }
}

/// What a request with `token` may do with `type_name`: the account's
/// rights and the token's scopes, both.
pub fn can(access: &Access, token: &Token, type_name: &str, action: Action) -> bool {
    token.allows(type_name, action) && access.can(type_name, action)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_parse_and_cover() {
        let r = Scope::parse("read").unwrap();
        let wa = Scope::parse("write:article").unwrap();
        assert!(Scope::parse("admin").is_none());
        assert!(Scope::parse("read:../x").is_none());
        assert!(r.covers("article", false) && !r.covers("article", true));
        assert!(wa.covers("article", true) && wa.covers("article", false));
        assert!(!wa.covers("page", false));
        assert_eq!(wa.to_string(), "write:article");
        let json = serde_json::to_string(&vec![r, wa]).unwrap();
        assert_eq!(json, "[\"read\",\"write:article\"]");
    }

    #[test]
    fn token_shape() {
        assert!(looks_like_token(&format!("wr_{}", "a".repeat(43))));
        assert!(!looks_like_token("wr_short"));
        assert!(!looks_like_token(&format!("xx_{}", "a".repeat(43))));
    }
}
