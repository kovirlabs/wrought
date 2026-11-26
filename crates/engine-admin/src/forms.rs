//! Edit forms generated from content types, and turning a submitted form
//! back into [`Changes`].
//!
//! Field names in the HTML are the type's field names. Blocks use
//! `<field>.<n>.type` and `<field>.<n>.<block field>`; a boolean sends a
//! hidden `false` before its checkbox so an unticked box still counts as
//! submitted. Engine values use a leading underscore: `_csrf`, `_version`,
//! `_slug`, `_status`, `_action`.
//!
//! Access is enforced here, not in the HTML: [`parse`] refuses a form that
//! carries any field the user may not write (a crafted POST), and only the
//! fields that were sent change.

use std::collections::BTreeMap;

use engine_auth::web::FormData;
use engine_auth::{Access, Action, FieldAccess};
use engine_content::store::{Changes, Item, Status};
use engine_content::types::{FieldDef, FieldKind, Registry, TypeDef};
use engine_content::value;
use serde::Serialize;
use serde_json::{Map, Value as Json};

/// One input as the templates draw it.
#[derive(Debug, Clone, Serialize)]
pub struct FieldView {
    /// HTML `name` (and, with `-` for `.`, its `id`).
    pub name: String,
    pub id: String,
    pub label: String,
    pub kind: &'static str,
    pub help: Option<String>,
    pub required: bool,
    /// `write` or `read`.
    pub access: &'static str,
    /// Text of the input.
    pub value: String,
    pub checked: bool,
    /// Select and relation choices: `(value, label)`.
    pub options: Vec<(String, String)>,
    pub max_length: Option<usize>,
    pub min: Option<i64>,
    pub max: Option<i64>,
    pub multiline: bool,
    pub error: Option<String>,
    /// Blocks fields: the blocks, and the block types that can be added.
    pub blocks: Vec<BlockView>,
    pub block_types: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BlockView {
    pub index: usize,
    pub block_type: String,
    pub label: String,
    pub fields: Vec<FieldView>,
    pub last: bool,
}

/// Key of the media library's choices in the relation options.
pub const MEDIA_OPTIONS: &str = "@media";

/// Field values while editing: what the inputs hold.
pub type Values = BTreeMap<String, Json>;

/// An item's values as input values (terms as names, relations as ids).
pub fn item_values(t: &TypeDef, item: &Item) -> Values {
    item.input_fields(t).into_iter().collect()
}

/// Text of a value for an input or a comparison.
pub fn display(kind: FieldKind, v: &Json) -> String {
    match (kind, v) {
        (_, Json::Null) => String::new(),
        (FieldKind::Terms, Json::Array(a)) => a
            .iter()
            .filter_map(|t| t.as_str().or_else(|| t.get("name").and_then(Json::as_str)))
            .collect::<Vec<_>>()
            .join(", "),
        (FieldKind::Blocks, v) => serde_json::to_string_pretty(v).unwrap_or_default(),
        (_, Json::String(s)) => s.replace("\r\n", "\n"),
        (_, other) => other.to_string(),
    }
}

fn truthy(v: &Json) -> bool {
    match v {
        Json::Bool(b) => *b,
        Json::String(s) => matches!(s.as_str(), "true" | "on" | "1" | "yes"),
        _ => false,
    }
}

pub(crate) fn label_of(f: &FieldDef) -> String {
    f.label.clone().unwrap_or_else(|| {
        let mut s = f.name.replace('_', " ");
        if let Some(c) = s.get(..1) {
            s.replace_range(..1, &c.to_uppercase());
        }
        s
    })
}

fn field_view(
    f: &FieldDef,
    name: String,
    access: FieldAccess,
    v: &Json,
    error: Option<String>,
    relation_options: &BTreeMap<String, Vec<(String, String)>>,
) -> FieldView {
    let mut options = Vec::new();
    match f.kind {
        FieldKind::Select => {
            options = f.options.iter().map(|o| (o.clone(), o.clone())).collect();
        }
        FieldKind::Relation => {
            options = relation_options
                .get(f.to.as_deref().unwrap_or(""))
                .cloned()
                .unwrap_or_default();
        }
        // The media library's images, when it is compiled in (else a
        // number input).
        FieldKind::Media => {
            options = relation_options
                .get(MEDIA_OPTIONS)
                .cloned()
                .unwrap_or_default();
            // Keep a value the list does not show (a PDF, or an old file).
            let cur = display(f.kind, v);
            if !options.is_empty() && !cur.is_empty() && !options.iter().any(|(id, _)| *id == cur) {
                options.push((cur.clone(), format!("#{cur}")));
            }
        }
        _ => {}
    }
    FieldView {
        id: format!("f-{}", name.replace('.', "-")),
        name,
        label: label_of(f),
        kind: f.kind.as_str(),
        help: f.help.clone(),
        required: f.required,
        access: if access == FieldAccess::Write {
            "write"
        } else {
            "read"
        },
        value: display(f.kind, v),
        checked: truthy(v),
        options,
        max_length: f.max_length,
        min: f.min,
        max: f.max,
        multiline: f.kind == FieldKind::Markdown
            || (f.kind == FieldKind::Text && f.max_length.is_none_or(|m| m > 300)),
        error,
        blocks: Vec::new(),
        block_types: Vec::new(),
    }
}

/// The inputs of a form for `t`, with `values` (an item's or a submitted
/// form's) and per-field `errors`. Hidden fields are left out entirely.
pub fn views(
    reg: &Registry,
    t: &TypeDef,
    access: &Access,
    action: Action,
    values: &Values,
    errors: &BTreeMap<String, String>,
    relation_options: &BTreeMap<String, Vec<(String, String)>>,
) -> Vec<FieldView> {
    let mut out = Vec::new();
    for f in &t.fields {
        let acc = access.field(&t.name, &f.name, action);
        if acc == FieldAccess::None {
            continue;
        }
        let v = values.get(&f.name).cloned().unwrap_or(Json::Null);
        let mut view = field_view(
            f,
            f.name.clone(),
            acc,
            &v,
            errors.get(&f.name).cloned(),
            relation_options,
        );
        if f.kind == FieldKind::Blocks {
            let list = v.as_array().cloned().unwrap_or_default();
            let n = list.len();
            for (i, b) in list.iter().enumerate() {
                let bt = b.get("type").and_then(Json::as_str).unwrap_or("");
                let Some(def) = reg.block(bt) else { continue };
                let fields = def
                    .fields
                    .iter()
                    .map(|bf| {
                        let name = format!("{}.{i}.{}", f.name, bf.name);
                        field_view(
                            bf,
                            name.clone(),
                            acc,
                            b.get(&bf.name).unwrap_or(&Json::Null),
                            errors.get(&name).cloned(),
                            relation_options,
                        )
                    })
                    .collect();
                view.blocks.push(BlockView {
                    index: i,
                    block_type: bt.to_string(),
                    label: def.label.clone(),
                    fields,
                    last: i + 1 == n,
                });
            }
            view.block_types = reg
                .blocks()
                .filter(|b| f.blocks.is_empty() || f.blocks.contains(&b.name))
                .map(|b| (b.name.clone(), b.label.clone()))
                .collect();
        }
        out.push(view);
    }
    out
}

/// Why a submitted form was refused.
#[derive(Debug)]
pub enum Refused {
    /// Fields the user may not write were sent.
    Forbidden(Vec<String>),
}

/// A parsed form: the changes to save, and the values to show again.
#[derive(Debug, Default)]
pub struct Parsed {
    pub changes: Changes,
    /// Values for re-rendering the form (all readable fields).
    pub values: Values,
    pub version: Option<i32>,
    /// A non-saving action (`add:<field>`, `up:<field>:<n>`, ...).
    pub action: Option<String>,
    /// Input errors found while parsing (block JSON and the like).
    pub errors: BTreeMap<String, String>,
}

fn sent(form: &FormData, field: &str) -> bool {
    let dotted = format!("{field}.");
    form.0
        .iter()
        .any(|(k, _)| k == field || k.starts_with(&dotted))
}

/// Blocks of `field` from a form, in index order.
fn parse_blocks(reg: &Registry, f: &FieldDef, form: &FormData) -> Vec<Json> {
    let prefix = format!("{}.", f.name);
    let mut by_index: BTreeMap<usize, Map<String, Json>> = BTreeMap::new();
    for (k, v) in &form.0 {
        let Some(rest) = k.strip_prefix(&prefix) else {
            continue;
        };
        let Some((idx, name)) = rest.split_once('.') else {
            continue;
        };
        let Ok(i) = idx.parse::<usize>() else {
            continue;
        };
        if i > 500 {
            continue;
        }
        by_index
            .entry(i)
            .or_default()
            .insert(name.to_string(), Json::String(v.clone()));
    }
    by_index
        .into_values()
        .filter_map(|mut m| {
            let bt = m.get("type")?.as_str()?.to_string();
            let def = reg.block(&bt)?;
            let mut out = Map::new();
            out.insert("type".into(), Json::String(bt));
            for bf in &def.fields {
                let raw = m.remove(&bf.name).unwrap_or(Json::Null);
                let v = match (bf.kind, raw) {
                    (FieldKind::Boolean, v) => Json::Bool(truthy(&v)),
                    (_, Json::Null) => continue,
                    (_, Json::String(s)) if s.trim().is_empty() => continue,
                    (_, v) => v,
                };
                out.insert(bf.name.clone(), v);
            }
            Some(Json::Object(out))
        })
        .collect()
}

/// Parse a submitted edit form for `t`. Refuses fields the user may not
/// write (for `action`, `Create` or `Edit`), and a status change without
/// the right to publish.
pub fn parse(
    reg: &Registry,
    t: &TypeDef,
    access: &Access,
    action: Action,
    form: &FormData,
    current: Option<&Values>,
) -> Result<Parsed, Refused> {
    let mut forbidden = Vec::new();
    // Anything that is not a field of the type or an engine value is noise
    // at best and a probe at worst.
    for (k, _) in &form.0 {
        let base = k.split('.').next().unwrap_or("");
        if !k.starts_with('_') && t.field(base).is_none() {
            forbidden.push(base.to_string());
        }
    }
    let mut p = Parsed::default();
    for f in &t.fields {
        let acc = access.field(&t.name, &f.name, action);
        let was_sent = sent(form, &f.name);
        if was_sent && acc != FieldAccess::Write {
            forbidden.push(f.name.clone());
            continue;
        }
        if acc == FieldAccess::None {
            continue;
        }
        if !was_sent {
            // Unchanged (read-only, or not on the form): show what is stored.
            if let Some(cur) = current.and_then(|c| c.get(&f.name)) {
                p.values.insert(f.name.clone(), cur.clone());
            }
            continue;
        }
        let input = match f.kind {
            FieldKind::Boolean => Json::Bool(form.all(&f.name).contains(&"true")),
            FieldKind::Blocks => Json::Array(parse_blocks(reg, f, form)),
            _ => {
                let raw = form.get(&f.name).unwrap_or("").replace("\r\n", "\n");
                match value::from_text(f, &raw) {
                    Ok(v) => v,
                    Err(e) => {
                        p.errors.insert(f.name.clone(), e);
                        Json::String(raw)
                    }
                }
            }
        };
        p.values.insert(f.name.clone(), input.clone());
        p.changes.fields.push((f.name.clone(), input));
    }
    if let Some(s) = form.get("_status") {
        let wanted = Status::parse(s);
        let unchanged = current
            .and_then(|c| c.get("_status"))
            .and_then(Json::as_str)
            .is_some_and(|c| c == s);
        if !access.can(&t.name, Action::Publish) && !unchanged {
            forbidden.push("status".into());
        } else if access.can(&t.name, Action::Publish) {
            p.changes.status = wanted;
        }
    }
    if let Some(s) = form.get("_slug").map(str::trim).filter(|s| !s.is_empty()) {
        p.changes.slug = Some(s.to_string());
    }
    if !forbidden.is_empty() {
        forbidden.sort();
        forbidden.dedup();
        return Err(Refused::Forbidden(forbidden));
    }
    p.version = form.get("_version").and_then(|v| v.trim().parse().ok());
    p.action = form
        .get("_action")
        .filter(|a| *a != "save")
        .map(str::to_string);
    Ok(p)
}

/// Apply a non-saving block action to the values. Returns whether it did
/// anything.
pub fn apply_block_action(
    reg: &Registry,
    t: &TypeDef,
    access: &Access,
    action: Action,
    p: &mut Parsed,
    form: &FormData,
) -> bool {
    let Some(a) = p.action.clone() else {
        return false;
    };
    let mut parts = a.split(':');
    let (op, field) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let Some(f) = t.field(field).filter(|f| f.kind == FieldKind::Blocks) else {
        return false;
    };
    if access.field(&t.name, field, action) != FieldAccess::Write {
        return false;
    }
    let mut list = p
        .values
        .get(field)
        .and_then(Json::as_array)
        .cloned()
        .unwrap_or_default();
    let idx: Option<usize> = parts.next().and_then(|i| i.parse().ok());
    match (op, idx) {
        ("add", _) => {
            let bt = form.get(&format!("{field}._add")).unwrap_or("");
            let allowed = f.blocks.is_empty() || f.blocks.iter().any(|b| b == bt);
            if reg.block(bt).is_none() || !allowed {
                return false;
            }
            list.push(serde_json::json!({ "type": bt }));
        }
        ("remove", Some(i)) if i < list.len() => {
            list.remove(i);
        }
        ("up", Some(i)) if i > 0 && i < list.len() => list.swap(i - 1, i),
        ("down", Some(i)) if i + 1 < list.len() => list.swap(i, i + 1),
        _ => return false,
    }
    p.values.insert(field.to_string(), Json::Array(list));
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_auth::Grants;

    fn reg() -> Registry {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("types/blocks")).unwrap();
        std::fs::write(
            d.path().join("types/article.toml"),
            r#"
[[fields]]
name = "title"
type = "text"
required = true
max_length = 100
[[fields]]
name = "author"
type = "text"
[[fields]]
name = "featured"
type = "boolean"
[[fields]]
name = "topics"
type = "terms"
vocabulary = "topics"
[[fields]]
name = "sections"
type = "blocks"
"#,
        )
        .unwrap();
        std::fs::write(
            d.path().join("types/blocks/quote.toml"),
            "[[fields]]\nname='text'\ntype='markdown'\nrequired=true\n[[fields]]\nname='cite'\ntype='text'\n",
        )
        .unwrap();
        Registry::load(d.path()).unwrap()
    }

