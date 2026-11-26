//! Roles stored in the database (`roles` feature).
//!
//! Two roles exist from the start: `admin` (everything; cannot be edited or
//! deleted) and `editor` (every content action on every type, taxonomy).
//! More are made in the admin. A user's rights are the union of their
//! roles' [`Grants`].

use anyhow::{Context, Result};
use engine_db::{Db, Exec, Timestamp, Tx};
use serde::Serialize;

use crate::sql;

use crate::access::{Access, Grants};
use crate::{Auth, AuthError};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Role {
    pub name: String,
    pub label: String,
    pub is_admin: bool,
    pub grants: Grants,
    pub version: i32,
    /// Accounts holding it.
    pub users: i64,
}

/// Whether `s` is a valid role name: `[a-z][a-z0-9_]{0,39}`.
pub fn valid_role_name(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 40
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

pub(crate) async fn user_roles(db: &Db, user_id: i64) -> Result<Vec<String>> {
    Ok(sql::USER_ROLES.bind(user_id).scalars(db).await?)
}

pub(crate) async fn set_user_roles(
    tx: &mut Tx,
    user_id: i64,
    roles: &[String],
) -> Result<(), AuthError> {
    let mut names: Vec<String> = roles.to_vec();
    names.sort();
    names.dedup();
    let known: Vec<String> = sql::KNOWN_ROLES
        .bind(names.as_slice())
        .scalars(&mut *tx)
        .await?;
    if let Some(bad) = names.iter().find(|n| !known.contains(n)) {
        return Err(AuthError::invalid(
            "roles",
            format!("there is no role `{bad}`"),
        ));
    }
    sql::CLEAR_USER_ROLES
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    sql::INSERT_USER_ROLES
        .bind(user_id)
        .bind(names)
        .execute(&mut *tx)
        .await?;
    Ok(())
}

pub(crate) async fn active_admin_count(ex: impl Exec) -> Result<i64> {
    Ok(sql::ACTIVE_ADMINS.q().scalar(ex).await?)
}

type JsonCol = sqlx::types::Json<serde_json::Value>;

pub(crate) async fn access_for(db: &Db, user_id: i64) -> Result<Access> {
    let rows: Vec<(String, bool, JsonCol)> = sql::ROLES_OF_USER.bind(user_id).fetch_all(db).await?;
    let parsed: Vec<(String, bool, Grants)> = rows
        .into_iter()
        .map(|(name, is_admin, grants)| {
            let g: Grants = serde_json::from_value(grants.0)
                .with_context(|| format!("role `{name}` has invalid grants"))?;
            Ok((name, is_admin, g))
        })
        .collect::<Result<_>>()?;
    Ok(Access::from_roles(
        parsed.iter().map(|(n, a, g)| (n.as_str(), *a, g)),
    ))
}

impl Auth {
    /// Every role, admin roles first, then by name.
    pub async fn roles(&self) -> Result<Vec<Role>> {
        let rows: Vec<(String, String, bool, JsonCol, i32, i64)> =
            sql::ROLES.q().fetch_all(self.db()).await?;
        rows.into_iter()
            .map(|(name, label, is_admin, grants, version, users)| {
                Ok(Role {
                    grants: serde_json::from_value(grants.0)
                        .with_context(|| format!("role `{name}` has invalid grants"))?,
                    name,
                    label,
                    is_admin,
                    version,
                    users,
                })
            })
            .collect()
    }

    pub async fn role(&self, name: &str) -> Result<Option<Role>> {
        Ok(self.roles().await?.into_iter().find(|r| r.name == name))
    }

    pub async fn create_role(
        &self,
        name: &str,
        label: &str,
        grants: &Grants,
    ) -> Result<Role, AuthError> {
        let label = label.trim();
        let mut errs = Vec::new();
        if !valid_role_name(name) {
            errs.push((
                "name".into(),
                "must be lowercase letters, digits and `_`, starting with a letter".into(),
            ));
        }
        if label.is_empty() || label.chars().count() > 100 {
            errs.push(("label".into(), "must be 1 to 100 characters".into()));
        }
        if !errs.is_empty() {
            return Err(AuthError::Invalid(errs));
        }
        let g = serde_json::to_value(grants).context("grants")?;
        let done = sql::INSERT_ROLE
            .bind(name)
            .bind(label)
            .bind(g)
            .execute(self.db())
            .await?;
        if done == 0 {
            return Err(AuthError::invalid("name", "is already a role"));
        }
        self.role(name).await?.ok_or(AuthError::NotFound)
    }

    /// Change a role read at `expected_version`. Administrator roles cannot
    /// be changed.
    pub async fn update_role(
        &self,
        name: &str,
        expected_version: i32,
        label: &str,
        grants: &Grants,
    ) -> Result<Role, AuthError> {
        let label = label.trim();
        if label.is_empty() || label.chars().count() > 100 {
            return Err(AuthError::invalid("label", "must be 1 to 100 characters"));
        }
        let g = serde_json::to_value(grants).context("grants")?;
        let updated: Option<i32> = sql::UPDATE_ROLE
            .bind(name)
            .bind(expected_version)
            .bind(label)
            .bind(g)
            .bind(Timestamp::now())
            .scalar_opt(self.db())
            .await?;
        if updated.is_none() {
            return Err(self.role_problem(name, expected_version).await?);
        }
        self.role(name).await?.ok_or(AuthError::NotFound)
    }

    pub async fn delete_role(&self, name: &str, expected_version: i32) -> Result<(), AuthError> {
        let done = sql::DELETE_ROLE
            .bind(name)
            .bind(expected_version)
            .execute(self.db())
            .await?;
        if done == 1 {
            Ok(())
        } else {
            Err(self.role_problem(name, expected_version).await?)
        }
    }

    async fn role_problem(&self, name: &str, expected: i32) -> Result<AuthError> {
        Ok(match self.role(name).await? {
            None => AuthError::NotFound,
            Some(r) if r.is_admin => {
                AuthError::invalid("name", "administrator roles cannot be changed")
            }
            Some(r) => AuthError::Conflict {
                expected,
                current: r.version,
            },
        })
    }
}
