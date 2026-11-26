//! Field values: parsing and validation of input (CLI, seed files, and the
//! admin UI later) against a type's field definitions.

use std::fmt;

use serde_json::{Map, Value as Json};

use crate::sanitize;
use crate::slug;
use crate::types::{BLOCK_ID, FieldDef, FieldKind, Registry, valid_block_id};

/// A validated value, ready to store.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    /// text, markdown, url and select fields.
    Text(String),
    Int(i64),
    Bool(bool),
    /// `YYYY-MM-DD`.
    Date(String),
    Relation(RelRef),
    /// Term names (a term's slug is made from its name).
    Terms(Vec<String>),
    /// Normalised block list.
    Blocks(Json),
}

/// A relation target, by id or by slug (resolved when saving).
#[derive(Debug, Clone, PartialEq)]
pub enum RelRef {
    Id(i64),
    Slug(String),
}

/// Per-field validation messages.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ValidationErrors(pub Vec<(String, String)>);

impl ValidationErrors {
    pub fn push(&mut self, field: &str, msg: impl Into<String>) {
        self.0.push((field.to_string(), msg.into()));
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Display for ValidationErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts: Vec<String> = self
            .0
            .iter()
            .map(|(field, msg)| format!("`{field}` {msg}"))
            .collect();
        write!(f, "invalid content: {}", parts.join("; "))
    }
}

impl std::error::Error for ValidationErrors {}

/// Turn command-line text (`--set name=value`) into input for a field:
/// numbers and booleans stay strings (validation parses them), `terms` split
/// on commas, `blocks` parse as JSON.
pub fn from_text(def: &FieldDef, s: &str) -> Result<Json, String> {
    Ok(match def.kind {
        FieldKind::Terms => Json::Array(
            s.split(',')
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(|t| Json::String(t.to_string()))
                .collect(),
        ),
        FieldKind::Blocks => {
            serde_json::from_str(s).map_err(|e| format!("must be a JSON list of blocks: {e}"))?
        }
        _ if s.is_empty() => Json::Null,
        _ => Json::String(s.to_string()),
    })
}

/// Convert a TOML value (seed files) to input.
pub fn from_toml(v: &toml::Value) -> Json {
    match v {
        toml::Value::String(s) => Json::String(s.clone()),
        toml::Value::Integer(i) => Json::from(*i),
        toml::Value::Float(f) => Json::from(*f),
        toml::Value::Boolean(b) => Json::Bool(*b),
        toml::Value::Datetime(d) => Json::String(d.to_string()),
        toml::Value::Array(a) => Json::Array(a.iter().map(from_toml).collect()),
        toml::Value::Table(t) => Json::Object(
            t.iter()
                .map(|(k, v)| (k.clone(), from_toml(v)))
                .collect::<Map<_, _>>(),
        ),
    }
}

fn is_empty(v: &Json) -> bool {
    match v {
        Json::Null => true,
        Json::String(s) => s.trim().is_empty(),
        Json::Array(a) => a.is_empty(),
        _ => false,
    }
}

/// Whether `s` is a real calendar date `YYYY-MM-DD`.
pub fn valid_date(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return false;
    }
    let num = |r: std::ops::Range<usize>| s.get(r).and_then(|x| x.parse::<u32>().ok());
    let (Some(y), Some(m), Some(d)) = (num(0..4), num(5..7), num(8..10)) else {
        return false;
    };
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let days = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    y >= 1 && (1..=days).contains(&d)
}

