//! The component catalogue: every component's manifest, its assets, and the
//! helpers templates use to load them.
//!
//! A built-in component is three files in a `components/` folder of a
//! template directory embedded in the binary:
//!
//! * `<name>.html`, the template (overridable from a site's
//!   `templates/components/`);
//! * `<name>.toml`, its manifest: version, group, the feature it needs, its
//!   arguments with types, its assets, editor hints and an example;
//! * optionally `<asset>.js` / `<asset>.css`, named by the manifest's
//!   `script` / `style`. Builds write them to
//!   `/_engine/components/<asset>-<hash>.<ext>` (the hash of the content, so
//!   they can be cached for ever), and a component template loads them with
//!   `{{ component_assets("<name>") }}` only when the page needs them. A page
//!   that uses a component twice gets one copy of each tag
//!   ([`dedup_assets`]).
//!
//! Templates whose name starts with `_` are partials shared by components,
//! not components. A site may add `templates/components/<name>.toml` next to
//! its own component to describe it the same way.

use std::collections::BTreeMap;
use std::path::Path;

use include_dir::Dir;
use minijinja::Value;
use serde::{Deserialize, Serialize};

/// URL folder of component assets.
pub const ASSET_PREFIX: &str = "/_engine/components/";

/// Component groups, in the spec's order.
pub const GROUPS: &[(&str, &str)] = &[
    ("layout", "Layout"),
    ("navigation", "Navigation"),
    ("content", "Content"),
    ("listings", "Listings"),
    ("forms", "Forms"),
    ("accounts", "Accounts"),
    ("app", "App UI"),
    ("commerce", "Commerce"),
];

/// Feature names a manifest may require (`core` is always compiled).
pub const FEATURES: &[&str] = &[
    "core", "request", "content", "search", "forms", "media", "auth", "payments",
];

/// Argument types a manifest may declare.
pub const ARG_TYPES: &[&str] = &[
    "text", "markdown", "html", "url", "integer", "number", "boolean", "date", "select", "list",
    "object", "media", "page", "any",
];

/// One argument of a component.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Arg {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<toml::Value>,
    /// `select`: the allowed values.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    /// `list` of plain values: the type of each entry (`html`, `text`...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<String>,
    /// `list` of objects and `object`: the fields of each entry.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<Arg>,
}

/// Hints for the visual editor (next milestone).
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EditorHints {
    /// A short icon name (`tabs`, `image`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// A preview thumbnail (URL), if the component has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<String>,
    /// Field layout: rows of argument names, shown side by side.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layout: Vec<Vec<String>>,
    /// Arguments that hold other blocks (layout components).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slots: Vec<String>,
    /// Not offered in the editor's block library (needs page context, or is
    /// a site frame part such as the header).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
}

/// A component manifest as written in `<name>.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestFile {
    label: String,
    version: String,
    group: String,
    features: Vec<String>,
    description: String,
    #[serde(default)]
    script: Option<String>,
    #[serde(default)]
    style: Option<String>,
    /// Page context the component reads besides its arguments (`page`,
    /// `listing`, `breadcrumbs`, `request`, `user`, `csrf`, `site`).
    #[serde(default)]
    reads: Vec<String>,
    #[serde(default)]
    args: Vec<Arg>,
    #[serde(default)]
    editor: EditorHints,
    #[serde(default)]
    example: Option<toml::Table>,
}

/// A component in the catalogue.
#[derive(Debug, Clone, Serialize)]
pub struct Component {
    pub name: String,
    pub label: String,
    pub version: String,
    pub group: String,
    pub features: Vec<String>,
    pub description: String,
    /// URL of the component's script module, if it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
    /// URL of the component's stylesheet, if it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reads: Vec<String>,
    pub args: Vec<Arg>,
    pub editor: EditorHints,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub example: Option<toml::Table>,
    /// `builtin` (in the binary) or `site` (only in the site's templates).
    pub source: &'static str,
    /// A built-in component the site replaces with its own template.
    pub overridden: bool,
    /// Template path, `components/<name>.html`.
    pub template: String,
}

