//! Queries.
//!
//! Every query on the engine's own tables is a [`Sql`] statement declared
//! with [`statements!`](crate::statements): a name and one PostgreSQL text.
//! Values are bound as [`Arg`]s and rows are decoded with
//! `#[derive(sqlx::FromRow)]` structs, tuples, or the dynamic [`Row`].
//! Queries run on a [`Db`] (the pool, with retries after a broken
//! connection and fail-fast outages, see [`crate::health`]), a [`Tx`] or a
//! held [`Conn`].
//!
//! Each crate lists its statements in a `sql` module; the catalogue test
//! (`crates/engine-cli/tests/queries.rs`) prepares every one of them
//! against a fully migrated database, so a typo in a column name fails a
//! test in seconds instead of at run time. That replaces sqlx's
//! compile-time checking (`query!`), which needs a database at build time;
//! see DECISIONS.md.
//!
//! Queries whose SQL is only known at run time (the per-site
//! `content_<type>` tables) use [`query`] with a `String`.

use std::borrow::Cow;

use crate::{Db, Timestamp};

/// A statement on the engine's own tables.
#[derive(Debug)]
pub struct Sql {
    /// `crate::module::NAME`, for messages and the catalogue test.
    pub name: &'static str,
    pub text: &'static str,
}

impl Sql {
    #[doc(hidden)]
    pub const fn new(name: &'static str, text: &'static str) -> Sql {
        Sql { name, text }
    }

