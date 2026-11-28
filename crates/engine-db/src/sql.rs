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
    PgPool(&'a sqlx::PgPool, u64, &'a std::sync::Arc<crate::Health>),
    #[cfg(feature = "postgres")]
    PgConn(
        &'a mut sqlx::PgConnection,
        &'a std::sync::Arc<crate::Health>,
    ),
    /// A held [`Conn`]: its connection (replaced when it breaks), pool
    /// and health.
    #[cfg(feature = "postgres")]
    PgHeld(
        &'a mut Option<sqlx::pool::PoolConnection<sqlx::Postgres>>,
        &'a sqlx::PgPool,
        &'a std::sync::Arc<crate::Health>,
    ),
    #[cfg(feature = "sqlite")]
    LitePool(&'a sqlx::SqlitePool),
    #[cfg(feature = "sqlite")]
    LiteConn(&'a mut sqlx::SqliteConnection),
}

impl Ex<'_> {
    fn backend(&self) -> Backend {
        match self {
            #[cfg(feature = "postgres")]
            Ex::PgPool(..) | Ex::PgConn(..) | Ex::PgHeld(..) => Backend::Postgres,
            #[cfg(feature = "sqlite")]
            Ex::LitePool(_) | Ex::LiteConn(_) => Backend::Sqlite,
        }
    }
}

impl Exec for &Db {
    fn ex(&mut self) -> Ex<'_> {
        match &self.pool {
            #[cfg(feature = "postgres")]
            crate::Pool::Pg(p) => Ex::PgPool(p, self.id, &self.health),
            #[cfg(feature = "sqlite")]
            crate::Pool::Lite(p) => Ex::LitePool(p),
        }
    }
}

/// How long a checkout may wait before one probe asks whether the server is
/// there at all: sqlx retries a refused connection until the pool's 10 s
/// timeout, so a stopped database would hold every request that long.
#[cfg(feature = "postgres")]
const SLOW_CHECKOUT: std::time::Duration = std::time::Duration::from_millis(500);

/// A pooled connection, failing fast (with [`crate::health::unavailable`])
/// when the database turns out to be unreachable instead of waiting for the
/// pool's timeout. With idle connections in the pool it is a plain
/// checkout (no timer); a checkout that fails, or one that has to open a
/// connection and takes longer than [`SLOW_CHECKOUT`], asks one probe
/// (shared, at most one per 250 ms), which marks the database down if it is.
#[cfg(feature = "postgres")]
pub(crate) async fn pg_checkout(
    p: &sqlx::PgPool,
    health: &std::sync::Arc<crate::Health>,
) -> Result<sqlx::pool::PoolConnection<sqlx::Postgres>, sqlx::Error> {
    fail_fast(p, health, p.acquire()).await
}

/// Whether a query may go ahead: the database is up, or it was asleep and
/// this demand woke it (bounded by [`crate::sleep::WAKE_WAIT`]). Background
/// work ([`crate::sleep::quietly`]) never wakes it.
#[cfg(feature = "postgres")]
pub(crate) async fn usable(health: &std::sync::Arc<crate::Health>) -> bool {
    !health.is_down()
        || (health.is_asleep()
            && !crate::sleep::is_quiet()
            && health.wait_awake(crate::sleep::WAKE_WAIT).await)
}

