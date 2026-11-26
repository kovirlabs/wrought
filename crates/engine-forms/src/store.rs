//! Forms made in the admin, submissions, spam events and engine keys.

use anyhow::Result;
use engine_db::Db;
use serde::Serialize;
use serde_json::Value as Json;

/// A stored submission.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Submission {
    pub id: i64,
    pub form: String,
    pub data: Json,
    pub user_id: Option<i64>,
    pub ip: Option<String>,
    pub created_at: String,
}

/// A form made in the admin.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct StoredForm {
    pub name: String,
    pub definition: Json,
    pub version: i32,
    pub updated_at: String,
}

/// Saving an admin form failed.
#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("a form named `{0}` already exists")]
    Exists(String),
    #[error("this form was changed by someone else (version {expected} edited, now {current})")]
    Conflict { expected: i32, current: i32 },
    #[error("no such form")]
    NotFound,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<sqlx::Error> for SaveError {
    fn from(e: sqlx::Error) -> Self {
        SaveError::Other(e.into())
    }
}

#[derive(Clone, Debug)]
pub struct FormStore {
    db: Db,
}

impl FormStore {
    pub fn new(db: Db) -> Self {
        FormStore { db }
    }

    fn pool(&self) -> &sqlx::PgPool {
        match &self.db {
            Db::Postgres(p) => p,
        }
    }

