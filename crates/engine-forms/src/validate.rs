//! Server-side validation of a submission. Browsers check `required`,
//! `type="email"` and so on too, but only this counts.

use std::collections::BTreeMap;

use engine_core::web::FormData;
use serde_json::Value as Json;

use crate::def::{Field, FormDef, Kind};

pub type Values = BTreeMap<String, Json>;
pub type Errors = BTreeMap<String, String>;

/// An email address's shape (not deliverability).
pub fn valid_email(s: &str) -> bool {
    let Some((local, domain)) = s.split_once('@') else {
        return false;
    };
    (3..=254).contains(&s.len())
        && !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.contains('@')
        && !s.chars().any(|c| {
            c.is_whitespace() || c.is_control() || matches!(c, '<' | '>' | '"' | ',' | ';' | '\\')
        })
}

fn valid_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
        && matches!(s[5..7].parse::<u8>(), Ok(1..=12))
        && matches!(s[8..10].parse::<u8>(), Ok(1..=31))
}

/// Clean text: no control characters (newlines and tabs only in
/// textareas), trimmed.
fn clean(f: &Field, raw: &str) -> String {
    let multi = f.kind == Kind::Textarea;
    raw.replace("\r\n", "\n")
        .chars()
        .filter(|c| !c.is_control() || (multi && (*c == '\n' || *c == '\t')))
        .collect::<String>()
        .trim()
        .to_string()
}

fn check_field(f: &Field, raw: Option<&str>) -> Result<Option<Json>, String> {
    if f.kind == Kind::Checkbox {
        let on = raw.is_some_and(|v| !v.is_empty());
        if f.required && !on {
            return Err("must be ticked".into());
        }
        return Ok(Some(Json::Bool(on)));
    }
    let v = clean(f, raw.unwrap_or(""));
    if v.is_empty() {
        return if f.required {
            Err("is required".into())
        } else {
            Ok(None)
        };
    }
    let chars = v.chars().count();
    if chars > f.max_len() {
        return Err(format!("is longer than {} characters", f.max_len()));
    }
    if let Some(min) = f.min_length
        && chars < min
    {
        return Err(format!("must be at least {min} characters"));
    }
    match f.kind {
        Kind::Email if !valid_email(&v) => Err("is not a valid email address".into()),
        Kind::Email => Ok(Some(Json::String(v.to_lowercase()))),
        Kind::Url if !(v.starts_with("https://") || v.starts_with("http://")) => {
            Err("must start with https:// or http://".into())
        }
        Kind::Date if !valid_date(&v) => Err("is not a date (YYYY-MM-DD)".into()),
        Kind::Tel
            if !v
                .chars()
                .all(|c| c.is_ascii_digit() || matches!(c, ' ' | '+' | '-' | '(' | ')' | '.'))
                || chars > 30 =>
        {
            Err("is not a phone number".into())
        }
        Kind::Number => {
            let n: f64 = v.parse().map_err(|_| "is not a number".to_string())?;
            if !n.is_finite() {
                return Err("is not a number".into());
            }
            if let Some(min) = f.min
                && n < min
            {
                return Err(format!("must be at least {min}"));
            }
            if let Some(max) = f.max
                && n > max
            {
                return Err(format!("must be at most {max}"));
            }
            Ok(Some(
                serde_json::Number::from_f64(n)
                    .map(Json::Number)
                    .unwrap_or(Json::Null),
            ))
        }
        Kind::Select | Kind::Radio if !f.options.iter().any(|o| o == &v) => {
            Err("is not one of the choices".into())
        }
        _ => Ok(Some(Json::String(v))),
    }
}

/// Validate `fields` of `form`. Values of other fields are ignored.
pub fn validate(fields: &[&Field], form: &FormData) -> (Values, Errors) {
    let mut values = Values::new();
    let mut errors = Errors::new();
    for f in fields {
        match check_field(f, form.get(&f.name)) {
            Ok(Some(v)) => {
                values.insert(f.name.clone(), v);
            }
            Ok(None) => {}
            Err(e) => {
                errors.insert(f.name.clone(), e);
            }
        }
    }
    (values, errors)
}

/// Every field of the form.
pub fn validate_all(def: &FormDef, form: &FormData) -> (Values, Errors) {
    let all: Vec<&Field> = def.fields.iter().collect();
    validate(&all, form)
}

/// The submitted text of each field, as typed (to show the form again).
pub fn raw_values(def: &FormDef, form: &FormData) -> BTreeMap<String, String> {
    def.fields
        .iter()
        .filter_map(|f| {
            form.get(&f.name)
                .map(|v| (f.name.clone(), v.chars().take(f.max_len() + 1).collect()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def() -> FormDef {
        FormDef::parse_toml(
            "t",
            r#"
title = "T"
[[fields]]
name = "email"
type = "email"
required = true
[[fields]]
name = "age"
type = "number"
min = 18
max = 120
[[fields]]
name = "size"
type = "select"
options = ["S", "M"]
[[fields]]
name = "agree"
type = "checkbox"
required = true
[[fields]]
name = "note"
type = "textarea"
max_length = 10
[[fields]]
name = "when"
type = "date"
"#,
        )
        .unwrap()
    }

    fn form(pairs: &[(&str, &str)]) -> FormData {
        FormData(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    #[test]
    fn good_and_bad_values() {
        let d = def();
        let (v, e) = validate_all(
            &d,
            &form(&[
                ("email", " Ada@Example.ORG "),
                ("age", "42"),
                ("size", "M"),
                ("agree", "on"),
                ("note", "a\r\nb\u{0}c"),
                ("when", "2026-11-26"),
                ("extra", "ignored"),
            ]),
        );
        assert!(e.is_empty(), "{e:?}");
        assert_eq!(v["email"], "ada@example.org");
        assert_eq!(v["age"], 42.0);
        assert_eq!(v["agree"], true);
        assert_eq!(v["note"], "a\nbc");
        assert!(!v.contains_key("extra"));
        let (_, e) = validate_all(
            &d,
            &form(&[
                ("email", "nope"),
                ("age", "12"),
                ("size", "XL"),
                ("note", "01234567890"),
                ("when", "2026-13-01"),
            ]),
        );
        assert_eq!(
            e.keys().collect::<Vec<_>>(),
            ["age", "agree", "email", "note", "size", "when"]
        );
        assert_eq!(e["agree"], "must be ticked");
        let (_, e) = validate_all(&d, &form(&[]));
        assert_eq!(e["email"], "is required");
    }
}