impl Component {
    /// The example arguments as a template value; `html` arguments are
    /// marked safe (they come from the engine's own manifests).
    pub fn example_value(&self) -> Value {
        let Some(ex) = &self.example else {
            return Value::from(BTreeMap::<String, Value>::new());
        };
        let mut m = BTreeMap::new();
        for (k, v) in ex {
            let spec = self.args.iter().find(|a| &a.name == k);
            m.insert(k.clone(), typed_value(v, spec));
        }
        Value::from(m)
    }

    /// The `<link>` and `<script>` tags of the component's assets.
    pub fn asset_tags(&self) -> String {
        let mut out = String::new();
        if let Some(href) = &self.style {
            out.push_str(&format!("<link rel=\"stylesheet\" href=\"{href}\">"));
        }
        if let Some(src) = &self.script {
            out.push_str(&format!("<script type=\"module\" src=\"{src}\"></script>"));
        }
        out
    }
}

fn typed_value(v: &toml::Value, spec: Option<&Arg>) -> Value {
    let kind = spec.map(|a| a.kind.as_str()).unwrap_or("any");
    match (kind, v) {
        ("html", toml::Value::String(s)) => Value::from_safe_string(s.clone()),
        ("list", toml::Value::Array(items)) => {
            let fields = spec.map(|a| a.fields.as_slice()).unwrap_or_default();
            Value::from(
                items
                    .iter()
                    .map(|item| match item {
                        toml::Value::Table(t) if !fields.is_empty() => table_value(t, fields),
                        toml::Value::String(s)
                            if spec.and_then(|a| a.items.as_deref()) == Some("html") =>
                        {
                            Value::from_safe_string(s.clone())
                        }
                        other => Value::from_serialize(other),
                    })
                    .collect::<Vec<_>>(),
            )
        }
        ("object", toml::Value::Table(t)) => {
            table_value(t, spec.map(|a| a.fields.as_slice()).unwrap_or_default())
        }
        _ => Value::from_serialize(v),
    }
}

fn table_value(t: &toml::Table, fields: &[Arg]) -> Value {
    let mut m = BTreeMap::new();
    for (k, v) in t {
        m.insert(
            k.clone(),
            typed_value(v, fields.iter().find(|f| &f.name == k)),
        );
    }
    Value::from(m)
}

/// A file a build writes and the server serves under `/_engine/components/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetFile {
    pub path: String,
    pub body: &'static [u8],
}

/// Every component of this build (built-in, then the site's own), and the
/// asset files they use.
#[derive(Debug, Clone, Default)]
pub struct Catalogue {
    components: Vec<Component>,
    assets: Vec<AssetFile>,
}

/// FNV-1a, 64 bits: a stable content hash for asset file names.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 40
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// `MAJOR.MINOR.PATCH`.
pub fn valid_version(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.chars().all(|c| c.is_ascii_digit())
                && (p.len() == 1 || !p.starts_with('0'))
        })
}

fn check_args(what: &str, args: &[Arg]) -> Result<(), String> {
    let mut seen = std::collections::BTreeSet::new();
    for a in args {
        if !valid_name(&a.name) {
            return Err(format!("{what}: argument name `{}` is not valid", a.name));
        }
        if !seen.insert(a.name.as_str()) {
            return Err(format!("{what}: argument `{}` is declared twice", a.name));
        }
        if !ARG_TYPES.contains(&a.kind.as_str()) {
            return Err(format!(
                "{what}: argument `{}` has unknown type `{}` (known: {})",
                a.name,
                a.kind,
                ARG_TYPES.join(", ")
            ));
        }
        if let Some(t) = &a.items
            && (a.kind != "list" || !ARG_TYPES.contains(&t.as_str()))
        {
            return Err(format!(
                "{what}: argument `{}`: `items` is for lists, naming an argument type",
                a.name
            ));
        }
        if a.kind == "select" && a.options.is_empty() {
            return Err(format!(
                "{what}: select argument `{}` needs options",
                a.name
            ));
        }
        check_args(&format!("{what}, argument `{}`", a.name), &a.fields)?;
    }
    Ok(())
}

