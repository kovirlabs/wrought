//! Forms made in the admin, submissions, spam events and engine keys.

use anyhow::Result;
use engine_db::{Db, Timestamp};
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

#[derive(sqlx::FromRow)]
struct FormRow {
    name: String,
    definition: sqlx::types::Json<Json>,
    version: i32,
    updated_at: Timestamp,
}

impl From<FormRow> for StoredForm {
    fn from(r: FormRow) -> StoredForm {
        StoredForm {
            name: r.name,
            definition: r.definition.0,
            version: r.version,
            updated_at: r.updated_at.iso(),
        }
    }
}

#[derive(sqlx::FromRow)]
struct SubmissionRow {
    id: i64,
    form: String,
    data: sqlx::types::Json<Json>,
    user_id: Option<i64>,
    ip: Option<String>,
    created_at: Timestamp,
}

impl From<SubmissionRow> for Submission {
    fn from(r: SubmissionRow) -> Submission {
        Submission {
            id: r.id,
            form: r.form,
            data: r.data.0,
            user_id: r.user_id,
            ip: r.ip,
            created_at: r.created_at.iso(),
        }
    }
}

impl FormStore {
    pub fn new(db: Db) -> Self {
        FormStore { db }
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    pub async fn admin_forms(&self) -> Result<Vec<StoredForm>> {
        let rows: Vec<FormRow> = sql::FORMS.q().fetch_all(&self.db).await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn admin_form(&self, name: &str) -> Result<Option<StoredForm>> {
        let row: Option<FormRow> = sql::FORM.bind(name).fetch_optional(&self.db).await?;
        Ok(row.map(Into::into))
    }

    pub async fn create_form(&self, name: &str, def: &Json) -> Result<(), SaveError> {
        let n = sql::CREATE_FORM
            .bind(name)
            .bind(def)
            .execute(&self.db)
            .await?;
        if n == 0 {
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
        let v: Option<i32> = sql::UPDATE_FORM
            .bind(name)
            .bind(expected)
            .bind(def)
            .bind(Timestamp::now())
            .scalar_opt(&self.db)
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
        let n = sql::DELETE_FORM
            .bind(name)
            .bind(expected)
            .execute(&self.db)
            .await?;
        if n == 0 {
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
        Ok(sql::INSERT_SUBMISSION
            .bind(form)
            .bind(data)
            .bind(unique_key)
            .bind(user_id)
            .bind(ip)
            .bind(ua)
            .scalar_opt(&self.db)
            .await?)
    }

    /// Newest first.
    pub async fn submissions(
        &self,
        form: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Submission>> {
        let rows: Vec<SubmissionRow> = sql::SUBMISSIONS
            .bind(form)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.db)
            .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// A signed-in visitor's own submissions, newest first.
    pub async fn submissions_of_user(
        &self,
        user_id: i64,
        form: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Submission>> {
        let rows: Vec<SubmissionRow> = sql::SUBMISSIONS_OF_USER
            .bind(user_id)
            .bind(form)
            .bind(limit)
            .fetch_all(&self.db)
            .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn submission(&self, id: i64) -> Result<Option<Submission>> {
        let row: Option<SubmissionRow> = sql::SUBMISSION.bind(id).fetch_optional(&self.db).await?;
        Ok(row.map(Into::into))
    }

    pub async fn delete_submission(&self, id: i64) -> Result<bool> {
        Ok(sql::DELETE_SUBMISSION.bind(id).execute(&self.db).await? == 1)
    }

    /// Submissions per form.
    pub async fn counts(&self) -> Result<Vec<(String, i64)>> {
        Ok(sql::COUNTS.q().fetch_all(&self.db).await?)
    }

    pub async fn record_event(
        &self,
        form: &str,
        kind: &str,
        key: &str,
        reason: Option<&str>,
    ) -> Result<()> {
        sql::INSERT_EVENT
            .bind(form)
            .bind(kind)
            .bind(key)
            .bind(reason)
            .bind(Timestamp::now())
            .execute(&self.db)
            .await?;
        Ok(())
    }

    /// Accepted submissions of `form` from `key` in the last `minutes`.
    pub async fn recent_accepted(&self, form: &str, key: &str, minutes: i32) -> Result<i64> {
        Ok(sql::RECENT_ACCEPTED
            .bind(form)
            .bind(key)
            .bind(Timestamp::now().minus_minutes(minutes.into()))
            .scalar(&self.db)
            .await?)
    }

    /// Spam refused per form in the last day.
    pub async fn spam_counts(&self) -> Result<Vec<(String, i64)>> {
        Ok(sql::SPAM_COUNTS
            .bind(Timestamp::now().minus_days(1))
            .fetch_all(&self.db)
            .await?)
    }

    /// Delete events older than two days.
    pub async fn prune_events(&self) -> Result<u64> {
        Ok(sql::PRUNE_EVENTS
            .bind(Timestamp::now().minus_days(2))
            .execute(&self.db)
            .await?)
    }

    /// A random key the engine keeps in the database, made on first use.
    pub async fn engine_key(&self, name: &str) -> Result<Vec<u8>> {
        let fresh = engine_core::web::secret::random_bytes(32);
        sql::INSERT_KEY
            .bind(name)
            .bind(fresh)
            .execute(&self.db)
            .await?;
        Ok(sql::KEY.bind(name).scalar(&self.db).await?)
    }
}

/// The `forms` feature's statements (see `engine_db::sql`).
pub mod sql {
    engine_db::statements! {
        FORMS = "SELECT name, definition, version, updated_at FROM engine_forms ORDER BY name";
        FORM = "SELECT name, definition, version, updated_at FROM engine_forms WHERE name = $1";
        CREATE_FORM = "INSERT INTO engine_forms (name, definition) VALUES ($1, $2)
            ON CONFLICT (name) DO NOTHING";
        UPDATE_FORM = "UPDATE engine_forms SET definition = $3, version = version + 1, updated_at = $4
            WHERE name = $1 AND version = $2 RETURNING version";
        DELETE_FORM = "DELETE FROM engine_forms WHERE name = $1 AND version = $2";
        INSERT_SUBMISSION = "INSERT INTO engine_form_submissions
                (form, data, unique_key, user_id, ip, user_agent)
            VALUES ($1, $2, $3, $4, $5, $6)
            ON CONFLICT (form, unique_key) WHERE unique_key IS NOT NULL DO NOTHING
            RETURNING id";
        SUBMISSIONS = "SELECT id, form, data, user_id, ip, created_at
            FROM engine_form_submissions WHERE form = $1
            ORDER BY id DESC LIMIT $2 OFFSET $3";
        SUBMISSIONS_OF_USER = "SELECT id, form, data, user_id, ip, created_at
            FROM engine_form_submissions
            WHERE user_id = $1 AND (CAST($2 AS TEXT) IS NULL OR form = $2)
            ORDER BY id DESC LIMIT $3";
        SUBMISSION = "SELECT id, form, data, user_id, ip, created_at
            FROM engine_form_submissions WHERE id = $1";
        DELETE_SUBMISSION = "DELETE FROM engine_form_submissions WHERE id = $1";
        COUNTS = "SELECT form, count(*) FROM engine_form_submissions GROUP BY form";
        INSERT_EVENT = "INSERT INTO engine_form_events (form, kind, key, reason, at)
            VALUES ($1, $2, $3, $4, $5)";
        RECENT_ACCEPTED = "SELECT count(*) FROM engine_form_events
            WHERE form = $1 AND key = $2 AND kind = 'accepted' AND at > $3";
        SPAM_COUNTS = "SELECT form, count(*) FROM engine_form_events
            WHERE kind = 'spam' AND at > $1 GROUP BY form";
        PRUNE_EVENTS = "DELETE FROM engine_form_events WHERE at < $1";
        INSERT_KEY = "INSERT INTO engine_keys (name, value) VALUES ($1, $2)
            ON CONFLICT (name) DO NOTHING";
        KEY = "SELECT value FROM engine_keys WHERE name = $1";
    }
}