/// Validate one field's input.
pub fn validate(reg: &Registry, def: &FieldDef, v: &Json) -> Result<Value, String> {
    if is_empty(v) {
        if def.required {
            return Err("is required".into());
        }
        return Ok(match def.kind {
            FieldKind::Terms => Value::Terms(Vec::new()),
            _ => Value::Null,
        });
    }
    let as_str = || -> Result<&str, String> {
        v.as_str()
            .ok_or_else(|| format!("must be text, got {}", type_name(v)))
    };
    let check_len = |s: &str| -> Result<(), String> {
        match def.max_length {
            Some(max) if s.chars().count() > max => Err(format!("is longer than {max} characters")),
            _ => Ok(()),
        }
    };
    match def.kind {
        FieldKind::Text | FieldKind::Markdown => {
            let s = as_str()?;
            check_len(s)?;
            Ok(Value::Text(s.to_string()))
        }
        FieldKind::Url => {
            let s = as_str()?.trim();
            check_len(s)?;
            if sanitize::safe_url(s).is_none() {
                return Err(
                    "must be an http(s):// or mailto: link, or a path starting with `/`".into(),
                );
            }
            Ok(Value::Text(s.to_string()))
        }
        FieldKind::Select => {
            let s = as_str()?;
            if !def.options.iter().any(|o| o == s) {
                return Err(format!("must be one of: {}", def.options.join(", ")));
            }
            Ok(Value::Text(s.to_string()))
        }
        FieldKind::Token => {
            let s = as_str()?.trim();
            let group = def.token.as_deref().unwrap_or("");
            let names = engine_core::ui::token_choices(group).unwrap_or_default();
            let allowed: Vec<&str> = names
                .iter()
                .copied()
                .filter(|n| def.options.is_empty() || def.options.iter().any(|o| o == n))
                .collect();
            if !allowed.contains(&s) {
                return Err(format!(
                    "must be one of the site's `{group}` design tokens: {}",
                    allowed.join(", ")
                ));
            }
            Ok(Value::Text(s.to_string()))
        }
        FieldKind::Media => {
            let n: i64 = match v {
                Json::Number(n) => n.as_i64().ok_or("must be a media file number")?,
                Json::String(s) => s
                    .trim()
                    .trim_start_matches('#')
                    .parse()
                    .map_err(|_| "must be a media file number")?,
                _ => return Err("must be a media file number".into()),
            };
            if n < 1 {
                return Err("must be a media file number".into());
            }
            Ok(Value::Int(n))
        }
        FieldKind::Integer => {
            let n = match v {
                Json::Number(n) => n.as_i64().ok_or("must be a whole number")?,
                Json::String(s) => s.trim().parse().map_err(|_| "must be a whole number")?,
                _ => return Err("must be a whole number".into()),
            };
            if let Some(min) = def.min
                && n < min
            {
                return Err(format!("must be at least {min}"));
            }
            if let Some(max) = def.max
                && n > max
            {
                return Err(format!("must be at most {max}"));
            }
            Ok(Value::Int(n))
        }
        FieldKind::Boolean => match v {
            Json::Bool(b) => Ok(Value::Bool(*b)),
            Json::String(s) => match s.trim().to_ascii_lowercase().as_str() {
                "true" | "yes" | "1" | "on" => Ok(Value::Bool(true)),
                "false" | "no" | "0" | "off" => Ok(Value::Bool(false)),
                _ => Err("must be true or false".into()),
            },
            _ => Err("must be true or false".into()),
        },
        FieldKind::Date => {
            let s = as_str()?.trim();
            let day = s.get(..10).unwrap_or(s);
            if !valid_date(day) || (s.len() > 10 && !s[10..].starts_with('T')) {
                return Err("must be a date like 2026-11-20".into());
            }
            Ok(Value::Date(day.to_string()))
        }
        FieldKind::Relation => match v {
            Json::Number(n) => n
                .as_i64()
                .map(|i| Value::Relation(RelRef::Id(i)))
                .ok_or_else(|| "must be an id or a slug".into()),
            Json::String(s) => {
                let s = s.trim();
                if let Ok(i) = s.parse::<i64>() {
                    Ok(Value::Relation(RelRef::Id(i)))
                } else if slug::is_valid(s) {
                    Ok(Value::Relation(RelRef::Slug(s.to_string())))
                } else {
                    Err("must be an id or a slug".into())
                }
            }
            _ => Err("must be an id or a slug".into()),
        },
        FieldKind::Terms => {
            let list: Vec<String> = match v {
                Json::Array(a) => a
                    .iter()
                    .map(|t| {
                        t.as_str()
                            .map(|s| s.trim().to_string())
                            .ok_or_else(|| "must be a list of term names".to_string())
                    })
                    .collect::<Result<_, _>>()?,
                Json::String(s) => s.split(',').map(|t| t.trim().to_string()).collect(),
                _ => return Err("must be a list of term names".into()),
            };
            let mut out: Vec<String> = Vec::new();
            for t in list.into_iter().filter(|t| !t.is_empty()) {
                if t.chars().count() > 100 {
                    return Err(format!("term `{t}` is longer than 100 characters"));
                }
                let s = slug::slugify(&t)
                    .ok_or_else(|| format!("term `{t}` needs at least one letter or digit"))?;
                if !out.iter().any(|o| slug::slugify(o).as_deref() == Some(&s)) {
                    out.push(t);
                }
            }
            Ok(Value::Terms(out))
        }
        FieldKind::Blocks => {
            let parsed;
            let v = match v {
                Json::String(s) => {
                    parsed = serde_json::from_str::<Json>(s)
                        .map_err(|e| format!("must be a JSON list of blocks: {e}"))?;
                    &parsed
                }
                other => other,
            };
            let Json::Array(items) = v else {
                return Err("must be a list of blocks".into());
            };
            // Stable ids: kept when valid and unique in the list, else (a
            // new block, a copy, a tree from before ids) the first free
            // `b<n>`. Deterministic, so validating twice changes nothing.
            let mut used: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            let mut ids: Vec<Option<String>> = Vec::with_capacity(items.len());
            for (i, item) in items.iter().enumerate() {
                let id = match item.get(BLOCK_ID) {
                    None | Some(Json::Null) => None,
                    Some(Json::String(s)) if valid_block_id(s) => Some(s.clone()),
                    Some(_) => {
                        return Err(format!(
                            "block {}: `id` must be 1 to 40 letters, digits, `_` or `-`",
                            i + 1
                        ));
                    }
                };
                ids.push(id.filter(|s| used.insert(s.clone())));
            }
            let mut next = 1usize;
            let mut fresh = || loop {
                let candidate = format!("b{next}");
                next += 1;
                if used.insert(candidate.clone()) {
                    return candidate;
                }
            };
            let ids: Vec<String> = ids
                .into_iter()
                .map(|id| id.unwrap_or_else(&mut fresh))
                .collect();
            let mut out = Vec::new();
            for (i, item) in items.iter().enumerate() {
                let at = |m: String| format!("block {}: {m}", i + 1);
                let Json::Object(obj) = item else {
                    return Err(at("must be a table with a `type`".into()));
                };
                let Some(bt) = obj.get("type").and_then(Json::as_str) else {
                    return Err(at("needs a `type`".into()));
                };
                let Some(block) = reg.block(bt) else {
                    return Err(at(format!("unknown block type `{bt}`")));
                };
                if !def.blocks.is_empty() && !def.blocks.iter().any(|b| b == bt) {
                    return Err(at(format!(
                        "`{bt}` is not allowed here (allowed: {})",
                        def.blocks.join(", ")
                    )));
                }
                if let Some(k) = obj.keys().find(|k| {
                    *k != "type" && *k != BLOCK_ID && !block.fields.iter().any(|f| &f.name == *k)
                }) {
                    return Err(at(format!("`{bt}` blocks have no field `{k}`")));
                }
                let mut clean = Map::new();
                clean.insert("type".into(), Json::String(bt.to_string()));
                clean.insert(BLOCK_ID.into(), Json::String(ids[i].clone()));
                for f in &block.fields {
                    let val = validate(reg, f, obj.get(&f.name).unwrap_or(&Json::Null))
                        .map_err(|m| at(format!("`{}` {m}", f.name)))?;
                    clean.insert(f.name.clone(), to_json(&val));
                }
                out.push(Json::Object(clean));
            }
            Ok(Value::Blocks(Json::Array(out)))
        }
    }
}