fn parse_manifest(what: &str, text: &str) -> Result<ManifestFile, String> {
    let m: ManifestFile = toml::from_str(text).map_err(|e| format!("{what}: {e}"))?;
    if !valid_version(&m.version) {
        return Err(format!(
            "{what}: version `{}` must be MAJOR.MINOR.PATCH",
            m.version
        ));
    }
    if !GROUPS.iter().any(|(g, _)| *g == m.group) {
        return Err(format!("{what}: unknown group `{}`", m.group));
    }
    if m.features.is_empty() {
        return Err(format!("{what}: `features` must name at least `core`"));
    }
    if let Some(f) = m.features.iter().find(|f| !FEATURES.contains(&f.as_str())) {
        return Err(format!("{what}: unknown feature `{f}`"));
    }
    for asset in [&m.script, &m.style].into_iter().flatten() {
        if !valid_name(asset) {
            return Err(format!("{what}: asset name `{asset}` is not valid"));
        }
    }
    check_args(what, &m.args)?;
    if let Some(ex) = &m.example
        && let Some(k) = ex.keys().find(|k| !m.args.iter().any(|a| &a.name == *k))
    {
        return Err(format!(
            "{what}: example sets `{k}`, which is not an argument"
        ));
    }
    Ok(m)
}

impl Catalogue {
    /// The catalogue of the given template folders (searched in order, as
    /// templates are: the first folder with a component wins), core's
    /// built-in folder(s) last, plus the components of a site's
    /// `templates/` folder.
    pub fn load(
        dirs: &[&'static Dir<'static>],
        site_templates: Option<&Path>,
    ) -> Result<Self, String> {
        let mut all: Vec<&'static Dir<'static>> = dirs.to_vec();
        for d in crate::templates::core_dirs() {
            if !all.iter().any(|x| std::ptr::eq(*x, d)) {
                all.push(d);
            }
        }
        let mut cat = Catalogue::default();
        let mut asset_urls: BTreeMap<(String, &'static str), String> = BTreeMap::new();
        for dir in &all {
            let Some(comp) = dir.get_dir("components") else {
                continue;
            };
            for f in comp.files() {
                let path = f.path();
                let (Some(stem), Some(ext)) = (
                    path.file_stem().and_then(|s| s.to_str()),
                    path.extension().and_then(|s| s.to_str()),
                ) else {
                    continue;
                };
                if (ext == "js" || ext == "css")
                    && !asset_urls
                        .contains_key(&(stem.to_string(), if ext == "js" { "js" } else { "css" }))
                {
                    let ext: &'static str = if ext == "js" { "js" } else { "css" };
                    let url = format!(
                        "{ASSET_PREFIX}{stem}-{:08x}.{ext}",
                        fnv1a(f.contents()) as u32
                    );
                    cat.assets.push(AssetFile {
                        path: url.clone(),
                        body: f.contents(),
                    });
                    asset_urls.insert((stem.to_string(), ext), url);
                }
            }
        }
        for dir in &all {
            let Some(comp) = dir.get_dir("components") else {
                continue;
            };
            let mut files: Vec<_> = comp.files().collect();
            files.sort_by_key(|f| f.path());
            for f in files {
                let path = f.path();
                if path.extension().and_then(|s| s.to_str()) != Some("html") {
                    continue;
                }
                let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                if name.starts_with('_') || cat.components.iter().any(|c| c.name == name) {
                    continue;
                }
                let what = format!("built-in component `{name}`");
                let text = comp
                    .get_file(path.with_extension("toml"))
                    .and_then(|m| m.contents_utf8())
                    .ok_or_else(|| format!("{what} has no manifest ({name}.toml)"))?;
                let m = parse_manifest(&what, text)?;
                let url =
                    |asset: &Option<String>, ext: &'static str| -> Result<Option<String>, String> {
                        match asset {
                            None => Ok(None),
                            Some(a) => asset_urls
                                .get(&(a.clone(), ext))
                                .cloned()
                                .map(Some)
                                .ok_or_else(|| format!("{what}: no asset file `{a}.{ext}`")),
                        }
                    };
                let overridden = site_templates
                    .is_some_and(|d| d.join("components").join(format!("{name}.html")).is_file());
                cat.components.push(Component {
                    script: url(&m.script, "js")?,
                    style: url(&m.style, "css")?,
                    name: name.to_string(),
                    label: m.label,
                    version: m.version,
                    group: m.group,
                    features: m.features,
                    description: m.description,
                    reads: m.reads,
                    args: m.args,
                    editor: m.editor,
                    example: m.example,
                    source: "builtin",
                    overridden,
                    template: format!("components/{name}.html"),
                });
            }
        }
        if let Some(site) = site_templates {
            cat.add_site_components(&site.join("components"))?;
        }
        cat.components.sort_by(|a, b| {
            let g = |c: &Component| GROUPS.iter().position(|(g, _)| *g == c.group);
            (g(a), &a.name).cmp(&(g(b), &b.name))
        });
        Ok(cat)
    }