    fn form(pairs: &[(&str, &str)]) -> FormData {
        FormData(
            pairs
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
        )
    }

    fn contributor() -> Access {
        let g: Grants = serde_json::from_str(
            r#"{"types": {"article": {"view": true, "edit": true, "create": true,
                 "fields": {"author": "read", "featured": "none"}}}}"#,
        )
        .unwrap();
        Access::from_roles([("contributor", false, &g)])
    }

    #[test]
    fn parses_fields_booleans_terms_and_blocks() {
        let r = reg();
        let t = r.get("article").unwrap();
        let p = parse(
            &r,
            t,
            &Access::full(),
            Action::Edit,
            &form(&[
                ("_csrf", "x"),
                ("_version", "3"),
                ("title", "Hi"),
                ("featured", "false"),
                ("featured", "true"),
                ("topics", "a, b"),
                ("sections.1.type", "quote"),
                ("sections.1.text", "second"),
                ("sections.0.type", "quote"),
                ("sections.0.text", "first"),
                ("sections.0.cite", ""),
                ("_status", "published"),
            ]),
            None,
        )
        .unwrap();
        assert_eq!(p.version, Some(3));
        let ch: BTreeMap<_, _> = p.changes.fields.iter().cloned().collect();
        assert_eq!(ch["title"], "Hi");
        assert_eq!(ch["featured"], true);
        assert_eq!(ch["topics"], serde_json::json!(["a", "b"]));
        assert_eq!(
            ch["sections"],
            serde_json::json!([{"type": "quote", "text": "first"}, {"type": "quote", "text": "second"}])
        );
        assert!(!ch.contains_key("author"), "not sent, not changed");
        assert_eq!(p.changes.status, Some(Status::Published));
    }

    #[test]
    fn crafted_posts_for_restricted_fields_are_refused() {
        let r = reg();
        let t = r.get("article").unwrap();
        let acc = contributor();
        for bad in [
            vec![("title", "x"), ("author", "me")],
            vec![("title", "x"), ("featured", "true")],
            vec![("title", "x"), ("_status", "published")],
            vec![("title", "x"), ("nonsense", "1")],
        ] {
            match parse(&r, t, &acc, Action::Edit, &form(&bad), None) {
                Err(Refused::Forbidden(f)) => assert!(!f.is_empty()),
                Ok(_) => panic!("accepted {bad:?}"),
            }
        }
        let ok = parse(&r, t, &acc, Action::Edit, &form(&[("title", "x")]), None).unwrap();
        assert_eq!(ok.changes.fields.len(), 1);
        // The form only shows what the user may see.
        let v = views(
            &r,
            t,
            &acc,
            Action::Edit,
            &Values::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
        );
        let names: Vec<_> = v.iter().map(|f| (f.name.as_str(), f.access)).collect();
        assert!(names.contains(&("author", "read")));
        assert!(!names.iter().any(|(n, _)| *n == "featured"));
    }

    #[test]
    fn block_actions() {
        let r = reg();
        let t = r.get("article").unwrap();
        let f = form(&[
            ("sections.0.type", "quote"),
            ("sections.0.text", "a"),
            ("sections.1.type", "quote"),
            ("sections.1.text", "b"),
            ("sections._add", "quote"),
            ("_action", "up:sections:1"),
        ]);
        let mut p = parse(&r, t, &Access::full(), Action::Edit, &f, None).unwrap();
        assert!(apply_block_action(
            &r,
            t,
            &Access::full(),
            Action::Edit,
            &mut p,
            &f
        ));
        assert_eq!(p.values["sections"][0]["text"], "b");
        p.action = Some("add:sections".into());
        assert!(apply_block_action(
            &r,
            t,
            &Access::full(),
            Action::Edit,
            &mut p,
            &f
        ));
        assert_eq!(p.values["sections"].as_array().unwrap().len(), 3);
        p.action = Some("remove:sections:0".into());
        assert!(apply_block_action(
            &r,
            t,
            &Access::full(),
            Action::Edit,
            &mut p,
            &f
        ));
        assert_eq!(p.values["sections"][0]["text"], "a");
    }
}
