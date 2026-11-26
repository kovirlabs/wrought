//! [`Timestamp`]: a UTC instant stored as `timestamptz` on Postgres and as
//! ISO-8601 text (`2026-11-26T07:00:00.123Z`, always millisecond precision,
//! so text order is time order) on SQLite.
//!
//! The engine computes times in Rust and binds them (`at > $3` with
//! `Timestamp::now().minus_minutes(15)`) instead of calling `now()` in SQL,
//! so one statement text works on both backends. It serializes (JSON,
//! templates) as `YYYY-MM-DDTHH:MM:SSZ`.

use chrono::{DateTime, NaiveDateTime, SecondsFormat, TimeZone, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(pub DateTime<Utc>);

/// The SQLite storage format (also what `DEFAULT` expressions in the SQLite
/// migrations produce: `strftime('%Y-%m-%dT%H:%M:%fZ', 'now')`).
const SQLITE_FMT: &str = "%Y-%m-%dT%H:%M:%S%.3fZ";

impl Timestamp {
    pub fn now() -> Timestamp {
        Timestamp(Utc::now())
    }

    pub fn from_unix(secs: i64) -> Option<Timestamp> {
        Utc.timestamp_opt(secs, 0).single().map(Timestamp)
    }

    pub fn unix(&self) -> i64 {
        self.0.timestamp()
    }

    pub fn plus_seconds(self, s: i64) -> Timestamp {
        Timestamp(self.0 + chrono::Duration::seconds(s))
    }

    pub fn minus_seconds(self, s: i64) -> Timestamp {
        Timestamp(self.0 - chrono::Duration::seconds(s))
    }

    pub fn plus_minutes(self, m: i64) -> Timestamp {
        self.plus_seconds(m * 60)
    }

    pub fn minus_minutes(self, m: i64) -> Timestamp {
        self.minus_seconds(m * 60)
    }

    pub fn minus_days(self, d: i64) -> Timestamp {
        self.minus_seconds(d * 86_400)
    }

    /// `YYYY-MM-DDTHH:MM:SSZ`.
    pub fn iso(&self) -> String {
        self.0.to_rfc3339_opts(SecondsFormat::Secs, true)
    }

    /// RFC 7231 date for HTTP headers.
    pub fn http_date(&self) -> String {
        self.0.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
    }

    /// The SQLite storage text (`2026-11-26T07:00:00.123Z`).
    pub fn sqlite_text(&self) -> String {
        self.0.format(SQLITE_FMT).to_string()
    }

    /// Parse the formats this engine writes and reads: RFC 3339 (with `Z`
    /// or an offset), SQLite's `YYYY-MM-DD HH:MM:SS[.fff]`.
    pub fn parse(s: &str) -> Option<Timestamp> {
        let s = s.trim();
        if let Ok(t) = DateTime::parse_from_rfc3339(s) {
            return Some(Timestamp(t.with_timezone(&Utc)));
        }
        for f in [
            "%Y-%m-%d %H:%M:%S%.f",
            "%Y-%m-%dT%H:%M:%S%.f",
            "%Y-%m-%d %H:%M:%S",
        ] {
            if let Ok(n) = NaiveDateTime::parse_from_str(s.trim_end_matches('Z'), f) {
                return Some(Timestamp(Utc.from_utc_datetime(&n)));
            }
        }
        None
    }
}

impl std::fmt::Display for Timestamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.iso())
    }
}

impl From<Timestamp> for String {
    fn from(t: Timestamp) -> String {
        t.iso()
    }
}

impl Serialize for Timestamp {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.iso())
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Timestamp::parse(&s).ok_or_else(|| serde::de::Error::custom(format!("bad time `{s}`")))
    }
}

#[cfg(feature = "postgres")]
mod pg {
    use super::*;
    use sqlx::postgres::{PgTypeInfo, PgValueRef};
    use sqlx::{Decode, Postgres, Type};

    impl Type<Postgres> for Timestamp {
        fn type_info() -> PgTypeInfo {
            <DateTime<Utc> as Type<Postgres>>::type_info()
        }
        fn compatible(ty: &PgTypeInfo) -> bool {
            <DateTime<Utc> as Type<Postgres>>::compatible(ty)
        }
    }

    impl<'r> Decode<'r, Postgres> for Timestamp {
        fn decode(v: PgValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
            Ok(Timestamp(<DateTime<Utc> as Decode<Postgres>>::decode(v)?))
        }
    }
}

#[cfg(feature = "sqlite")]
mod lite {
    use super::*;
    use sqlx::sqlite::{SqliteTypeInfo, SqliteValueRef};
    use sqlx::{Decode, Sqlite, Type, TypeInfo, ValueRef};

    impl Type<Sqlite> for Timestamp {
        fn type_info() -> SqliteTypeInfo {
            <String as Type<Sqlite>>::type_info()
        }
        fn compatible(ty: &SqliteTypeInfo) -> bool {
            matches!(
                ty.name(),
                "TEXT" | "DATETIME" | "DATE" | "TIME" | "INTEGER" | "NUMERIC" | "NULL"
            )
        }
    }

    impl<'r> Decode<'r, Sqlite> for Timestamp {
        fn decode(v: SqliteValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
            if v.type_info().name() == "INTEGER" {
                let n = <i64 as Decode<Sqlite>>::decode(v)?;
                return Timestamp::from_unix(n).ok_or_else(|| "time out of range".into());
            }
            let s = <String as Decode<Sqlite>>::decode(v)?;
            Timestamp::parse(&s).ok_or_else(|| format!("bad time `{s}`").into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqlite_text_sorts_like_time() {
        let a = Timestamp::parse("2026-11-26T07:00:00Z").unwrap();
        let b = a.plus_seconds(1);
        let c = Timestamp(a.0 + chrono::Duration::milliseconds(5));
        assert!(a.sqlite_text() < c.sqlite_text());
        assert!(c.sqlite_text() < b.sqlite_text());
        assert_eq!(a.sqlite_text(), "2026-11-26T07:00:00.000Z");
        assert_eq!(a.iso(), "2026-11-26T07:00:00Z");
        assert_eq!(Timestamp::parse("2026-11-26 07:00:00").unwrap(), a);
        assert_eq!(Timestamp::parse(&a.sqlite_text()).unwrap(), a);
        let j = serde_json::to_string(&a).unwrap();
        assert_eq!(j, "\"2026-11-26T07:00:00Z\"");
        assert_eq!(serde_json::from_str::<Timestamp>(&j).unwrap(), a);
    }
}
