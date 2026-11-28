//! [`Timestamp`]: a UTC instant, stored as `timestamptz`.
//!
//! The engine computes times in Rust and binds them (`at > $3` with
//! `Timestamp::now().minus_minutes(15)`); that was needed while SQLite was
//! supported and still keeps a statement's times testable. It serializes
//! (JSON, templates) as `YYYY-MM-DDTHH:MM:SSZ`.

use chrono::{DateTime, NaiveDateTime, SecondsFormat, TimeZone, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(pub DateTime<Utc>);

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

    /// Parse the formats this engine writes and reads: RFC 3339 (with `Z`
    /// or an offset), and `YYYY-MM-DD HH:MM:SS[.fff]` (taken as UTC).
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_formats() {
        let a = Timestamp::parse("2026-11-26T07:00:00Z").unwrap();
        let b = a.plus_seconds(1);
        assert!(a < b);
        assert_eq!(a.iso(), "2026-11-26T07:00:00Z");
        assert_eq!(Timestamp::parse("2026-11-26 07:00:00").unwrap(), a);
        assert_eq!(Timestamp::parse("2026-11-26T07:00:00.000Z").unwrap(), a);
        assert_eq!(Timestamp::parse("2026-11-26T08:00:00+01:00").unwrap(), a);
        let j = serde_json::to_string(&a).unwrap();
        assert_eq!(j, "\"2026-11-26T07:00:00Z\"");
        assert_eq!(serde_json::from_str::<Timestamp>(&j).unwrap(), a);
    }
}
