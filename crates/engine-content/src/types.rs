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
    /// A file of the media library (`media` feature), by number; templates
    /// show it with `media(page.<field>)`.
    Media,
    /// A design choice: the name of a design token from group `token`
    /// (`color`, `background`, `font`, `spacing`; see
    /// `engine_core::ui::TOKEN_CHOICES`), never a CSS value. Templates print
    /// it in the group's data attribute (`data-background="{{ block.bg }}"`).
    Token,
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
            FieldKind::Media => "media",
            FieldKind::Token => "token",
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
    /// Token: the token group to pick from (`color`, `background`, `font`,
    /// `spacing`). `options` may narrow it to some of the group's names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// The value a new block starts with (blocks derived from a component:
    /// the argument's default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
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
    /// One line for the editor's block library.
    #[serde(default)]
    description: Option<String>,
    /// Render the block with this component (its fields are the
    /// component's arguments) when the site has no `blocks/<name>.html`.
    /// Without `[[fields]]`, the fields are taken from the component's
    /// manifest (every argument a block field can hold).
    #[serde(default)]
    component: Option<String>,
    #[serde(default)]
    fields: Vec<FieldDef>,
    /// Hints for the visual editor (icon, thumbnail, field layout); for a
    /// component block, they override the manifest's.
    #[serde(default)]
    editor: Option<engine_core::components::EditorHints>,
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
    /// A component that renders the block (see [`BlockFile`]).
    pub component: Option<String>,
    /// One line for the editor's block library.
    pub description: Option<String>,
    /// Editor hints from the block type file (the component manifest's
    /// apply where these say nothing).
    pub editor: Option<engine_core::components::EditorHints>,
}

impl BlockDef {
    /// A block type with just fields (tests, tools).
    pub fn new(name: &str, fields: Vec<FieldDef>) -> Self {
        BlockDef {
            name: name.to_string(),
            label: default_label(name),
            fields,
            component: None,
            description: None,
            editor: None,
        }
    }
}

/// The key of a block's stable id in a block tree (`{"type": "quote",
/// "id": "b3", ...}`). `id` is a reserved name, so no block field uses it.
pub const BLOCK_ID: &str = "id";

/// Whether `s` is a valid block id: 1 to 40 ASCII letters, digits, `_` or
/// `-`, so it is safe in an HTML attribute or comment as is.
pub fn valid_block_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 40
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
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

/// A block rendered by a component: the component must exist (built in, or
/// the site's own `templates/components/<name>.html`), and for a built-in
/// one the site does not override, every field must be one of its
/// arguments. A component block without `[[fields]]` gets them from the
/// component's manifest ([`fields_from_manifest`]).
fn check_block_component(site_dir: &Path, b: &mut BlockDef, c: &str) -> Result<()> {
    let what = format!("block `{}` (types/blocks/{}.toml)", b.name, b.name);
    if !valid_name(c) {
        bail!("{what}: `component = \"{c}\"` is not a component name");
    }
    let templates = site_dir.join("templates");
    let site_has = templates
        .join("components")
        .join(format!("{c}.html"))
        .is_file();
    let cat = engine_core::components::Catalogue::load(
        &[&crate::render::LISTINGS],
        Some(&templates)
            .filter(|t| t.is_dir())
            .map(PathBuf::as_path),
    )
    .map_err(|e| anyhow!(e))?;
    let comp = cat.get(c);
    if b.fields.is_empty() {
        let Some(comp) = comp else {
            if !site_has {
                bail!(
                    "{what}: there is no component `{c}` (see `engine components`, or add \
                     templates/components/{c}.html)"
                );
            }
            bail!(
                "{what}: has no [[fields]], and component `{c}` has no manifest to take them \
                 from (add [[fields]], or templates/components/{c}.toml)"
            );
        };
        b.fields = fields_from_manifest(&what, comp)?;
    }
    if site_has {
        return Ok(());
    }
    let Some(comp) = comp else {
        bail!(
            "{what}: there is no component `{c}` (see `engine components`, or add \
             templates/components/{c}.html)"
        );
    };
    for f in &b.fields {
        if !comp.args.iter().any(|a| a.name == f.name) {
            let args: Vec<&str> = comp.args.iter().map(|a| a.name.as_str()).collect();
            bail!(
                "{what}: field `{}` is not an argument of component `{c}` (arguments: {})",
                f.name,
                args.join(", ")
            );
        }
    }
    Ok(())
}

