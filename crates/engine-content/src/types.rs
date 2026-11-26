//! Content types, declared in `types/*.toml` (one file per type, named after
//! it), and block types in `types/blocks/*.toml`.
//!
//! ```toml
//! # types/article.toml
//! label = "Article"
//! title_field = "title"      # default "title"
//! slug_from = "title"        # default: the title field
//!
//! [[fields]]
//! name = "title"
//! type = "text"
//! required = true
//! max_length = 200
//!
//! [[fields]]
//! name = "body"
//! type = "markdown"
//!
//! [[fields]]
//! name = "author"
//! type = "relation"
//! to = "author"
//!
//! [[fields]]
//! name = "topics"
//! type = "terms"
//! vocabulary = "topics"
//! ```
//!
//! Names (types, fields, blocks, vocabularies) are lowercase identifiers:
//! `[a-z][a-z0-9_]*`, at most 40 characters. They become table and column
//! names, so they are checked strictly.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

/// Field names every item has, which type files cannot reuse.
pub const RESERVED: &[&str] = &[
    "id",
    "slug",
    "status",
    "version",
    "created_at",
    "updated_at",
    "published_at",
    "url",
    "type",
    "kind",
    "date",
    "content_type",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum FieldKind {
    /// Plain text, auto-escaped in templates.
    Text,
    /// Markdown, rendered to sanitised HTML.
    Markdown,
    Integer,
    Boolean,
    /// A calendar date, `2026-11-20`.
    Date,
    /// An `http(s)://`, `mailto:` or site-relative (`/x`) link.
    Url,
    /// One of `options`.
    Select,
    /// The id of an item of type `to`.
    Relation,
    /// Taxonomy terms from `vocabulary`.
    Terms,
    /// An ordered list of typed blocks.
    Blocks,
}

impl FieldKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FieldKind::Text => "text",
            FieldKind::Markdown => "markdown",
            FieldKind::Integer => "integer",
            FieldKind::Boolean => "boolean",
            FieldKind::Date => "date",
            FieldKind::Url => "url",
            FieldKind::Select => "select",
            FieldKind::Relation => "relation",
            FieldKind::Terms => "terms",
            FieldKind::Blocks => "blocks",
        }
    }

    /// Whether values live in a column of the type's table.
    pub fn has_column(self) -> bool {
        self != FieldKind::Terms
    }

    fn allowed_in_block(self) -> bool {
        !matches!(
            self,
            FieldKind::Relation | FieldKind::Terms | FieldKind::Blocks
        )
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FieldDef {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: FieldKind,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub help: Option<String>,
    #[serde(default)]
    pub required: bool,
    /// Text, markdown and url: maximum length in characters.
    #[serde(default)]
    pub max_length: Option<usize>,
    /// Integer bounds.
    #[serde(default)]
    pub min: Option<i64>,
    #[serde(default)]
    pub max: Option<i64>,
    /// Select: allowed values.
    #[serde(default)]
    pub options: Vec<String>,
    /// Relation: target type.
    #[serde(default)]
    pub to: Option<String>,
    /// Terms: vocabulary name.
    #[serde(default)]
    pub vocabulary: Option<String>,
    /// Blocks: allowed block types (empty: all).
    #[serde(default)]
    pub blocks: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TypeFile {
    label: Option<String>,
    #[serde(default)]
    singleton: bool,
    title_field: Option<String>,
    slug_from: Option<String>,
    #[serde(default)]
    fields: Vec<FieldDef>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BlockFile {
    label: Option<String>,
    #[serde(default)]
    fields: Vec<FieldDef>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeDef {
    pub name: String,
    pub label: String,
    /// Exactly one item (site settings, a menu). Tagged `s:<name>`.
    pub singleton: bool,
    /// The text field used as the item's title, if any.
    pub title_field: Option<String>,
    /// The text field new slugs are made from.
    pub slug_from: Option<String>,
    pub fields: Vec<FieldDef>,
    pub source: PathBuf,
}

impl TypeDef {
    pub fn field(&self, name: &str) -> Option<&FieldDef> {
        self.fields.iter().find(|f| f.name == name)
    }

    /// Database table of this type.
    pub fn table(&self) -> String {
        table_name(&self.name)
    }

    /// Fields stored in columns, in declaration order.
    pub fn columns(&self) -> impl Iterator<Item = &FieldDef> {
        self.fields.iter().filter(|f| f.kind.has_column())
    }
}

/// Table name of a content type.
pub fn table_name(type_name: &str) -> String {
    format!("content_{type_name}")
}

#[derive(Debug, Clone, PartialEq)]
pub struct BlockDef {
    pub name: String,
    pub label: String,
    pub fields: Vec<FieldDef>,
}

/// All content types and blocks of a site.
#[derive(Debug, Clone, Default)]
pub struct Registry {
    types: BTreeMap<String, TypeDef>,
    blocks: BTreeMap<String, BlockDef>,
    vocabularies: BTreeSet<String>,
}

/// Whether `s` is a valid type/field/block/vocabulary name.
pub fn valid_name(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 40
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

fn toml_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    if dir.is_dir() {
        for e in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
            let p = e?.path();
            if p.is_file() && p.extension().is_some_and(|x| x == "toml") {
                out.push(p);
            }
        }
    }
    out.sort();
    Ok(out)
}

fn stem(p: &Path) -> Result<String> {
    let s = p
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("{}: file name is not UTF-8", p.display()))?;
    if !valid_name(s) {
        bail!(
            "{}: type and block files are named after the type, in lowercase letters, digits \
             and `_` (e.g. `blog_post.toml`)",
            p.display()
        );
    }
    Ok(s.to_string())
}

fn default_label(name: &str) -> String {
    let mut s = name.replace('_', " ");
    if let Some(f) = s.get(..1) {
        s.replace_range(..1, &f.to_uppercase());
    }
    s
}

impl Registry {
    /// Load `types/*.toml` and `types/blocks/*.toml` from a site folder. A
    /// site without `types/` has an empty registry.
    pub fn load(site_dir: &Path) -> Result<Self> {
        let dir = site_dir.join("types");
        let mut reg = Registry::default();
        for p in toml_files(&dir.join("blocks"))? {
            let name = stem(&p)?;
            let text = std::fs::read_to_string(&p)?;
            let f: BlockFile =
                toml::from_str(&text).with_context(|| format!("in {}", p.display()))?;
            reg.blocks.insert(
                name.clone(),
                BlockDef {
                    label: f.label.unwrap_or_else(|| default_label(&name)),
                    name,
                    fields: f.fields,
                },
            );
        }
        for p in toml_files(&dir)? {
            let name = stem(&p)?;
            let text = std::fs::read_to_string(&p)?;
            let f: TypeFile =
                toml::from_str(&text).with_context(|| format!("in {}", p.display()))?;
            let title_field = match f.title_field {
                Some(t) => Some(t),
                None if f.fields.iter().any(|x| x.name == "title") => Some("title".into()),
                None => None,
            };
            reg.types.insert(
                name.clone(),
                TypeDef {
                    label: f.label.unwrap_or_else(|| default_label(&name)),
                    singleton: f.singleton,
                    slug_from: f.slug_from.or_else(|| title_field.clone()),
                    title_field,
                    fields: f.fields,
                    source: p.strip_prefix(site_dir).unwrap_or(&p).to_path_buf(),
                    name,
                },
            );
        }
        reg.validate()?;
        Ok(reg)
    }

    /// Build a registry from definitions (tests, tools).
    pub fn from_defs(types: Vec<TypeDef>, blocks: Vec<BlockDef>) -> Result<Self> {
        let mut reg = Registry::default();
        for b in blocks {
            reg.blocks.insert(b.name.clone(), b);
        }
        for t in types {
            reg.types.insert(t.name.clone(), t);
        }
        reg.validate()?;
        Ok(reg)
    }

    fn validate(&mut self) -> Result<()> {
        for b in self.blocks.values() {
            let what = format!("block `{}`", b.name);
            check_fields(&what, &b.fields)?;
            if let Some(f) = b.fields.iter().find(|f| !f.kind.allowed_in_block()) {
                bail!(
                    "{what}: field `{}` is a {} field, which blocks cannot have",
                    f.name,
                    f.kind.as_str()
                );
            }
        }
        let mut vocabularies = BTreeSet::new();
        for t in self.types.values() {
            let what = format!("type `{}` ({})", t.name, t.source.display());
            check_fields(&what, &t.fields)?;
            for (key, val) in [("title_field", &t.title_field), ("slug_from", &t.slug_from)] {
                if let Some(name) = val {
                    match t.field(name) {
                        Some(f) if f.kind == FieldKind::Text => {}
                        Some(_) => bail!("{what}: `{key}` must name a text field"),
                        None => bail!("{what}: `{key}` names no field `{name}`"),
                    }
                }
            }
            if !t.singleton && t.slug_from.is_none() {
                bail!(
                    "{what}: needs a text field to make slugs from (a `title` field, or set \
                     `slug_from`)"
                );
            }
            let mut seen_vocab = BTreeSet::new();
            for f in &t.fields {
                match f.kind {
                    FieldKind::Relation => {
                        let to = f.to.as_deref().unwrap_or("");
                        match self.types.get(to) {
                            Some(target) if target.singleton => bail!(
                                "{what}: field `{}` relates to singleton `{to}`; use \
                                 `get_singleton(\"{to}\")` in templates instead",
                                f.name
                            ),
                            Some(_) => {}
                            None => bail!(
                                "{what}: field `{}` needs `to` naming another type (got `{to}`)",
                                f.name
                            ),
                        }
                    }
                    FieldKind::Terms => {
                        let v = f.vocabulary.as_deref().unwrap_or("");
                        if !valid_name(v) {
                            bail!(
                                "{what}: field `{}` needs `vocabulary` (a lowercase name)",
                                f.name
                            );
                        }
                        if !seen_vocab.insert(v) {
                            bail!("{what}: two fields use vocabulary `{v}`");
                        }
                        vocabularies.insert(v.to_string());
                    }
                    FieldKind::Blocks => {
                        for b in &f.blocks {
                            if !self.blocks.contains_key(b) {
                                bail!(
                                    "{what}: field `{}` allows block `{b}`, but there is no \
                                     types/blocks/{b}.toml",
                                    f.name
                                );
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        if let Some(v) = vocabularies.iter().find(|v| self.types.contains_key(*v)) {
            bail!(
                "vocabulary `{v}` has the same name as a content type; they share cache tags \
                 (`c:{v}`), so rename one"
            );
        }
        self.vocabularies = vocabularies;
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&TypeDef> {
        self.types.get(name)
    }

    pub fn require(&self, name: &str) -> Result<&TypeDef> {
        self.types.get(name).ok_or_else(|| {
            let known: Vec<&str> = self.types.keys().map(String::as_str).collect();
            anyhow!(
                "no content type `{name}` (known: {})",
                if known.is_empty() {
                    "none; add types/<name>.toml".to_string()
                } else {
                    known.join(", ")
                }
            )
        })
    }

    pub fn types(&self) -> impl Iterator<Item = &TypeDef> {
        self.types.values()
    }

    pub fn block(&self, name: &str) -> Option<&BlockDef> {
        self.blocks.get(name)
    }

    pub fn blocks(&self) -> impl Iterator<Item = &BlockDef> {
        self.blocks.values()
    }

    pub fn is_vocabulary(&self, name: &str) -> bool {
        self.vocabularies.contains(name)
    }

    pub fn vocabularies(&self) -> impl Iterator<Item = &str> {
        self.vocabularies.iter().map(String::as_str)
    }

    /// Types in an order where every relation target comes first.
    pub fn dependency_order(&self) -> Vec<&TypeDef> {
        let mut out: Vec<&TypeDef> = Vec::new();
        let mut done = BTreeSet::new();
        fn visit<'a>(
            reg: &'a Registry,
            t: &'a TypeDef,
            done: &mut BTreeSet<String>,
            out: &mut Vec<&'a TypeDef>,
            depth: usize,
        ) {
            if done.contains(&t.name) || depth > 64 {
                return;
            }
            done.insert(t.name.clone());
            for f in &t.fields {
                if let Some(target) = f.to.as_deref().and_then(|to| reg.get(to)) {
                    visit(reg, target, done, out, depth + 1);
                }
            }
            out.push(t);
        }
        for t in self.types.values() {
            visit(self, t, &mut done, &mut out, 0);
        }
        out
    }
}

fn check_fields(what: &str, fields: &[FieldDef]) -> Result<()> {
    let mut names = BTreeSet::new();
    for f in fields {
        if !valid_name(&f.name) {
            bail!(
                "{what}: field name `{}` must be lowercase letters, digits and `_`, starting \
                 with a letter (at most 40 characters)",
                f.name
            );
        }
        if RESERVED.contains(&f.name.as_str()) {
            bail!(
                "{what}: `{}` is a built-in property of every item; pick another field name",
                f.name
            );
        }
        if !names.insert(&f.name) {
            bail!("{what}: field `{}` is declared twice", f.name);
        }
        let k = f.kind;
        let fname = &f.name;
        if f.max_length.is_some()
            && !matches!(k, FieldKind::Text | FieldKind::Markdown | FieldKind::Url)
        {
            bail!("{what}: field `{fname}`: `max_length` only applies to text, markdown and url");
        }
        if (f.min.is_some() || f.max.is_some()) && k != FieldKind::Integer {
            bail!("{what}: field `{fname}`: `min`/`max` only apply to integer fields");
        }
        if k == FieldKind::Select {
            if f.options.is_empty() {
                bail!("{what}: select field `{fname}` needs `options`");
            }
        } else if !f.options.is_empty() {
            bail!("{what}: field `{fname}`: `options` only applies to select fields");
        }
        if f.to.is_some() && k != FieldKind::Relation {
            bail!("{what}: field `{fname}`: `to` only applies to relation fields");
        }
        if f.vocabulary.is_some() && k != FieldKind::Terms {
            bail!("{what}: field `{fname}`: `vocabulary` only applies to terms fields");
        }
        if !f.blocks.is_empty() && k != FieldKind::Blocks {
            bail!("{what}: field `{fname}`: `blocks` only applies to blocks fields");
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn write(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    pub const AUTHOR: &str = r#"
label = "Author"
title_field = "name"
[[fields]]
name = "name"
type = "text"
required = true
"#;

    pub const ARTICLE: &str = r#"
[[fields]]
name = "title"
type = "text"
required = true
max_length = 120
[[fields]]
name = "body"
type = "markdown"
[[fields]]
name = "rating"
type = "integer"
min = 1
max = 5
[[fields]]
name = "featured"
type = "boolean"
[[fields]]
name = "published_on"
type = "date"
[[fields]]
name = "link"
type = "url"
[[fields]]
name = "tone"
type = "select"
options = ["calm", "loud"]
[[fields]]
name = "author"
type = "relation"
to = "author"
[[fields]]
name = "topics"
type = "terms"
vocabulary = "topics"
[[fields]]
name = "sections"
type = "blocks"
blocks = ["quote"]
"#;

    pub const QUOTE: &str = r#"
[[fields]]
name = "text"
type = "markdown"
required = true
[[fields]]
name = "cite"
type = "text"
"#;

    pub const SETTINGS: &str = r#"
singleton = true
[[fields]]
name = "tagline"
type = "text"
"#;

    pub fn sample_site() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "types/author.toml", AUTHOR);
        write(d.path(), "types/article.toml", ARTICLE);
        write(d.path(), "types/settings.toml", SETTINGS);
        write(d.path(), "types/blocks/quote.toml", QUOTE);
        d
    }

    #[test]
    fn loads_types_blocks_and_vocabularies() {
        let d = sample_site();
        let reg = Registry::load(d.path()).unwrap();
        let a = reg.get("article").unwrap();
        assert_eq!(a.label, "Article");
        assert_eq!(a.title_field.as_deref(), Some("title"));
        assert_eq!(a.slug_from.as_deref(), Some("title"));
        assert_eq!(a.table(), "content_article");
        assert_eq!(a.columns().count(), 9, "terms have no column");
        assert_eq!(
            reg.get("author").unwrap().title_field.as_deref(),
            Some("name")
        );
        assert!(reg.get("settings").unwrap().singleton);
        assert!(reg.is_vocabulary("topics"));
        assert!(reg.block("quote").is_some());
        let order: Vec<_> = reg
            .dependency_order()
            .iter()
            .map(|t| t.name.as_str())
            .collect();
        let pos = |n| order.iter().position(|x| *x == n).unwrap();
        assert!(pos("author") < pos("article"), "{order:?}");
    }

    fn bad(rel: &str, text: &str) -> String {
        let d = sample_site();
        write(d.path(), rel, text);
        format!("{:#}", Registry::load(d.path()).unwrap_err())
    }

    #[test]
    fn rejects_bad_definitions() {
        let f = |body: &str| format!("[[fields]]\nname='title'\ntype='text'\n{body}");
        for (rel, text, needle) in [
            ("types/Bad.toml", f(""), "named after"),
            (
                "types/x.toml",
                f("[[fields]]\nname='id'\ntype='text'\n"),
                "built-in",
            ),
            (
                "types/x.toml",
                f("[[fields]]\nname='title'\ntype='text'\n"),
                "twice",
            ),
            (
                "types/x.toml",
                f("[[fields]]\nname='Bad'\ntype='text'\n"),
                "lowercase",
            ),
            (
                "types/x.toml",
                f("[[fields]]\nname='a'\ntype='colour'\n"),
                "unknown variant",
            ),
            (
                "types/x.toml",
                f("[[fields]]\nname='a'\ntype='select'\n"),
                "options",
            ),
            (
                "types/x.toml",
                f("[[fields]]\nname='a'\ntype='relation'\nto='nope'\n"),
                "`to`",
            ),
            (
                "types/x.toml",
                f("[[fields]]\nname='a'\ntype='relation'\nto='settings'\n"),
                "singleton",
            ),
            (
                "types/x.toml",
                f("[[fields]]\nname='a'\ntype='terms'\n"),
                "vocabulary",
            ),
            (
                "types/x.toml",
                f("[[fields]]\nname='a'\ntype='blocks'\nblocks=['nope']\n"),
                "types/blocks/nope.toml",
            ),
            (
                "types/x.toml",
                f("[[fields]]\nname='a'\ntype='integer'\nmax_length=3\n"),
                "max_length",
            ),
            (
                "types/x.toml",
                f("[[fields]]\nname='a'\ntype='terms'\nvocabulary='author'\n"),
                "same name",
            ),
            (
                "types/x.toml",
                "[[fields]]\nname='n'\ntype='integer'\n".into(),
                "slugs",
            ),
            ("types/x.toml", f("typo = 1\n"), "unknown field"),
            (
                "types/blocks/b.toml",
                "[[fields]]\nname='x'\ntype='terms'\nvocabulary='v'\n".into(),
                "blocks cannot",
            ),
        ] {
            let err = bad(rel, &text);
            assert!(err.contains(needle), "{rel} {text}: {err}");
        }
    }
}