    fn add_site_components(&mut self, dir: &Path) -> Result<(), String> {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return Ok(());
        };
        let mut names: Vec<String> = rd
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                (p.extension().is_some_and(|x| x == "html"))
                    .then(|| p.file_stem()?.to_str().map(str::to_string))
                    .flatten()
            })
            .filter(|n| !n.starts_with('_'))
            .collect();
        names.sort();
        for name in names {
            let manifest = dir.join(format!("{name}.toml"));
            let what = format!("templates/components/{name}.toml");
            let m = if manifest.is_file() {
                let text =
                    std::fs::read_to_string(&manifest).map_err(|e| format!("{what}: {e}"))?;
                Some(parse_manifest(&what, &text)?)
            } else {
                None
            };
            if let Some(c) = self.components.iter_mut().find(|c| c.name == name) {
                // A site override with its own manifest describes itself.
                if let Some(m) = m {
                    c.label = m.label;
                    c.version = m.version;
                    c.description = m.description;
                    c.args = m.args;
                    c.editor = m.editor;
                    c.example = m.example;
                }
                continue;
            }
            let m = m.unwrap_or_else(|| ManifestFile {
                label: name.replace('_', " "),
                version: "0.0.0".into(),
                group: "content".into(),
                features: vec!["core".into()],
                description: String::new(),
                script: None,
                style: None,
                reads: Vec::new(),
                args: Vec::new(),
                editor: EditorHints::default(),
                example: None,
            });
            self.components.push(Component {
                name: name.clone(),
                label: m.label,
                version: m.version,
                group: m.group,
                features: m.features,
                description: m.description,
                script: None,
                style: None,
                reads: m.reads,
                args: m.args,
                editor: m.editor,
                example: m.example,
                source: "site",
                overridden: false,
                template: format!("components/{name}.html"),
            });
        }
        Ok(())
    }

    pub fn components(&self) -> &[Component] {
        &self.components
    }

    pub fn get(&self, name: &str) -> Option<&Component> {
        self.components.iter().find(|c| c.name == name)
    }

    /// The asset files a build writes (every compiled component's, whether
    /// a page uses it or not: they are small, and pages decide what loads).
    pub fn assets(&self) -> &[AssetFile] {
        &self.assets
    }

    /// The catalogue as JSON (for `engine components --json` and the admin).
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "engine": env!("CARGO_PKG_VERSION"),
            "groups": GROUPS.iter().map(|(id, label)| serde_json::json!({"id": id, "label": label})).collect::<Vec<_>>(),
            "components": self.components,
        })
    }
}

/// Keep the first of identical component asset tags in a page and drop the
/// rest, so a component used several times loads its files once (browsers
/// would not run a module twice anyway; this keeps the HTML tidy).
pub fn dedup_assets(html: String) -> String {
    const STARTS: [&str; 2] = [
        "<script type=\"module\" src=\"/_engine/components/",
        "<link rel=\"stylesheet\" href=\"/_engine/components/",
    ];
    if !STARTS.iter().any(|s| html.contains(s)) {
        return html;
    }
    let mut out = String::with_capacity(html.len());
    let mut seen = std::collections::HashSet::new();
    let mut rest = html.as_str();
    loop {
        let next = STARTS
            .iter()
            .filter_map(|s| rest.find(s).map(|i| (i, *s)))
            .min_by_key(|(i, _)| *i);
        let Some((i, start)) = next else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..i]);
        let tag_rest = &rest[i..];
        let end = if start.starts_with("<script") {
            tag_rest.find("</script>").map(|e| e + "</script>".len())
        } else {
            tag_rest.find('>').map(|e| e + 1)
        };
        let Some(end) = end else {
            out.push_str(tag_rest);
            break;
        };
        let tag = &tag_rest[..end];
        if seen.insert(tag.to_string()) {
            out.push_str(tag);
        }
        rest = &tag_rest[end..];
    }
    out
}

