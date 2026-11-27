//! Backend-neutral queries.
//!
//! Every query on the engine's own tables is a [`Sql`] statement declared
//! with [`statements!`](crate::statements): one SQL text that both
//! PostgreSQL and SQLite accept (`$1`-style parameters work on both), or a
//! pair of texts where the dialects differ, or a Postgres-only text for the
//! features the spec keeps on Postgres (`jobs`). Values are bound as
//! [`Arg`]s and rows are decoded with `#[derive(sqlx::FromRow)]` structs,
//! tuples, or the dynamic [`Row`]. The backend is chosen at run time by the
//! [`Db`] (or [`Tx`]) the query runs on.
//!
//! Each crate lists its statements in a `sql` module; the catalogue test
//! (`crates/engine-cli/tests/queries.rs`) prepares every one of them
//! against a fully migrated database of each backend, so a typo in a
//! column name or a dialect slip fails a test in seconds instead of at run
//! time. That replaces sqlx's compile-time checking (`query!`), which needs
//! one database per backend at build time; see DECISIONS.md.
//!
//! Queries whose SQL is only known at run time (the per-site
//! `content_<type>` tables) use [`query`] with a `String`.

use std::borrow::Cow;

use crate::{Backend, Db, Timestamp};

/// A statement on the engine's own tables.
#[derive(Debug)]
pub struct Sql {
    /// `crate::module::NAME`, for messages and the catalogue test.
    pub name: &'static str,
    pub postgres: &'static str,
    /// `None`: Postgres only.
    pub sqlite: Option<&'static str>,
}

impl Sql {
    #[doc(hidden)]
    pub const fn new(
        name: &'static str,
        postgres: &'static str,
        sqlite: Option<&'static str>,
    ) -> Sql {
        Sql {
            name,
            postgres,
            sqlite,
        }
    }