    pub async fn admin_forms(&self) -> Result<Vec<StoredForm>> {
        Ok(sqlx::query_as!(
            StoredForm,
            r#"SELECT name, definition, version,
                 to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS "updated_at!"
               FROM engine_forms ORDER BY name"#
        )
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn admin_form(&self, name: &str) -> Result<Option<StoredForm>> {
        Ok(sqlx::query_as!(
            StoredForm,
            r#"SELECT name, definition, version,
                 to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS "updated_at!"
               FROM engine_forms WHERE name = $1"#,
            name
        )
        .fetch_optional(self.pool())
        .await?)
    }

    pub async fn create_form(&self, name: &str, def: &Json) -> Result<(), SaveError> {
        let r = sqlx::query!(
            "INSERT INTO engine_forms (name, definition) VALUES ($1, $2) ON CONFLICT (name) DO NOTHING",
            name,
            def
        )
        .execute(self.pool())
        .await?;
        if r.rows_affected() == 0 {
            return Err(SaveError::Exists(name.to_string()));
        }
        Ok(())
    }

    /// Optimistic locking: only if the stored version is `expected`.
    /// Returns the new version.
    pub async fn update_form(
        &self,
        name: &str,
        expected: i32,
        def: &Json,
    ) -> Result<i32, SaveError> {
        let v = sqlx::query_scalar!(
            r#"UPDATE engine_forms SET definition = $3, version = version + 1, updated_at = now()
               WHERE name = $1 AND version = $2 RETURNING version"#,
            name,
            expected,
            def
        )
        .fetch_optional(self.pool())
        .await?;
        match v {
            Some(v) => Ok(v),
            None => match self.admin_form(name).await? {
                Some(f) => Err(SaveError::Conflict {
                    expected,
                    current: f.version,
                }),
                None => Err(SaveError::NotFound),
            },
        }
    }

    pub async fn delete_form(&self, name: &str, expected: i32) -> Result<(), SaveError> {
        let r = sqlx::query!(
            "DELETE FROM engine_forms WHERE name = $1 AND version = $2",
            name,
            expected
        )
        .execute(self.pool())
        .await?;
        if r.rows_affected() == 0 {
            return match self.admin_form(name).await? {
                Some(f) => Err(SaveError::Conflict {
                    expected,
                    current: f.version,
                }),
                None => Err(SaveError::NotFound),
            };
        }
        Ok(())
    }

    /// Store a submission. `None` if `unique_key` was already stored for
    /// this form (e.g. an address already on the newsletter).
    pub async fn insert(
        &self,
        form: &str,
        data: &Json,
        unique_key: Option<&str>,
        user_id: Option<i64>,
        ip: Option<&str>,
        user_agent: &str,
    ) -> Result<Option<i64>> {
        let ua: String = user_agent.chars().take(300).collect();
        Ok(sqlx::query_scalar!(
            r#"INSERT INTO engine_form_submissions (form, data, unique_key, user_id, ip, user_agent)
               VALUES ($1, $2, $3, $4, $5, $6)
               ON CONFLICT (form, unique_key) WHERE unique_key IS NOT NULL DO NOTHING
               RETURNING id"#,
            form,
            data,
            unique_key,
            user_id,
            ip,
            ua
        )
        .fetch_optional(self.pool())
        .await?)
    }

    /// Newest first.
    pub async fn submissions(
        &self,
        form: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Submission>> {
        Ok(sqlx::query_as!(
            Submission,
            r#"SELECT id, form, data, user_id, ip,
                 to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS "created_at!"
               FROM engine_form_submissions WHERE form = $1
               ORDER BY id DESC LIMIT $2 OFFSET $3"#,
            form,
            limit,
            offset
        )
        .fetch_all(self.pool())
        .await?)
    }

    /// A signed-in visitor's own submissions, newest first.
    pub async fn submissions_of_user(
        &self,
        user_id: i64,
        form: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Submission>> {
        Ok(sqlx::query_as!(
            Submission,
            r#"SELECT id, form, data, user_id, ip,
                 to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS "created_at!"
               FROM engine_form_submissions
               WHERE user_id = $1 AND ($2::text IS NULL OR form = $2)
               ORDER BY id DESC LIMIT $3"#,
            user_id,
            form,
            limit
        )
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn submission(&self, id: i64) -> Result<Option<Submission>> {
        Ok(sqlx::query_as!(
            Submission,
            r#"SELECT id, form, data, user_id, ip,
                 to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') AS "created_at!"
               FROM engine_form_submissions WHERE id = $1"#,
            id
        )
        .fetch_optional(self.pool())
        .await?)
    }

    pub async fn delete_submission(&self, id: i64) -> Result<bool> {
        let r = sqlx::query!("DELETE FROM engine_form_submissions WHERE id = $1", id)
            .execute(self.pool())
            .await?;
        Ok(r.rows_affected() == 1)
    }

    /// Submissions per form.
    pub async fn counts(&self) -> Result<Vec<(String, i64)>> {
        let rows = sqlx::query!(
            r#"SELECT form, count(*) AS "n!" FROM engine_form_submissions GROUP BY form"#
        )
        .fetch_all(self.pool())
        .await?;
        Ok(rows.into_iter().map(|r| (r.form, r.n)).collect())
    }

    pub async fn record_event(
        &self,
        form: &str,
        kind: &str,
        key: &str,
        reason: Option<&str>,
    ) -> Result<()> {
        sqlx::query!(
            "INSERT INTO engine_form_events (form, kind, key, reason) VALUES ($1, $2, $3, $4)",
            form,
            kind,
            key,
            reason
        )
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Accepted submissions of `form` from `key` in the last `minutes`.
    pub async fn recent_accepted(&self, form: &str, key: &str, minutes: i32) -> Result<i64> {
        Ok(sqlx::query_scalar!(
            r#"SELECT count(*) AS "n!" FROM engine_form_events
               WHERE form = $1 AND key = $2 AND kind = 'accepted'
                 AND at > now() - make_interval(mins => $3)"#,
            form,
            key,
            minutes
        )
        .fetch_one(self.pool())
        .await?)
    }

    /// Spam refused per form in the last day.
    pub async fn spam_counts(&self) -> Result<Vec<(String, i64)>> {
        let rows = sqlx::query!(
            r#"SELECT form, count(*) AS "n!" FROM engine_form_events
               WHERE kind = 'spam' AND at > now() - interval '1 day' GROUP BY form"#
        )
        .fetch_all(self.pool())
        .await?;
        Ok(rows.into_iter().map(|r| (r.form, r.n)).collect())
    }

    /// Delete events older than two days.
    pub async fn prune_events(&self) -> Result<u64> {
        let r = sqlx::query!("DELETE FROM engine_form_events WHERE at < now() - interval '2 days'")
            .execute(self.pool())
            .await?;
        Ok(r.rows_affected())
    }

    /// A random key the engine keeps in the database, made on first use.
    pub async fn engine_key(&self, name: &str) -> Result<Vec<u8>> {
        let fresh = engine_core::web::secret::random_bytes(32);
        sqlx::query!(
            "INSERT INTO engine_keys (name, value) VALUES ($1, $2) ON CONFLICT (name) DO NOTHING",
            name,
            fresh
        )
        .execute(self.pool())
        .await?;
        Ok(
            sqlx::query_scalar!("SELECT value FROM engine_keys WHERE name = $1", name)
                .fetch_one(self.pool())
                .await?,
        )
    }
}