/// A video address, understood: YouTube and Vimeo links become their
/// privacy-friendlier embed addresses; `.mp4`, `.webm` and `.ogv` files play
/// in a `<video>`; any other `https://` address is embedded as it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VideoSource {
    /// `youtube`, `vimeo`, `file` or `other`.
    pub provider: &'static str,
    /// Provider name for people ("YouTube").
    pub provider_name: &'static str,
    /// What goes in the iframe's or video's `src`.
    pub src: String,
    /// Where to watch it without an embed (the link of the no-script version).
    pub watch_url: String,
    /// Whether it plays in an `<iframe>` (else a `<video>`).
    pub iframe: bool,
}

fn url_ok(u: &str) -> bool {
    !u.chars()
        .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '"' | '\'' | '<' | '>' | '`'))
}

/// Understand a video address; `None` for anything unsafe or unknown
/// (only `https://` addresses and site paths are accepted).
pub fn video_source(url: &str) -> Option<VideoSource> {
    let u = url.trim();
    if u.is_empty() || !url_ok(u) {
        return None;
    }
    let lower = u.to_ascii_lowercase();
    let path_only = lower.split(['?', '#']).next().unwrap_or("");
    let is_file = [".mp4", ".webm", ".ogv"]
        .iter()
        .any(|e| path_only.ends_with(e));
    if (u.starts_with('/') && !u.starts_with("//")) || (lower.starts_with("https://") && is_file) {
        if !is_file {
            return None;
        }
        return Some(VideoSource {
            provider: "file",
            provider_name: "this site",
            src: u.to_string(),
            watch_url: u.to_string(),
            iframe: false,
        });
    }
    let rest = lower.strip_prefix("https://")?;
    let raw_rest = &u["https://".len()..];
    let (host, path) = raw_rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let host = host.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let id_ok = |id: &str| {
        !id.is_empty()
            && id.len() <= 64
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    };
    let query_param = |key: &str| -> Option<String> {
        let q = path.split_once('?')?.1.split('#').next()?;
        q.split('&')
            .find_map(|kv| kv.strip_prefix(&format!("{key}=")).map(str::to_string))
    };
    let youtube = |id: String| VideoSource {
        provider: "youtube",
        provider_name: "YouTube",
        src: format!("https://www.youtube-nocookie.com/embed/{id}"),
        watch_url: format!("https://www.youtube.com/watch?v={id}"),
        iframe: true,
    };
    match host {
        "youtube.com" | "m.youtube.com" | "youtube-nocookie.com" => {
            let p = path.split(['?', '#']).next().unwrap_or("");
            let id = if p == "/watch" {
                query_param("v")
            } else {
                ["/embed/", "/shorts/", "/live/"]
                    .iter()
                    .find_map(|pre| p.strip_prefix(pre).map(str::to_string))
            }?;
            id_ok(&id).then(|| youtube(id))
        }
        "youtu.be" => {
            let id = path
                .trim_start_matches('/')
                .split(['?', '#', '/'])
                .next()?
                .to_string();
            id_ok(&id).then(|| youtube(id))
        }
        "vimeo.com" | "player.vimeo.com" => {
            let id = path
                .split(['?', '#'])
                .next()?
                .rsplit('/')
                .find(|s| !s.is_empty())?
                .to_string();
            (id.chars().all(|c| c.is_ascii_digit()) && !id.is_empty()).then(|| VideoSource {
                provider: "vimeo",
                provider_name: "Vimeo",
                src: format!("https://player.vimeo.com/video/{id}?dnt=1"),
                watch_url: format!("https://vimeo.com/{id}"),
                iframe: true,
            })
        }
        _ => Some(VideoSource {
            provider: "other",
            provider_name: "the video's site",
            src: u.to_string(),
            watch_url: u.to_string(),
            iframe: true,
        }),
    }
}

