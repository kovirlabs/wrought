//! Roles stored in the database (`roles` feature).
//!
//! Two roles exist from the start: `admin` (everything; cannot be edited or
//! deleted) and `editor` (every content action on every type, taxonomy).
//! More are made in the admin. A user's rights are the union of their
//! roles' [`Grants`].

use anyhow::{Context, Result};
use serde::Serialize;
use sqlx::{PgConnection, PgPool};

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

pub(crate) async fn user_roles(pool: &PgPool, user_id: i64) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar!(
        "SELECT role FROM engine_user_roles WHERE user_id = $1 ORDER BY role",
        user_id
    )
    .fetch_all(pool)
    .await?)
}

pub(crate) async fn set_user_roles(
    conn: &mut PgConnection,
    user_id: i64,
    roles: &[String],
) -> Result<(), AuthError> {
    let mut names: Vec<String> = roles.to_vec();
    names.sort();
    names.dedup();
    let known: Vec<String> =
        sqlx::query_scalar!("SELECT name FROM engine_roles WHERE name = ANY($1)", &names)
            .fetch_all(&mut *conn)
            .await?;
    if let Some(bad) = names.iter().find(|n| !known.contains(n)) {
        return Err(AuthError::invalid(
            "roles",
            format!("there is no role `{bad}`"),
        ));
    }
    sqlx::query!("DELETE FROM engine_user_roles WHERE user_id = $1", user_id)
        .execute(&mut *conn)
        .await?;
    sqlx::query!(
        "INSERT INTO engine_user_roles (user_id, role) SELECT $1, unnest($2::text[])",
        user_id,
        &names
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

pub(crate) async fn active_admin_count<'e>(exec: impl sqlx::PgExecutor<'e>) -> Result<i64> {
    Ok(sqlx::query_scalar!(
        r#"SELECT count(DISTINCT u.id) AS "n!" FROM engine_users u
           JOIN engine_user_roles ur ON ur.user_id = u.id
           JOIN engine_roles r ON r.name = ur.role
           WHERE u.active AND r.is_admin"#
    )
    .fetch_one(exec)
    .await?)
}

pub(crate) async fn access_for(pool: &PgPool, user_id: i64) -> Result<Access> {
    let rows = sqlx::query!(
        r#"SELECT r.name, r.is_admin, r.grants FROM engine_roles r
           JOIN engine_user_roles ur ON ur.role = r.name
           WHERE ur.user_id = $1 ORDER BY r.name"#,
        user_id
    )
    .fetch_all(pool)
    .await?;
    let parsed: Vec<(String, bool, Grants)> = rows
        .into_iter()
        .map(|r| {
            let g: Grants = serde_json::from_value(r.grants)
                .with_context(|| format!("role `{}` has invalid grants", r.name))?;
            Ok((r.name, r.is_admin, g))
        })
        .collect::<Result<_>>()?;
    Ok(Access::from_roles(
        parsed.iter().map(|(n, a, g)| (n.as_str(), *a, g)),
    ))
}

impl Auth {
    fn roles_pool(&self) -> &PgPool {
        match self.db() {
            engine_db::Db::Postgres(p) => p,
        }
    }

    /// Every role, admin roles first, then by name.
    pub async fn roles(&self) -> Result<Vec<Role>> {
        let rows = sqlx::query!(
            r#"SELECT r.name, r.label, r.is_admin, r.grants, r.version,
                      (SELECT count(*) FROM engine_user_roles ur WHERE ur.role = r.name) AS "users!"
               FROM engine_roles r ORDER BY r.is_admin DESC, r.name"#
        )
        .fetch_all(self.roles_pool())
        .await?;
        rows.into_iter()
            .map(|r| {
                Ok(Role {
                    grants: serde_json::from_value(r.grants)
                        .with_context(|| format!("role `{}` has invalid grants", r.name))?,
                    name: r.name,
                    label: r.label,
                    is_admin: r.is_admin,
                    version: r.version,
                    users: r.users,
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
        let done = sqlx::query!(
            r#"INSERT INTO engine_roles (name, label, grants) VALUES ($1, $2, $3)
               ON CONFLICT (name) DO NOTHING"#,
            name,
            label,
            g
        )
        .execute(self.roles_pool())
        .await?;
        if done.rows_affected() == 0 {
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
        let updated = sqlx::query_scalar!(
            r#"UPDATE engine_roles SET label = $3, grants = $4, version = version + 1, updated_at = now()
               WHERE name = $1 AND version = $2 AND NOT is_admin RETURNING version"#,
            name,
            expected_version,
            label,
            g
        )
        .fetch_optional(self.roles_pool())
        .await?;
        if updated.is_none() {
            return Err(self.role_problem(name, expected_version).await?);
        }
        self.role(name).await?.ok_or(AuthError::NotFound)
    }

    pub async fn delete_role(&self, name: &str, expected_version: i32) -> Result<(), AuthError> {
        let done = sqlx::query!(
            "DELETE FROM engine_roles WHERE name = $1 AND version = $2 AND NOT is_admin",
            name,
            expected_version
        )
        .execute(self.roles_pool())
        .await?;
        if done.rows_affected() == 1 {
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