    /// Start a query with this statement.
    #[track_caller]
    pub fn q(&'static self) -> Query<'static> {
        Query {
            text: Cow::Borrowed(self.text),
            args: Vec::new(),
            name: Some(Cow::Borrowed(self.name)),
            caller: std::panic::Location::caller(),
        }
    }

    /// Start a query with this statement and its first argument.
    #[track_caller]
    pub fn bind(&'static self, a: impl Into<Arg>) -> Query<'static> {
        self.q().bind(a)
    }
}

/// Declare statements: `NAME = "sql";`. Also defines
/// `pub static ALL: &[&Sql]`, the module's catalogue.
#[macro_export]
macro_rules! statements {
    ($( $(#[$m:meta])* $name:ident = $text:literal ; )*) => {
        $(
            $(#[$m])*
            pub static $name: $crate::sql::Sql =
                $crate::sql::Sql::new(concat!(module_path!(), "::", stringify!($name)), $text);
        )*
        /// Every statement of this module (for the catalogue test).
        pub static ALL: &[&$crate::sql::Sql] = &[$( &$name ),*];
    };
}

/// A bound value. Every variant carries an `Option` so a SQL `NULL` keeps
/// its type (PostgreSQL types parameters when preparing).
#[derive(Debug, Clone, PartialEq)]
pub enum Arg {
    I64(Option<i64>),
    I32(Option<i32>),
    F64(Option<f64>),
    Bool(Option<bool>),
    Text(Option<String>),
    Bytes(Option<Vec<u8>>),
    /// `jsonb`.
    Json(Option<serde_json::Value>),
    /// `timestamptz`.
    Time(Option<Timestamp>),
    /// `bigint[]` (`= ANY($n)`, `unnest($n)`).
    I64s(Vec<i64>),
    /// `text[]`.
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

/// A query with its arguments, ready to run on a [`Db`], [`Tx`] or
/// [`Conn`].
pub struct Query<'a> {
    text: Cow<'a, str>,
    args: Vec<Arg>,
    /// For `engine dev`'s query log: the statement's catalogue name (or
    /// what ran it) and where in Rust the query was made.
    name: Option<Cow<'static, str>>,
    caller: &'static std::panic::Location<'static>,
}

/// A query whose SQL is built at run time.
#[track_caller]
pub fn query<'a>(sql: impl Into<Cow<'a, str>>) -> Query<'a> {
    Query {
        text: sql.into(),
        args: Vec::new(),
        name: None,
        caller: std::panic::Location::caller(),
    }
}

// ----- engine dev's query log ------------------------------------------------

/// A query being timed for `engine dev` (`None` otherwise: one atomic load,
/// nothing else, see `engine_core::dev`).
struct Timed {
    log: std::sync::Arc<engine_core::dev::RequestLog>,
    sql: String,
    name: Option<Cow<'static, str>>,
    caller: &'static std::panic::Location<'static>,
    start: std::time::Instant,
}

fn timed(q: &Query<'_>) -> Option<Timed> {
    let log = engine_core::dev::current()?;
    Some(Timed {
        log,
        sql: q.text.to_string(),
        name: q.name.clone(),
        caller: q.caller,
        start: std::time::Instant::now(),
    })
}

fn done<T>(t: Option<Timed>, r: &Result<T, sqlx::Error>, rows: impl FnOnce(&T) -> u64) {
    if let Some(t) = t {
        t.log.record(engine_core::dev::QueryRecord {
            sql: t.sql,
            name: t.name,
            caller: Some(t.caller),
            duration: t.start.elapsed(),
            rows: r.as_ref().ok().map(rows),
            error: r.as_ref().err().map(|e| e.to_string()),
        });
    }
}

// ----- decoding --------------------------------------------------------------

/// A row type (derive `sqlx::FromRow`).
pub trait FromRow: for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> + Send + Unpin {}
impl<T: for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> + Send + Unpin> FromRow for T {}

/// A column type.
pub trait Col:
    for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send + Unpin
{
}
impl<T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send + Unpin> Col
    for T
{
}

/// A row whose columns are only known at run time.
pub struct Row(sqlx::postgres::PgRow);

impl Row {
    /// The column `name`, decoded as `T`.
    pub fn get<T: Col>(&self, name: &str) -> Result<T, sqlx::Error> {
        use sqlx::Row as _;
        self.0.try_get(name)
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
    Pool(&'a sqlx::PgPool, u64, &'a std::sync::Arc<crate::Health>),
    /// A transaction's connection.
    Conn(
        &'a mut sqlx::PgConnection,
        &'a std::sync::Arc<crate::Health>,
    ),
    /// A held [`Conn`]: its connection (replaced when it breaks), pool
    /// and health.
    Held(
        &'a mut Option<sqlx::pool::PoolConnection<sqlx::Postgres>>,
        &'a sqlx::PgPool,
        &'a std::sync::Arc<crate::Health>,
    ),
}

impl Exec for &Db {
    fn ex(&mut self) -> Ex<'_> {
        Ex::Pool(&self.pool, self.id, &self.health)
    }
}

/// How long a checkout may wait before one probe asks whether the server is
/// there at all: sqlx retries a refused connection until the pool's 10 s
/// timeout, so a stopped database would hold every request that long.
const SLOW_CHECKOUT: std::time::Duration = std::time::Duration::from_millis(500);

/// A pooled connection, failing fast (with [`crate::health::unavailable`])
/// when the database turns out to be unreachable instead of waiting for the
/// pool's timeout. With idle connections in the pool it is a plain
/// checkout (no timer); a checkout that fails, or one that has to open a
/// connection and takes longer than [`SLOW_CHECKOUT`], asks one probe
/// (shared, at most one per 250 ms), which marks the database down if it is.
pub(crate) async fn pg_checkout(
    p: &sqlx::PgPool,
    health: &std::sync::Arc<crate::Health>,
) -> Result<sqlx::pool::PoolConnection<sqlx::Postgres>, sqlx::Error> {
    fail_fast(p, health, p.acquire()).await
}

/// Whether a query may go ahead: the database is up, or it was asleep and
/// this demand woke it (bounded by [`crate::sleep::WAKE_WAIT`]). Background
/// work ([`crate::sleep::quietly`]) never wakes it.
pub(crate) async fn usable(health: &std::sync::Arc<crate::Health>) -> bool {
    !health.is_down()
        || (health.is_asleep()
            && !crate::sleep::is_quiet()
            && health.wait_awake(crate::sleep::WAKE_WAIT).await)
}

/// [`pg_checkout`] for anything that checks a connection out of `p` first
/// (`p.begin()`).
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
    inner: sqlx::Transaction<'static, sqlx::Postgres>,
    db: Db,
}

impl Tx {
    pub(crate) async fn begin(db: &Db) -> Result<Tx, sqlx::Error> {
        // `BEGIN` on a broken connection changes nothing: start again on a
        // fresh one (`crate::health`).
        let mut tries = 0;
        let inner = loop {
            if !usable(&db.health).await {
                return Err(crate::health::unavailable());
            }
            match fail_fast(&db.pool, &db.health, db.pool.begin()).await {
                Ok(t) => break t,
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
        };
        Ok(Tx {
            inner,
            db: db.clone(),
        })
    }

    pub async fn commit(self) -> Result<(), sqlx::Error> {
        self.inner.commit().await
    }

    /// The database this transaction belongs to.
    pub fn db(&self) -> &Db {
        &self.db
    }
}

impl Exec for &mut Tx {
    fn ex(&mut self) -> Ex<'_> {
        Ex::Conn(&mut self.inner, &self.db.health)
    }
}

/// A connection held from the pool (several queries on one connection
/// without a transaction).
pub struct Conn {
    /// `Some` until given back (or closed after it broke; the next query
    /// takes a fresh one).
    conn: Option<sqlx::pool::PoolConnection<sqlx::Postgres>>,
    pool: sqlx::PgPool,
    health: std::sync::Arc<crate::Health>,
    /// Taken from [`Db::pinned`]'s slot (this `Db`'s id), given back on drop.
    pinned: Option<u64>,
}

impl Conn {
    pub(crate) async fn acquire(db: &Db) -> Result<Conn, sqlx::Error> {
        if !usable(&db.health).await {
            return Err(crate::health::unavailable());
        }
        let (c, pinned) = match crate::pin::take(db.id) {
            Some(Some(c)) => (c, Some(db.id)),
            Some(None) => (pg_checkout(&db.pool, &db.health).await?, Some(db.id)),
            None => (pg_checkout(&db.pool, &db.health).await?, None),
        };
        Ok(Conn {
            conn: Some(c),
            pool: db.pool.clone(),
            health: db.health.clone(),
            pinned,
        })
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        if let (Some(id), Some(c)) = (self.pinned, self.conn.take()) {
            crate::pin::put(id, c);
        }
    }
}

impl Exec for &mut Conn {
    fn ex(&mut self) -> Ex<'_> {
        Ex::Held(&mut self.conn, &self.pool, &self.health)
    }
}

// ----- running ---------------------------------------------------------------

fn pg_args(args: &[Arg]) -> Result<sqlx::postgres::PgArguments, sqlx::Error> {
    use sqlx::Arguments as _;
    let mut a = sqlx::postgres::PgArguments::default();
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

/// sqlx 0.9 wants proof that query text is not built from user input. Ours
/// comes from the statement catalogue, or (content tables) is built with
/// validated, quoted identifiers and bound values only (DECISIONS.md, v0.2
/// and v0.5), so it is asserted safe here, in one place.
fn sql_text(text: &str) -> sqlx::AssertSqlSafe<String> {
    sqlx::AssertSqlSafe(text.to_owned())
}

/// Pins the error type of a query's future (for `?` inside it).
async fn typed<T>(
    f: impl std::future::Future<Output = Result<T, sqlx::Error>>,
) -> Result<T, sqlx::Error> {
    f.await
}

/// Run `$body` with `$q` bound to the sqlx query parts (text, arguments)
/// and `$x` to the connection.
macro_rules! dispatch {
    ($self:ident, $ex:ident, |$q:ident, $x:ident| $body:expr) => {{
        let mut ex = $ex;
        let ex = ex.ex();
        let Query { text, args, .. } = $self;
        match ex {
            // On the pool, a query whose connection turns out to be broken
            // (a database restart) runs again on a fresh one when that is
            // safe (`crate::health`).
            Ex::Pool(p, id, health) => {
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
            Ex::Conn(c, health) => {
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
            Ex::Held(slot, pool, health) => {
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

impl Query<'_> {
    /// Name the query for `engine dev`'s query log (what ran it, when it
    /// is not a catalogue statement).
    pub fn named(mut self, name: impl Into<Cow<'static, str>>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Run a statement; returns the number of rows affected.
    pub async fn execute(self, ex: impl Exec) -> Result<u64, sqlx::Error> {
        let t = timed(&self);
        let r = dispatch!(self, ex, |q, x| {
            Ok(sqlx::query_with(q.0, q.1).execute(x).await?.rows_affected())
        });
        done(t, &r, |n| *n);
        r
    }

    pub async fn fetch_all<T: FromRow>(self, ex: impl Exec) -> Result<Vec<T>, sqlx::Error> {
        let t = timed(&self);
        let r = dispatch!(self, ex, |q, x| sqlx::query_as_with::<_, T, _>(q.0, q.1)
            .fetch_all(x)
            .await);
        done(t, &r, |v| v.len() as u64);
        r
    }

    pub async fn fetch_optional<T: FromRow>(self, ex: impl Exec) -> Result<Option<T>, sqlx::Error> {
        let t = timed(&self);
        let r = dispatch!(self, ex, |q, x| sqlx::query_as_with::<_, T, _>(q.0, q.1)
            .fetch_optional(x)
            .await);
        done(t, &r, |v| v.is_some() as u64);
        r
    }

    pub async fn fetch_one<T: FromRow>(self, ex: impl Exec) -> Result<T, sqlx::Error> {
        let t = timed(&self);
        let r = dispatch!(self, ex, |q, x| sqlx::query_as_with::<_, T, _>(q.0, q.1)
            .fetch_one(x)
            .await);
        done(t, &r, |_| 1);
        r
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
        let t = timed(&self);
        let r = dispatch!(self, ex, |q, x| {
            let rows = sqlx::query_with(q.0, q.1).fetch_all(x).await?;
            Ok(rows.into_iter().map(Row).collect())
        });
        done(t, &r, |v: &Vec<Row>| v.len() as u64);
        r
    }

    pub async fn row_opt(self, ex: impl Exec) -> Result<Option<Row>, sqlx::Error> {
        let t = timed(&self);
        let r = dispatch!(self, ex, |q, x| {
            let row = sqlx::query_with(q.0, q.1).fetch_optional(x).await?;
            Ok(row.map(Row))
        });
        done(t, &r, |v| v.is_some() as u64);
        r
    }
}

/// Run SQL text with several statements and no arguments (migrations,
/// test setup).
pub async fn raw(ex: impl Exec, sql: &str) -> Result<(), sqlx::Error> {
    let mut ex = ex;
    match ex.ex() {
        Ex::Pool(p, ..) => sqlx::raw_sql(sql_text(sql)).execute(p).await.map(|_| ()),
        Ex::Conn(c, _) => sqlx::raw_sql(sql_text(sql))
            .execute(&mut *c)
            .await
            .map(|_| ()),
        Ex::Held(slot, pool, health) => {
            if slot.is_none() {
                *slot = Some(pg_checkout(pool, health).await?);
            }
            sqlx::raw_sql(sql_text(sql))
                .execute(&mut **slot.as_mut().expect("just filled"))
                .await
                .map(|_| ())
        }
    }
}

/// Prepare `sql` without running it: an error names what the database
/// refused (unknown table or column, syntax). Used by the catalogue test.
pub async fn check(db: &Db, sql: &str) -> Result<(), sqlx::Error> {
    use sqlx::{Executor as _, SqlSafeStr as _};
    db.pool
        .prepare(sql_text(sql).into_sql_str())
        .await
        .map(|_| ())
}