/// Markdown written by an editor or a visitor, as safe HTML: raw HTML is
/// shown as text, and links and images only keep `http(s)`, `mailto`,
/// site-relative and `#fragment` addresses. For the `rich_text` component's
/// `markdown` argument (the `markdown` filter); developer-written Markdown
/// files keep their raw HTML (see [`crate::content::render_markdown`]).
pub fn safe_markdown(md: &str) -> String {
    use pulldown_cmark::{CowStr, Event, Options, Parser, Tag};
    let opts = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_SMART_PUNCTUATION;
    let safe_url = |u: &str| {
        let l = u.trim().to_ascii_lowercase();
        l.starts_with("https://")
            || l.starts_with("http://")
            || l.starts_with("mailto:")
            || l.starts_with('#')
            || (l.starts_with('/') && !l.starts_with("//"))
            || (!l.contains(':') && !l.starts_with("//"))
    };
    let events = Parser::new_ext(md, opts).map(|e| match e {
        Event::Html(h) | Event::InlineHtml(h) => Event::Text(h),
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) if !safe_url(&dest_url) => Event::Start(Tag::Link {
            link_type,
            dest_url: CowStr::from(""),
            title,
            id,
        }),
        Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) if !safe_url(&dest_url) => Event::Start(Tag::Image {
            link_type,
            dest_url: CowStr::from(""),
            title,
            id,
        }),
        other => other,
    });
    let mut out = String::new();
    pulldown_cmark::html::push_html(&mut out, events);
    crate::content::focusable_code_blocks(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_component_has_a_valid_manifest() {
        let cat = Catalogue::load(&[], None).unwrap();
        assert!(cat.components().len() >= 30, "{}", cat.components().len());
        for c in cat.components() {
            assert!(valid_version(&c.version), "{}", c.name);
            assert_eq!(c.source, "builtin");
            assert!(!c.description.is_empty(), "{} needs a description", c.name);
            assert!(c.example.is_some(), "{} needs an example", c.name);
        }
        // Every template in the components folders is a component or a
        // partial, and every manifest has a template.
        for d in crate::templates::core_dirs() {
            let comp = d.get_dir("components").unwrap();
            for f in comp.files() {
                let p = f.path();
                let stem = p.file_stem().unwrap().to_str().unwrap();
                match p.extension().and_then(|e| e.to_str()) {
                    Some("toml") => assert!(
                        comp.get_file(p.with_extension("html")).is_some(),
                        "{} has no template",
                        p.display()
                    ),
                    Some("html") if !stem.starts_with('_') => {
                        assert!(cat.get(stem).is_some(), "{stem}")
                    }
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn core_has_the_spec_layout_navigation_and_content_groups() {
        let cat = Catalogue::load(&[], None).unwrap();
        for name in [
            "section",
            "columns",
            "grid",
            "stack",
            "container",
            "spacer",
            "divider",
            "header",
            "menu",
            "mobile_menu",
            "breadcrumbs",
            "footer",
            "pagination",
            "tabs",
            "hero",
            "rich_text",
            "image",
            "gallery",
            "video_embed",
            "quote",
            "card",
            "accordion",
            "cta",
            "timeline",
        ] {
            let c = cat.get(name).unwrap_or_else(|| panic!("{name} missing"));
            assert_eq!(c.features, ["core"], "{name}");
        }
        assert_eq!(cat.get("tabs").unwrap().group, "navigation");
        assert_eq!(cat.get("stack").unwrap().group, "layout");
    }

    #[test]
    fn versions_are_semver() {
        for ok in ["1.0.0", "0.1.12", "10.0.3"] {
            assert!(valid_version(ok), "{ok}");
        }
        for bad in ["1.0", "1.0.0-beta", "01.0.0", "a.b.c", ""] {
            assert!(!valid_version(bad), "{bad}");
        }
    }

    #[test]
    fn asset_names_carry_a_content_hash() {
        let cat = Catalogue::load(&[], None).unwrap();
        let v = cat.get("video_embed").unwrap();
        let src = v.script.as_deref().unwrap();
        assert!(src.starts_with("/_engine/components/video_embed-"), "{src}");
        assert!(src.ends_with(".js"));
        assert!(cat.assets().iter().any(|a| a.path == src));
        assert_eq!(
            v.asset_tags(),
            format!("<script type=\"module\" src=\"{src}\"></script>")
        );
        let s = cat.get("stack").unwrap().style.clone().unwrap();
        assert!(s.starts_with("/_engine/components/layout-") && s.ends_with(".css"));
    }

    #[test]
    fn bad_manifests_are_refused() {
        let base =
            "label='x'\nversion='1.0.0'\ngroup='content'\nfeatures=['core']\ndescription='d'\n";
        assert!(parse_manifest("x", base).is_ok());
        for (bad, why) in [
            (base.replace("1.0.0", "1.0"), "version"),
            (base.replace("'content'", "'misc'"), "group"),
            (base.replace("['core']", "['nope']"), "feature"),
            (format!("{base}[[args]]\nname='a'\ntype='blob'\n"), "type"),
            (
                format!("{base}[[args]]\nname='a'\ntype='select'\n"),
                "options",
            ),
            (format!("{base}[example]\nzzz=1\n"), "not an argument"),
            (format!("{base}colour='red'\n"), "unknown field"),
        ] {
            let e = parse_manifest("x", &bad).unwrap_err();
            assert!(e.contains(why), "{why}: {e}");
        }
    }

    #[test]
    fn site_components_and_overrides_are_listed() {
        let d = tempfile::tempdir().unwrap();
        let c = d.path().join("components");
        std::fs::create_dir_all(&c).unwrap();
        std::fs::write(c.join("card.html"), "<p>mine</p>").unwrap();
        std::fs::write(c.join("promo.html"), "<p>promo</p>").unwrap();
        std::fs::write(
            c.join("promo.toml"),
            "label='Promo'\nversion='2.1.0'\ngroup='content'\nfeatures=['core']\ndescription='A promo.'\n[editor]\nicon='star'\n",
        )
        .unwrap();
        std::fs::write(c.join("_partial.html"), "x").unwrap();
        let cat = Catalogue::load(&[], Some(d.path())).unwrap();
        assert!(cat.get("card").unwrap().overridden);
        assert!(!cat.get("quote").unwrap().overridden);
        let p = cat.get("promo").unwrap();
        assert_eq!((p.source, p.version.as_str()), ("site", "2.1.0"));
        assert_eq!(p.editor.icon.as_deref(), Some("star"));
        assert!(cat.get("_partial").is_none());
        std::fs::write(c.join("promo.toml"), "label='Promo'").unwrap();
        assert!(Catalogue::load(&[], Some(d.path())).is_err());
    }

    #[test]
    fn duplicate_asset_tags_are_dropped() {
        let s = "<script type=\"module\" src=\"/_engine/components/a-1.js\"></script>";
        let l = "<link rel=\"stylesheet\" href=\"/_engine/components/l-1.css\">";
        let html = format!("<p>{s}{l}x{s}<i>{l}</i>{s}</p><script src=\"/other.js\"></script>");
        assert_eq!(
            dedup_assets(html),
            format!("<p>{s}{l}x<i></i></p><script src=\"/other.js\"></script>")
        );
        assert_eq!(dedup_assets("plain".into()), "plain");
    }

    #[test]
    fn video_sources() {
        let y = video_source("https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=1").unwrap();
        assert_eq!(y.src, "https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ");
        assert_eq!(y.watch_url, "https://www.youtube.com/watch?v=dQw4w9WgXcQ");
        assert_eq!(
            video_source("https://youtu.be/dQw4w9WgXcQ").unwrap().src,
            y.src
        );
        let v = video_source("https://vimeo.com/76979871").unwrap();
        assert_eq!(v.src, "https://player.vimeo.com/video/76979871?dnt=1");
        let f = video_source("/media/tour.mp4").unwrap();
        assert!(!f.iframe);
        assert_eq!(
            video_source("https://example.org/embed/1")
                .unwrap()
                .provider,
            "other"
        );
        for bad in [
            "javascript:alert(1)",
            "http://youtube.com/watch?v=x",
            "//evil.example/x",
            "https://youtube.com/watch?v=a\"b",
            "https://www.youtube.com/watch?v=<x>",
            "/page/",
            "",
        ] {
            assert!(video_source(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn safe_markdown_escapes_html_and_bad_links() {
        let h = safe_markdown(
            "Hi <script>alert(1)</script> *there* [a](javascript:alert(1)) [b](/ok/) ![i](data:x)",
        );
        assert!(!h.contains("<script"), "{h}");
        assert!(h.contains("&lt;script&gt;"), "{h}");
        assert!(h.contains("<em>there</em>"), "{h}");
        assert!(h.contains("<a href=\"\">a</a>"), "{h}");
        assert!(h.contains("<a href=\"/ok/\">b</a>"), "{h}");
        assert!(!h.contains("data:"), "{h}");
    }
}