/// A validated value as JSON (revisions, block contents).
pub fn to_json(v: &Value) -> Json {
    match v {
        Value::Null => Json::Null,
        Value::Text(s) | Value::Date(s) => Json::String(s.clone()),
        Value::Int(i) => Json::from(*i),
        Value::Bool(b) => Json::Bool(*b),
        Value::Relation(RelRef::Id(i)) => Json::from(*i),
        Value::Relation(RelRef::Slug(s)) => Json::String(s.clone()),
        Value::Terms(t) => Json::Array(t.iter().map(|s| Json::String(s.clone())).collect()),
        Value::Blocks(j) => j.clone(),
    }
}

fn type_name(v: &Json) -> &'static str {
    match v {
        Json::Null => "nothing",
        Json::Bool(_) => "a boolean",
        Json::Number(_) => "a number",
        Json::String(_) => "text",
        Json::Array(_) => "a list",
        Json::Object(_) => "a table",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::tests::sample_site;
    use serde_json::json;

    fn reg() -> Registry {
        Registry::load(sample_site().path()).unwrap()
    }

    fn check(field: &str, v: Json) -> Result<Value, String> {
        let r = reg();
        let def = r.get("article").unwrap().field(field).unwrap().clone();
        validate(&r, &def, &v)
    }

    #[test]
    fn scalar_fields() {
        assert_eq!(check("title", json!("Hi")), Ok(Value::Text("Hi".into())));
        assert_eq!(check("title", json!("  ")).unwrap_err(), "is required");
        assert!(
            check("title", json!("x".repeat(121)))
                .unwrap_err()
                .contains("120")
        );
        assert_eq!(check("body", json!(null)), Ok(Value::Null));
        assert_eq!(check("rating", json!("4")), Ok(Value::Int(4)));
        assert!(check("rating", json!(9)).unwrap_err().contains("at most 5"));
        assert!(check("rating", json!("four")).is_err());
        assert_eq!(check("featured", json!("yes")), Ok(Value::Bool(true)));
        assert_eq!(
            check("published_on", json!("2026-02-28")),
            Ok(Value::Date("2026-02-28".into()))
        );
        assert!(check("published_on", json!("2026-02-30")).is_err());
        assert!(check("published_on", json!("2026-13-01")).is_err());
        assert!(check("link", json!("javascript:alert(1)")).is_err());
        assert!(check("link", json!("/about/")).is_ok());
        assert!(check("tone", json!("shouty")).unwrap_err().contains("calm"));
        assert_eq!(
            check("author", json!("ada")),
            Ok(Value::Relation(RelRef::Slug("ada".into())))
        );
        assert_eq!(
            check("author", json!(3)),
            Ok(Value::Relation(RelRef::Id(3)))
        );
        assert!(check("author", json!("../x")).is_err());
    }

    #[test]
    fn terms_and_blocks() {
        assert_eq!(
            check("topics", json!("Rust, web ,rust,")),
            Ok(Value::Terms(vec!["Rust".into(), "web".into()]))
        );
        assert!(check("topics", json!(["!!!"])).is_err());
        let ok = check(
            "sections",
            json!([{ "type": "quote", "text": "*Hi*", "cite": "Me" }]),
        )
        .unwrap();
        assert_eq!(
            ok,
            Value::Blocks(json!([{ "type": "quote", "id": "b1", "text": "*Hi*", "cite": "Me" }]))
        );
        for (bad, needle) in [
            (json!([{ "text": "x" }]), "needs a `type`"),
            (json!([{ "type": "hero" }]), "unknown block"),
            (json!([{ "type": "quote" }]), "`text` is required"),
            (
                json!([{ "type": "quote", "text": "x", "evil": 1 }]),
                "no field `evil`",
            ),
            (json!("not json"), "JSON"),
            (
                json!([{ "type": "quote", "text": "x", "id": "<!-- -->" }]),
                "`id` must be",
            ),
            (
                json!([{ "type": "quote", "text": "x", "id": 7 }]),
                "`id` must be",
            ),
        ] {
            let err = check("sections", bad).unwrap_err();
            assert!(err.contains(needle), "{err}");
        }
    }

    #[test]
    fn block_ids_are_kept_assigned_and_unique() {
        let quote = |id: Option<&str>| match id {
            Some(i) => json!({ "type": "quote", "text": "x", "id": i }),
            None => json!({ "type": "quote", "text": "x" }),
        };
        let ids = |v: Value| -> Vec<String> {
            let Value::Blocks(Json::Array(a)) = v else {
                panic!()
            };
            a.iter()
                .map(|b| b["id"].as_str().unwrap().to_string())
                .collect()
        };
        // Kept; missing ones get the first free `b<n>`; a duplicate (a
        // copied block) gets a new one; the first keeps its id.
        let got = ids(check(
            "sections",
            json!([
                quote(Some("k9")),
                quote(None),
                quote(Some("b1")),
                quote(Some("k9")),
                quote(None)
            ]),
        )
        .unwrap());
        assert_eq!(got, ["k9", "b2", "b1", "b3", "b4"]);
        // Validating again changes nothing.
        let again: Vec<Json> = got.iter().map(|i| quote(Some(i))).collect();
        assert_eq!(ids(check("sections", Json::Array(again)).unwrap()), got);
    }

    #[test]
    fn token_fields_take_only_token_names() {
        let r = reg();
        let b = r.block("panel").unwrap();
        let bg = b.fields.iter().find(|f| f.name == "background").unwrap();
        assert_eq!(
            validate(&r, bg, &json!("surface")),
            Ok(Value::Text("surface".into()))
        );
        for bad in [
            "#ff0000",
            "red",
            "var(--color-primary)",
            "text",
            "primary; x",
        ] {
            let e = validate(&r, bg, &json!(bad)).unwrap_err();
            assert!(e.contains("design tokens"), "{bad}: {e}");
        }
        // `options` narrows the group.
        let pad = b.fields.iter().find(|f| f.name == "padding").unwrap();
        assert!(validate(&r, pad, &json!("md")).is_ok());
        assert!(validate(&r, pad, &json!("xl")).is_err());
    }

    #[test]
    fn text_input_conversion() {
        let r = reg();
        let a = r.get("article").unwrap();
        assert_eq!(
            from_text(a.field("topics").unwrap(), "a, b").unwrap(),
            json!(["a", "b"])
        );
        assert_eq!(
            from_text(a.field("title").unwrap(), "").unwrap(),
            json!(null)
        );
        assert!(from_text(a.field("sections").unwrap(), "[").is_err());
        assert!(valid_date("2024-02-29") && !valid_date("2023-02-29"));
    }
}