/// [`pg_checkout`] for anything that checks a connection out of `p` first
/// (`p.begin()`).
#[cfg(feature = "postgres")]
async fn fail_fast<T>(
    p: &sqlx::PgPool,
    health: &std::sync::Arc<crate::Health>,
    acquire: impl std::future::Future<Output = Result<T, sqlx::Error>>,
) -> Result<T, sqlx::Error> {
    const PROBE_AT_MOST: std::time::Duration = std::time::Duration::from_millis(250);
    // Asleep (the host's watchdog): a demand wakes it and waits, bounded;
    // background work does not ([`crate::sleep::quietly`]).
    if !usable(health).await {
        return Err(crate::health::unavailable());
    }
    if health.counting() && !crate::sleep::is_quiet() {
        health.note_use();
    }
    let r = if p.num_idle() > 0 {
        acquire.await
    } else {
        tokio::pin!(acquire);
        tokio::select! {
            r = &mut acquire => r,
            () = tokio::time::sleep(SLOW_CHECKOUT) => {
                if !health.check_recent(PROBE_AT_MOST).await {
                    return Err(crate::health::unavailable());
                }
                acquire.await
            }
        }
    };
    match r {
        Err(e) if crate::health::is_unavailable(&e) => {
            if !health.check_recent(PROBE_AT_MOST).await {
                return Err(crate::health::unavailable());
            }
            Err(e)
        }
        r => r,
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
            // `BEGIN` on a broken connection changes nothing: start again
            // on a fresh one (`crate::health`).
            #[cfg(feature = "postgres")]
            crate::Pool::Pg(p) => {
                let mut tries = 0;
                loop {
                    if !usable(&db.health).await {
                        return Err(crate::health::unavailable());
                    }
                    match fail_fast(p, &db.health, p.begin()).await {
                        Ok(t) => break TxInner::Pg(t),
                        Err(e) => {
                            if crate::health::disconnect(&e).is_none() {
                                return Err(e);
                            }
                            tries += 1;
                            if !db.health.recover(db.health.mark()).await {
                                return Err(crate::health::unavailable());
                            }
                            if tries >= 3 {
                                return Err(e);
                            }
                        }
                    }
                }
            }
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
            TxInner::Pg(t) => Ex::PgConn(t, &self.db.health),
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
    /// `conn` is `Some` until given back (or closed after it broke; the
    /// next query takes a fresh one).
    #[cfg(feature = "postgres")]
    Pg {
        conn: Option<sqlx::pool::PoolConnection<sqlx::Postgres>>,
        pool: sqlx::PgPool,
        health: std::sync::Arc<crate::Health>,
    },
    #[cfg(feature = "sqlite")]
    Lite(sqlx::pool::PoolConnection<sqlx::Sqlite>),
}

impl Conn {
    pub(crate) async fn acquire(db: &Db) -> Result<Conn, sqlx::Error> {
        let inner = match &db.pool {
            #[cfg(feature = "postgres")]
            crate::Pool::Pg(p) => {
                if !usable(&db.health).await {
                    return Err(crate::health::unavailable());
                }
                if let Some(held) = crate::pin::take(db.id) {
                    let c = match held {
                        Some(c) => c,
                        None => pg_checkout(p, &db.health).await?,
                    };
                    return Ok(Conn {
                        inner: ConnInner::Pg {
                            conn: Some(c),
                            pool: p.clone(),
                            health: db.health.clone(),
                        },
                        pinned: Some(db.id),
                    });
                }
                ConnInner::Pg {
                    conn: Some(pg_checkout(p, &db.health).await?),
                    pool: p.clone(),
                    health: db.health.clone(),
                }
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
        if let (Some(id), ConnInner::Pg { conn, .. }) = (self.pinned, &mut self.inner)
            && let Some(c) = conn.take()
        {
            crate::pin::put(id, c);
        }
    }
}

impl Exec for &mut Conn {
    fn ex(&mut self) -> Ex<'_> {
        match &mut self.inner {
            #[cfg(feature = "postgres")]
            ConnInner::Pg { conn, pool, health } => Ex::PgHeld(conn, pool, health),
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
fn pg_args(args: &[Arg]) -> Result<PgArgs, sqlx::Error> {
    use sqlx::Arguments as _;
    let mut a = PgArgs::default();
    let e = |e| sqlx::Error::Encode(e);
    // By reference: the arguments stay for a retry (see `crate::health`).
    for v in args {
        match v {
            Arg::I64(x) => a.add(x).map_err(e)?,
            Arg::I32(x) => a.add(x).map_err(e)?,
            Arg::F64(x) => a.add(x).map_err(e)?,
            Arg::Bool(x) => a.add(x).map_err(e)?,
            Arg::Text(x) => a.add(x).map_err(e)?,
            Arg::Bytes(x) => a.add(x).map_err(e)?,
            Arg::Json(x) => a.add(x.as_ref().map(sqlx::types::Json)).map_err(e)?,
            Arg::Time(x) => a.add(x.as_ref().map(|t| t.0)).map_err(e)?,
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

/// Pins the error type of a query's future (for `?` inside it).
#[cfg(feature = "postgres")]
async fn typed<T>(
    f: impl std::future::Future<Output = Result<T, sqlx::Error>>,
) -> Result<T, sqlx::Error> {
    f.await
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
            // On the pool, a query whose connection turns out to be broken
            // (a database restart) runs again on a fresh one when that is
            // safe (`crate::health`).
            #[cfg(feature = "postgres")]
            Ex::PgPool(p, id, health) => {
                let mut tries = 0;
                loop {
                    if !crate::sql::usable(health).await {
                        break Err(crate::health::unavailable());
                    }
                    let $q = (sql_text(&text), pg_args(&args)?);
                    let r = match crate::pin::take(id) {
                        // A connection of its own for this query (as sqlx
                        // would take), so a stopped server fails fast.
                        None => match pg_checkout(p, health).await {
                            Err(e) => Err(e),
                            Ok(mut c) => {
                                let r = {
                                    let $x = &mut *c;
                                    typed(async move { $body }).await
                                };
                                if r.as_ref()
                                    .is_err_and(|e| crate::health::disconnect(e).is_some())
                                {
                                    c.close_on_drop();
                                }
                                r
                            }
                        },
                        // Inside `Db::pinned`: the render's one connection.
                        Some(held) => {
                            let c = match held {
                                Some(c) => Ok(c),
                                None => pg_checkout(p, health).await,
                            };
                            match c {
                                Err(e) => Err(e),
                                Ok(mut c) => {
                                    let r = {
                                        let $x = &mut *c;
                                        typed(async move { $body }).await
                                    };
                                    if r.as_ref()
                                        .is_err_and(|e| crate::health::disconnect(e).is_some())
                                    {
                                        // Not given back: closed.
                                        c.close_on_drop();
                                    } else {
                                        crate::pin::put(id, c);
                                    }
                                    r
                                }
                            }
                        }
                    };
                    let kind = match &r {
                        Err(e) => crate::health::disconnect(e),
                        Ok(_) => None,
                    };
                    let Some(kind) = kind else { break r };
                    let seen = health.mark();
                    tries += 1;
                    if !health.recover(seen).await {
                        break Err(crate::health::unavailable());
                    }
                    let safe =
                        kind == crate::health::Disconnect::NotRun || crate::health::is_read(&text);
                    if !safe || tries >= 3 {
                        break r;
                    }
                }
            }
            // In a transaction nothing can be run again, but a broken
            // connection still tells the health state at once.
            #[cfg(feature = "postgres")]
            Ex::PgConn(c, health) => {
                let $q = (sql_text(&text), pg_args(&args)?);
                let $x = &mut *c;
                let r = typed(async move { $body }).await;
                if r.as_ref()
                    .is_err_and(|e| crate::health::disconnect(e).is_some())
                    && !health.recover(health.mark()).await
                {
                    return Err(crate::health::unavailable());
                }
                r
            }
            // A held connection that broke is closed and replaced, and the
            // query run again when that is safe (as on the pool).
            #[cfg(feature = "postgres")]
            Ex::PgHeld(slot, pool, health) => {
                let mut tries = 0;
                loop {
                    if !crate::sql::usable(health).await {
                        break Err(crate::health::unavailable());
                    }
                    let $q = (sql_text(&text), pg_args(&args)?);
                    if slot.is_none() {
                        *slot = Some(pg_checkout(pool, health).await?);
                    }
                    let c = slot.as_mut().expect("just filled");
                    let r = {
                        let $x = &mut **c;
                        typed(async move { $body }).await
                    };
                    let kind = match &r {
                        Err(e) => crate::health::disconnect(e),
                        Ok(_) => None,
                    };
                    let Some(kind) = kind else { break r };
                    if let Some(mut c) = slot.take() {
                        c.close_on_drop();
                    }
                    tries += 1;
                    if !health.recover(health.mark()).await {
                        break Err(crate::health::unavailable());
                    }
                    let safe =
                        kind == crate::health::Disconnect::NotRun || crate::health::is_read(&text);
                    if !safe || tries >= 3 {
                        break r;
                    }
                }
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
        Ex::PgPool(p, ..) => sqlx::raw_sql(sql_text(sql)).execute(p).await.map(|_| ()),
        #[cfg(feature = "postgres")]
        Ex::PgConn(c, _) => sqlx::raw_sql(sql_text(sql))
            .execute(&mut *c)
            .await
            .map(|_| ()),
        #[cfg(feature = "postgres")]
        Ex::PgHeld(slot, pool, health) => {
            if slot.is_none() {
                *slot = Some(pg_checkout(pool, health).await?);
            }
            sqlx::raw_sql(sql_text(sql))
                .execute(&mut **slot.as_mut().expect("just filled"))
                .await
                .map(|_| ())
        }
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