/// Block fields for a component's arguments: every argument whose type a
/// block field can hold (`text`, `markdown`, `url`, `integer`, `boolean`,
/// `date`, `select`, `media`), with its label, help, options and default.
/// Arguments a block cannot hold (`html`, lists, objects, page context) or
/// whose name is reserved (`id`, `url`, ...) are left out; a required one
/// is an error, since the component could not render without it.
pub fn fields_from_manifest(
    what: &str,
    comp: &engine_core::components::Component,
) -> Result<Vec<FieldDef>> {
    let mut out = Vec::new();
    for a in &comp.args {
        let kind = match a.kind.as_str() {
            "text" => Some(FieldKind::Text),
            "markdown" => Some(FieldKind::Markdown),
            "url" => Some(FieldKind::Url),
            "integer" => Some(FieldKind::Integer),
            "boolean" => Some(FieldKind::Boolean),
            "date" => Some(FieldKind::Date),
            "select" => Some(FieldKind::Select),
            "media" => Some(FieldKind::Media),
            _ => None,
        };
        let usable = kind.filter(|_| valid_name(&a.name) && !RESERVED.contains(&a.name.as_str()));
        let Some(kind) = usable else {
            if a.required {
                bail!(
                    "{what}: component `{}` needs argument `{}` ({}), which a block field \
                     cannot hold; write the block's [[fields]] and a blocks/{}.html template",
                    comp.name,
                    a.name,
                    a.kind,
                    comp.name
                );
            }
            continue;
        };
        out.push(FieldDef {
            name: a.name.clone(),
            kind,
            label: a.label.clone(),
            help: a.help.clone(),
            required: a.required,
            max_length: None,
            min: None,
            max: None,
            options: a.options.clone(),
            to: None,
            vocabulary: None,
            blocks: Vec::new(),
            token: None,
            default: a.default.as_ref().map(crate::value::from_toml),
        });
    }
    Ok(out)
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
                    component: f.component,
                    description: f.description,
                    editor: f.editor,
                },
            );
        }
        for b in reg.blocks.values_mut() {
            if let Some(c) = b.component.clone() {
                check_block_component(site_dir, b, &c)?;
            }
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
        for b in self.blocks.values() {
            for f in &b.fields {
                if let Some(d) = &f.default
                    && let Err(m) = crate::value::validate(self, f, d)
                {
                    bail!("block `{}`: the default of field `{}` {m}", b.name, f.name);
                }
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
        } else if k == FieldKind::Token {
            let groups: Vec<&str> = engine_core::ui::TOKEN_CHOICES
                .iter()
                .map(|(g, _)| *g)
                .collect();
            let Some(names) = f.token.as_deref().and_then(engine_core::ui::token_choices) else {
                bail!(
                    "{what}: token field `{fname}` needs `token`, one of: {}",
                    groups.join(", ")
                );
            };
            if let Some(o) = f.options.iter().find(|o| !names.contains(&o.as_str())) {
                bail!(
                    "{what}: token field `{fname}`: option `{o}` is not a `{}` token (choices: {})",
                    f.token.as_deref().unwrap_or(""),
                    names.join(", ")
                );
            }
        } else if !f.options.is_empty() {
            bail!("{what}: field `{fname}`: `options` only applies to select and token fields");
        }
        if f.token.is_some() && k != FieldKind::Token {
            bail!("{what}: field `{fname}`: `token` only applies to token fields");
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

    pub const PANEL: &str = r#"
label = "Panel"
description = "A coloured box."
[editor]
icon = "panel"
[[fields]]
name = "title"
type = "text"
[[fields]]
name = "background"
type = "token"
token = "background"
[[fields]]
name = "padding"
type = "token"
token = "spacing"
options = ["sm", "md", "lg"]
default = "md"
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
        write(d.path(), "types/blocks/panel.toml", PANEL);
        d
    }

    #[test]
    fn component_blocks_take_fields_and_hints_from_the_manifest() {
        let d = sample_site();
        write(
            d.path(),
            "types/blocks/banner.toml",
            "component = \"hero\"\n",
        );
        write(
            d.path(),
            "types/blocks/box.toml",
            "component = \"section\"\n[editor]\nicon = \"box\"\n",
        );
        let reg = Registry::load(d.path()).unwrap();
        let b = reg.block("banner").unwrap();
        let names: Vec<&str> = b.fields.iter().map(|f| f.name.as_str()).collect();
        // `actions` (a list) cannot be a block field; it is optional.
        assert_eq!(names, ["title", "text", "label", "level"]);
        assert!(b.fields[0].required);
        assert_eq!(b.fields[3].default, Some(serde_json::json!(1)));
        // `id` is reserved and `body` is HTML: both left out.
        let s = reg.block("box").unwrap();
        assert!(s.fields.iter().all(|f| f.name != "id" && f.name != "body"));
        assert_eq!(
            s.fields.iter().find(|f| f.name == "theme").unwrap().kind,
            FieldKind::Select
        );
        assert_eq!(s.editor.as_ref().unwrap().icon.as_deref(), Some("box"));
        let p = reg.block("panel").unwrap();
        assert_eq!(p.description.as_deref(), Some("A coloured box."));
        assert_eq!(p.fields[1].kind, FieldKind::Token);
    }

    #[test]
    fn blocks_can_name_a_component() {
        let d = sample_site();
        write(
            d.path(),
            "types/blocks/pull.toml",
            "component = \"quote\"\n[[fields]]\nname = \"text\"\ntype = \"text\"\n[[fields]]\nname = \"cite\"\ntype = \"text\"\n",
        );
        let reg = Registry::load(d.path()).unwrap();
        assert_eq!(
            reg.block("pull").unwrap().component.as_deref(),
            Some("quote")
        );
        write(
            d.path(),
            "types/blocks/pull.toml",
            "component = \"quote\"\n[[fields]]\nname = \"colour\"\ntype = \"text\"\n",
        );
        let e = Registry::load(d.path()).unwrap_err().to_string();
        assert!(e.contains("not an argument of component `quote`"), "{e}");
        write(d.path(), "types/blocks/pull.toml", "component = \"nope\"\n");
        let e = Registry::load(d.path()).unwrap_err().to_string();
        assert!(e.contains("no component `nope`"), "{e}");
        // The site's own component: any fields.
        write(d.path(), "templates/components/nope.html", "x");
        write(
            d.path(),
            "types/blocks/pull.toml",
            "component = \"nope\"\n[[fields]]\nname = \"colour\"\ntype = \"text\"\n",
        );
        assert!(Registry::load(d.path()).is_ok());
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
            (
                "types/blocks/t.toml",
                "[[fields]]\nname='c'\ntype='token'\n".to_string(),
                "needs `token`",
            ),
            (
                "types/blocks/t.toml",
                "[[fields]]\nname='c'\ntype='token'\ntoken='colour'\n".to_string(),
                "needs `token`",
            ),
            (
                "types/blocks/t.toml",
                "[[fields]]\nname='c'\ntype='token'\ntoken='color'\noptions=['#fff']\n".to_string(),
                "not a `color` token",
            ),
            (
                "types/blocks/t.toml",
                "[[fields]]\nname='c'\ntype='text'\ntoken='color'\n".to_string(),
                "only applies to token",
            ),
            (
                "types/blocks/t.toml",
                "[[fields]]\nname='c'\ntype='token'\ntoken='color'\ndefault='red'\n".to_string(),
                "default of field `c`",
            ),
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