    /// The text for `backend`, if the statement exists there.
    pub fn text(&self, backend: Backend) -> Option<&'static str> {
        match backend {
            Backend::Postgres => Some(self.postgres),
            Backend::Sqlite => self.sqlite,
        }
    }

    /// Start a query with this statement.
    pub fn q(&'static self) -> Query<'static> {
        Query {
            text: Text::Static(self),
            args: Vec::new(),
        }
    }

    /// Start a query with this statement and its first argument.
    pub fn bind(&'static self, a: impl Into<Arg>) -> Query<'static> {
        self.q().bind(a)
    }
}

/// Declare statements: `NAME = "sql";` (both backends),
/// `NAME = { postgres: "...", sqlite: "..." };` (dialects differ) or
/// `NAME = { postgres: "..." };` (Postgres only). Also defines
/// `pub static ALL: &[&Sql]`, the module's catalogue.
#[macro_export]
macro_rules! statements {
    ($( $(#[$m:meta])* $name:ident = $body:tt ; )*) => {
        $(
            $(#[$m])*
            pub static $name: $crate::sql::Sql = $crate::statements!(@body $name $body);
        )*
        /// Every statement of this module (for the catalogue test).
        pub static ALL: &[&$crate::sql::Sql] = &[$( &$name ),*];
    };
    (@body $name:ident { postgres: $pg:literal, sqlite: $lite:literal $(,)? }) => {
        $crate::sql::Sql::new(concat!(module_path!(), "::", stringify!($name)), $pg, Some($lite))
    };
    (@body $name:ident { postgres: $pg:literal $(,)? }) => {
        $crate::sql::Sql::new(concat!(module_path!(), "::", stringify!($name)), $pg, None)
    };
    (@body $name:ident $text:literal) => {
        $crate::sql::Sql::new(concat!(module_path!(), "::", stringify!($name)), $text, Some($text))
    };
}

/// A bound value. Every variant carries an `Option` so a SQL `NULL` keeps
/// its type (Postgres types parameters when preparing).
#[derive(Debug, Clone, PartialEq)]
pub enum Arg {
    I64(Option<i64>),
    I32(Option<i32>),
    F64(Option<f64>),
    Bool(Option<bool>),
    Text(Option<String>),
    Bytes(Option<Vec<u8>>),
    /// `jsonb` on Postgres, text on SQLite.
    Json(Option<serde_json::Value>),
    /// `timestamptz` on Postgres, ISO-8601 text on SQLite.
    Time(Option<Timestamp>),
    /// `bigint[]` on Postgres (`= ANY($n)`), a JSON array on SQLite
    /// (`IN (SELECT value FROM json_each($n))`).
    I64s(Vec<i64>),
    /// `text[]` on Postgres, a JSON array on SQLite.
    Texts(Vec<String>),
}

macro_rules! arg_from {
    ($t:ty, $v:ident) => {
        impl From<$t> for Arg {
            fn from(x: $t) -> Arg {
                Arg::$v(Some(x.into()))
            }
        }
        impl From<Option<$t>> for Arg {
            fn from(x: Option<$t>) -> Arg {
                Arg::$v(x.map(Into::into))
            }
        }
    };
}
arg_from!(i64, I64);
arg_from!(i32, I32);
arg_from!(f64, F64);
arg_from!(bool, Bool);
arg_from!(String, Text);
arg_from!(Vec<u8>, Bytes);
arg_from!(serde_json::Value, Json);
arg_from!(Timestamp, Time);

impl From<u32> for Arg {
    fn from(x: u32) -> Arg {
        Arg::I64(Some(x.into()))
    }
}
impl From<&str> for Arg {
    fn from(x: &str) -> Arg {
        Arg::Text(Some(x.to_string()))
    }
}
impl From<Option<&str>> for Arg {
    fn from(x: Option<&str>) -> Arg {
        Arg::Text(x.map(str::to_string))
    }
}
impl From<&String> for Arg {
    fn from(x: &String) -> Arg {
        Arg::Text(Some(x.clone()))
    }
}
impl From<&[u8]> for Arg {
    fn from(x: &[u8]) -> Arg {
        Arg::Bytes(Some(x.to_vec()))
    }
}
impl From<&Vec<u8>> for Arg {
    fn from(x: &Vec<u8>) -> Arg {
        Arg::Bytes(Some(x.clone()))
    }
}
impl From<&serde_json::Value> for Arg {
    fn from(x: &serde_json::Value) -> Arg {
        Arg::Json(Some(x.clone()))
    }
}
impl From<&[i64]> for Arg {
    fn from(x: &[i64]) -> Arg {
        Arg::I64s(x.to_vec())
    }
}
impl From<Vec<i64>> for Arg {
    fn from(x: Vec<i64>) -> Arg {
        Arg::I64s(x)
    }
}
impl From<&[String]> for Arg {
    fn from(x: &[String]) -> Arg {
        Arg::Texts(x.to_vec())
    }
}
impl From<Vec<String>> for Arg {
    fn from(x: Vec<String>) -> Arg {
        Arg::Texts(x)
    }
}

enum Text<'a> {
    Static(&'static Sql),
    Dynamic(Cow<'a, str>),
}

/// A query with its arguments, ready to run on a [`Db`] or [`Tx`].
pub struct Query<'a> {
    text: Text<'a>,
    args: Vec<Arg>,
}

/// A query whose SQL is built at run time (identical for both backends, or
/// built per backend by the caller from [`Db::backend`]).
pub fn query<'a>(sql: impl Into<Cow<'a, str>>) -> Query<'a> {
    Query {
        text: Text::Dynamic(sql.into()),
        args: Vec::new(),
    }
}

// ----- decoding --------------------------------------------------------------
//
// `FromRow` and `Col` stand for "decodable on every compiled backend". The
// helper traits are empty when their backend is not compiled in.

#[cfg(feature = "postgres")]
#[doc(hidden)]
pub trait PgRowOk: for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> {}
#[cfg(feature = "postgres")]
impl<T: for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>> PgRowOk for T {}
#[cfg(not(feature = "postgres"))]
#[doc(hidden)]
pub trait PgRowOk {}
#[cfg(not(feature = "postgres"))]
impl<T> PgRowOk for T {}

#[cfg(feature = "sqlite")]
#[doc(hidden)]
pub trait LiteRowOk: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow> {}
#[cfg(feature = "sqlite")]
impl<T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>> LiteRowOk for T {}
#[cfg(not(feature = "sqlite"))]
#[doc(hidden)]
pub trait LiteRowOk {}
#[cfg(not(feature = "sqlite"))]
impl<T> LiteRowOk for T {}

/// A row type decodable on every compiled backend (derive `sqlx::FromRow`).
pub trait FromRow: PgRowOk + LiteRowOk + Send + Unpin {}
impl<T: PgRowOk + LiteRowOk + Send + Unpin> FromRow for T {}

#[cfg(feature = "postgres")]
#[doc(hidden)]
pub trait PgColOk: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> {}
#[cfg(feature = "postgres")]
impl<T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>> PgColOk for T {}
#[cfg(not(feature = "postgres"))]
#[doc(hidden)]
pub trait PgColOk {}
#[cfg(not(feature = "postgres"))]
impl<T> PgColOk for T {}

#[cfg(feature = "sqlite")]
#[doc(hidden)]
pub trait LiteColOk: for<'r> sqlx::Decode<'r, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite> {}
#[cfg(feature = "sqlite")]
impl<T: for<'r> sqlx::Decode<'r, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite>> LiteColOk for T {}
#[cfg(not(feature = "sqlite"))]
#[doc(hidden)]
pub trait LiteColOk {}
#[cfg(not(feature = "sqlite"))]
impl<T> LiteColOk for T {}

/// A column type decodable on every compiled backend.
pub trait Col: PgColOk + LiteColOk + Send + Unpin {}
impl<T: PgColOk + LiteColOk + Send + Unpin> Col for T {}

/// A row whose columns are only known at run time.
pub enum Row {
    #[cfg(feature = "postgres")]
    Pg(sqlx::postgres::PgRow),
    #[cfg(feature = "sqlite")]
    Lite(sqlx::sqlite::SqliteRow),
}

impl Row {
    /// The column `name`, decoded as `T`.
    pub fn get<T: Col>(&self, name: &str) -> Result<T, sqlx::Error> {
        use sqlx::Row as _;
        match self {
            #[cfg(feature = "postgres")]
            Row::Pg(r) => r.try_get(name),
            #[cfg(feature = "sqlite")]
            Row::Lite(r) => r.try_get(name),
        }
    }
}

// ----- executors -------------------------------------------------------------

/// What a query runs on: the pool ([`Db`]), a transaction ([`Tx`]) or a
/// held connection ([`Conn`]).
pub trait Exec: Send {
    #[doc(hidden)]
    fn ex(&mut self) -> Ex<'_>;
}

#[doc(hidden)]
pub enum Ex<'a> {
    /// The pool and the id of its [`Db`] (for [`Db::pinned`]).
    #[cfg(feature = "postgres")]
    PgPool(&'a sqlx::PgPool, u64),
    #[cfg(feature = "postgres")]
    PgConn(&'a mut sqlx::PgConnection),
    #[cfg(feature = "sqlite")]
    LitePool(&'a sqlx::SqlitePool),
    #[cfg(feature = "sqlite")]
    LiteConn(&'a mut sqlx::SqliteConnection),
}

impl Ex<'_> {
    fn backend(&self) -> Backend {
        match self {
            #[cfg(feature = "postgres")]
            Ex::PgPool(..) | Ex::PgConn(_) => Backend::Postgres,
            #[cfg(feature = "sqlite")]
            Ex::LitePool(_) | Ex::LiteConn(_) => Backend::Sqlite,
        }
    }
}

impl Exec for &Db {
    fn ex(&mut self) -> Ex<'_> {
        match &self.pool {
            #[cfg(feature = "postgres")]
            crate::Pool::Pg(p) => Ex::PgPool(p, self.id),
            #[cfg(feature = "sqlite")]
            crate::Pool::Lite(p) => Ex::LitePool(p),
        }
    }
}

/// A transaction. Dropping it without [`Tx::commit`] rolls back.
pub struct Tx {
    inner: TxInner,
    db: Db,
}

enum TxInner {
    #[cfg(feature = "postgres")]
    Pg(sqlx::Transaction<'static, sqlx::Postgres>),
    #[cfg(feature = "sqlite")]
    Lite(sqlx::Transaction<'static, sqlx::Sqlite>),
}

impl Tx {
    pub(crate) async fn begin(db: &Db) -> Result<Tx, sqlx::Error> {
        let inner = match &db.pool {
            #[cfg(feature = "postgres")]
            crate::Pool::Pg(p) => TxInner::Pg(p.begin().await?),
            // IMMEDIATE takes the write lock up front: a deferred
            // transaction that reads first and writes later can fail with
            // SQLITE_BUSY when another connection writes in between.
            #[cfg(feature = "sqlite")]
            crate::Pool::Lite(p) => TxInner::Lite(p.begin_with("BEGIN IMMEDIATE").await?),
        };
        Ok(Tx {
            inner,
            db: db.clone(),
        })
    }

    pub async fn commit(self) -> Result<(), sqlx::Error> {
        match self.inner {
            #[cfg(feature = "postgres")]
            TxInner::Pg(t) => t.commit().await,
            #[cfg(feature = "sqlite")]
            TxInner::Lite(t) => t.commit().await,
        }
    }

    pub fn backend(&self) -> Backend {
        self.db.backend()
    }

    /// The database this transaction belongs to.
    pub fn db(&self) -> &Db {
        &self.db
    }
}

impl Exec for &mut Tx {
    fn ex(&mut self) -> Ex<'_> {
        match &mut self.inner {
            #[cfg(feature = "postgres")]
            TxInner::Pg(t) => Ex::PgConn(t),
            #[cfg(feature = "sqlite")]
            TxInner::Lite(t) => Ex::LiteConn(t),
        }
    }
}

/// A connection held from the pool (several queries on one connection
/// without a transaction).
pub struct Conn {
    inner: ConnInner,
    /// Taken from [`Db::pinned`]'s slot (this `Db`'s id), given back on drop.
    #[cfg(feature = "postgres")]
    pinned: Option<u64>,
}

enum ConnInner {
    /// Always `Some` until the connection is given back.
    #[cfg(feature = "postgres")]
    Pg(Option<sqlx::pool::PoolConnection<sqlx::Postgres>>),
    #[cfg(feature = "sqlite")]
    Lite(sqlx::pool::PoolConnection<sqlx::Sqlite>),
}

impl Conn {
    pub(crate) async fn acquire(db: &Db) -> Result<Conn, sqlx::Error> {
        let inner = match &db.pool {
            #[cfg(feature = "postgres")]
            crate::Pool::Pg(p) => {
                if let Some(held) = crate::pin::take(db.id) {
                    let c = match held {
                        Some(c) => c,
                        None => p.acquire().await?,
                    };
                    return Ok(Conn {
                        inner: ConnInner::Pg(Some(c)),
                        pinned: Some(db.id),
                    });
                }
                ConnInner::Pg(Some(p.acquire().await?))
            }
            #[cfg(feature = "sqlite")]
            crate::Pool::Lite(p) => ConnInner::Lite(p.acquire().await?),
        };
        Ok(Conn {
            inner,
            #[cfg(feature = "postgres")]
            pinned: None,
        })
    }
}

#[cfg(feature = "postgres")]
impl Drop for Conn {
    fn drop(&mut self) {
        #[allow(irrefutable_let_patterns)]
        if let (Some(id), ConnInner::Pg(c)) = (self.pinned, &mut self.inner)
            && let Some(c) = c.take()
        {
            crate::pin::put(id, c);
        }
    }
}

impl Exec for &mut Conn {
    fn ex(&mut self) -> Ex<'_> {
        match &mut self.inner {
            #[cfg(feature = "postgres")]
            ConnInner::Pg(c) => Ex::PgConn(c.as_mut().expect("held until dropped")),
            #[cfg(feature = "sqlite")]
            ConnInner::Lite(c) => Ex::LiteConn(c),
        }
    }
}

// ----- running ---------------------------------------------------------------

#[cfg(feature = "postgres")]
type PgArgs = sqlx::postgres::PgArguments;
#[cfg(feature = "sqlite")]
type LiteArgs = sqlx::sqlite::SqliteArguments;

#[cfg(feature = "postgres")]
fn pg_args(args: Vec<Arg>) -> Result<PgArgs, sqlx::Error> {
    use sqlx::Arguments as _;
    let mut a = PgArgs::default();
    let e = |e| sqlx::Error::Encode(e);
    for v in args {
        match v {
            Arg::I64(x) => a.add(x).map_err(e)?,
            Arg::I32(x) => a.add(x).map_err(e)?,
            Arg::F64(x) => a.add(x).map_err(e)?,
            Arg::Bool(x) => a.add(x).map_err(e)?,
            Arg::Text(x) => a.add(x).map_err(e)?,
            Arg::Bytes(x) => a.add(x).map_err(e)?,
            Arg::Json(x) => a.add(x.map(sqlx::types::Json)).map_err(e)?,
            Arg::Time(x) => a.add(x.map(|t| t.0)).map_err(e)?,
            Arg::I64s(x) => a.add(x).map_err(e)?,
            Arg::Texts(x) => a.add(x).map_err(e)?,
        }
    }
    Ok(a)
}

#[cfg(feature = "sqlite")]
fn lite_args(args: Vec<Arg>) -> Result<LiteArgs, sqlx::Error> {
    use sqlx::Arguments as _;
    let mut a = LiteArgs::default();
    let e = |e| sqlx::Error::Encode(e);
    for v in args {
        match v {
            Arg::I64(x) => a.add(x).map_err(e)?,
            Arg::I32(x) => a.add(x).map_err(e)?,
            Arg::F64(x) => a.add(x).map_err(e)?,
            Arg::Bool(x) => a.add(x).map_err(e)?,
            Arg::Text(x) => a.add(x).map_err(e)?,
            Arg::Bytes(x) => a.add(x).map_err(e)?,
            Arg::Json(x) => a.add(x.map(|j| j.to_string())).map_err(e)?,
            Arg::Time(x) => a.add(x.map(|t| t.sqlite_text())).map_err(e)?,
            Arg::I64s(x) => a.add(serde_json::Value::from(x).to_string()).map_err(e)?,
            Arg::Texts(x) => a.add(serde_json::Value::from(x).to_string()).map_err(e)?,
        }
    }
    Ok(a)
}

/// sqlx 0.9 wants proof that query text is not built from user input. Ours
/// comes from the statement catalogue, or (content tables) is built with
/// validated, quoted identifiers and bound values only (DECISIONS.md, v0.2
/// and v0.5), so it is asserted safe here, in one place.
fn sql_text(text: &str) -> sqlx::AssertSqlSafe<String> {
    sqlx::AssertSqlSafe(text.to_owned())
}

/// Run `$body` with `$q` bound to the backend's `sqlx` query and `$x` to
/// its executor.
macro_rules! dispatch {
    ($self:ident, $ex:ident, |$q:ident, $x:ident| $body:expr) => {{
        let mut ex = $ex;
        let ex = ex.ex();
        let Query { text, args } = $self;
        let text = text_for(&text, ex.backend())?;
        match ex {
            #[cfg(feature = "postgres")]
            Ex::PgPool(p, id) => {
                let $q = (sql_text(&text), pg_args(args)?);
                match crate::pin::take(id) {
                    None => {
                        let $x = p;
                        $body
                    }
                    // Inside `Db::pinned`: the render's one connection.
                    Some(held) => {
                        let mut c = match held {
                            Some(c) => c,
                            None => p.acquire().await?,
                        };
                        let r = {
                            let $x = &mut *c;
                            $body
                        };
                        crate::pin::put(id, c);
                        r
                    }
                }
            }
            #[cfg(feature = "postgres")]
            Ex::PgConn(c) => {
                let $q = (sql_text(&text), pg_args(args)?);
                let $x = &mut *c;
                $body
            }
            #[cfg(feature = "sqlite")]
            Ex::LitePool(p) => {
                let $q = (sql_text(&text), lite_args(args)?);
                let $x = p;
                $body
            }
            #[cfg(feature = "sqlite")]
            Ex::LiteConn(c) => {
                let $q = (sql_text(&text), lite_args(args)?);
                let $x = &mut *c;
                $body
            }
        }
    }};
}

impl<'a> Query<'a> {
    /// Add the next argument (`$1`, `$2`, ... in order).
    pub fn bind(mut self, a: impl Into<Arg>) -> Self {
        self.args.push(a.into());
        self
    }

    /// Add an argument in place (for queries built in a loop).
    pub fn push(&mut self, a: impl Into<Arg>) {
        self.args.push(a.into());
    }
}

fn text_for<'t>(text: &'t Text<'_>, backend: Backend) -> Result<Cow<'t, str>, sqlx::Error> {
    {
        match text {
            Text::Static(s) => s.text(backend).map(Cow::Borrowed).ok_or_else(|| {
                sqlx::Error::Configuration(
                    format!(
                        "{} is a PostgreSQL-only statement; this database is {backend}",
                        s.name
                    )
                    .into(),
                )
            }),
            Text::Dynamic(t) => Ok(Cow::Borrowed(t.as_ref())),
        }
    }
}

impl Query<'_> {
    /// Run a statement; returns the number of rows affected.
    pub async fn execute(self, ex: impl Exec) -> Result<u64, sqlx::Error> {
        dispatch!(self, ex, |q, x| {
            Ok(sqlx::query_with(q.0, q.1).execute(x).await?.rows_affected())
        })
    }

    pub async fn fetch_all<T: FromRow>(self, ex: impl Exec) -> Result<Vec<T>, sqlx::Error> {
        dispatch!(self, ex, |q, x| sqlx::query_as_with::<_, T, _>(q.0, q.1)
            .fetch_all(x)
            .await)
    }

    pub async fn fetch_optional<T: FromRow>(self, ex: impl Exec) -> Result<Option<T>, sqlx::Error> {
        dispatch!(self, ex, |q, x| sqlx::query_as_with::<_, T, _>(q.0, q.1)
            .fetch_optional(x)
            .await)
    }

    pub async fn fetch_one<T: FromRow>(self, ex: impl Exec) -> Result<T, sqlx::Error> {
        dispatch!(self, ex, |q, x| sqlx::query_as_with::<_, T, _>(q.0, q.1)
            .fetch_one(x)
            .await)
    }

    /// The first column of the one row.
    pub async fn scalar<T: Col>(self, ex: impl Exec) -> Result<T, sqlx::Error> {
        Ok(self.fetch_one::<(T,)>(ex).await?.0)
    }

    /// The first column of the row, if there is one.
    pub async fn scalar_opt<T: Col>(self, ex: impl Exec) -> Result<Option<T>, sqlx::Error> {
        Ok(self.fetch_optional::<(T,)>(ex).await?.map(|r| r.0))
    }

    /// The first column of every row.
    pub async fn scalars<T: Col>(self, ex: impl Exec) -> Result<Vec<T>, sqlx::Error> {
        Ok(self
            .fetch_all::<(T,)>(ex)
            .await?
            .into_iter()
            .map(|r| r.0)
            .collect())
    }

    /// Rows with columns decoded by name ([`Row::get`]).
    pub async fn rows(self, ex: impl Exec) -> Result<Vec<Row>, sqlx::Error> {
        dispatch!(self, ex, |q, x| {
            let rows = sqlx::query_with(q.0, q.1).fetch_all(x).await?;
            Ok(rows.into_iter().map(wrap).collect())
        })
    }

    pub async fn row_opt(self, ex: impl Exec) -> Result<Option<Row>, sqlx::Error> {
        dispatch!(self, ex, |q, x| {
            let row = sqlx::query_with(q.0, q.1).fetch_optional(x).await?;
            Ok(row.map(wrap))
        })
    }
}

trait WrapRow {
    fn wrap(self) -> Row;
}
#[cfg(feature = "postgres")]
impl WrapRow for sqlx::postgres::PgRow {
    fn wrap(self) -> Row {
        Row::Pg(self)
    }
}
#[cfg(feature = "sqlite")]
impl WrapRow for sqlx::sqlite::SqliteRow {
    fn wrap(self) -> Row {
        Row::Lite(self)
    }
}
fn wrap<R: WrapRow>(r: R) -> Row {
    r.wrap()
}

/// Run SQL text with several statements and no arguments (migrations,
/// test setup).
pub async fn raw(ex: impl Exec, sql: &str) -> Result<(), sqlx::Error> {
    let mut ex = ex;
    match ex.ex() {
        #[cfg(feature = "postgres")]
        Ex::PgPool(p, _) => sqlx::raw_sql(sql_text(sql)).execute(p).await.map(|_| ()),
        #[cfg(feature = "postgres")]
        Ex::PgConn(c) => sqlx::raw_sql(sql_text(sql))
            .execute(&mut *c)
            .await
            .map(|_| ()),
        #[cfg(feature = "sqlite")]
        Ex::LitePool(p) => sqlx::raw_sql(sql_text(sql)).execute(p).await.map(|_| ()),
        #[cfg(feature = "sqlite")]
        Ex::LiteConn(c) => sqlx::raw_sql(sql_text(sql))
            .execute(&mut *c)
            .await
            .map(|_| ()),
    }
}

/// Prepare `sql` without running it: an error names what the database
/// refused (unknown table or column, syntax). Used by the catalogue test.
pub async fn check(db: &Db, sql: &str) -> Result<(), sqlx::Error> {
    use sqlx::{Executor as _, SqlSafeStr as _};
    match &db.pool {
        #[cfg(feature = "postgres")]
        crate::Pool::Pg(p) => p.prepare(sql_text(sql).into_sql_str()).await.map(|_| ()),
        #[cfg(feature = "sqlite")]
        crate::Pool::Lite(p) => p.prepare(sql_text(sql).into_sql_str()).await.map(|_| ()),
    }
}
