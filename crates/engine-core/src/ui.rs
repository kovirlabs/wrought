//! `[ui]` providers and design tokens.
//!
//! Styling and behaviour are providers. The defaults are the Kovir Labs Tag
//! stack, bundled in the binary and served from the site's own origin under
//! `/_engine/`:
//!
//! * `css = "tagbrush"` — TagBrush.css, classless CSS for semantic tags.
//! * `behaviour = "tagaction"` — TagAction, behaviour for semantic tags.
//!
//! Either can be `"none"`, a site path (`"/css/site.css"`, served from the
//! site's `static/` folder) or an `https://` URL.
//!
//! Design tokens (`[ui.tokens]`) use Hammerwright's names and are mapped onto
//! TagBrush's custom properties (see [`TOKEN_MAP`]); `--raw-name` keys pass
//! through unchanged. A `[light, dark]` pair becomes `light-dark(a, b)`.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use crate::config::{ConfigError, TokenValue, UiConfig};

pub const TAGBRUSH_VERSION: &str = "2.0.1";
pub const TAGACTION_VERSION: &str = "0.2.0";

/// A file the engine serves from its own origin under `/_engine/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Asset {
    /// URL path, e.g. `/_engine/tagbrush-2.0.1.min.<hash>.css`: content
    /// hashed ([`crate::assets`]), so it is served `immutable`.
    pub path: &'static str,
    pub body: &'static [u8],
}

impl Asset {
    /// The asset at `path` with the hash of `body` in its name. Made once
    /// per process (the name is kept for the life of the process).
    fn hashed(path: &str, body: &'static [u8]) -> Self {
        Asset {
            path: Box::leak(crate::assets::hashed_name(path, body).into_boxed_str()),
            body,
        }
    }
}

pub static TAGBRUSH_CSS: LazyLock<Asset> = LazyLock::new(|| {
    Asset::hashed(
        "/_engine/tagbrush-2.0.1.min.css",
        include_bytes!("../vendor/tagbrush/tagbrush.min.css"),
    )
});
pub static TAGACTION_JS: LazyLock<Asset> = LazyLock::new(|| {
    Asset::hashed(
        "/_engine/tagaction-0.2.0.min.js",
        include_bytes!("../vendor/tagaction/tagaction.min.js"),
    )
});
// Linked from nowhere (kept for the licence terms), so not hashed.
const TAGBRUSH_LICENSE: Asset = Asset {
    path: "/_engine/licenses/tagbrush-LICENSE.txt",
    body: include_bytes!("../vendor/tagbrush/LICENSE"),
};
const TAGACTION_LICENSE: Asset = Asset {
    path: "/_engine/licenses/tagaction-LICENSE.txt",
    body: include_bytes!("../vendor/tagaction/LICENSE"),
};

/// Path of the generated token stylesheet before its hash is added
/// (`/_engine/theme.<hash>.css`, see [`UiProviders::theme_href`]).
pub const THEME_CSS_PATH: &str = "/_engine/theme.css";

/// Hammerwright token name → TagBrush custom property.
pub const TOKEN_MAP: &[(&str, &str)] = &[
    ("primary", "--color-primary"),
    ("on_primary", "--color-on-primary"),
    ("link", "--color-link"),
    ("background", "--color-background"),
    ("surface", "--color-surface"),
    ("text", "--color-text"),
    ("heading", "--color-heading"),
    ("muted", "--color-text-muted"),
    ("border", "--color-border"),
    ("highlight", "--color-highlight"),
    ("error", "--color-error"),
    ("success", "--color-success"),
    ("warning", "--color-warning"),
    ("font", "--font-family-base"),
    ("font_heading", "--font-family-heading"),
    ("font_mono", "--font-family-mono"),
    ("font_size", "--font-size-base"),
    ("heading_weight", "--font-weight-heading"),
    ("line_height", "--line-height"),
    ("radius", "--border-radius"),
    ("spacing", "--spacing"),
    ("width", "--container-max-width"),
    ("reading_width", "--reading-width"),
    ("hero_background", "--hero-background"),
];

/// What kind of value a token takes (for editors and tools): `color`,
/// `font`, `length`, `number` or `background`.
pub fn token_kind(name: &str) -> &'static str {
    match name {
        "font" | "font_heading" | "font_mono" => "font",
        "font_size" | "radius" | "spacing" | "width" | "reading_width" => "length",
        "heading_weight" | "line_height" => "number",
        "hero_background" => "background",
        _ => "color",
    }
}

/// Styling choices a `token` field (a block's design option, set in the
/// visual editor or the admin) may hold: per group, the token names an
/// editor picks from. Values are names, never CSS: a template prints the
/// name in a data attribute (`data-color`, `data-background`, `data-font`,
/// `data-padding`, see [`TOKEN_ATTRIBUTES`]) and the engine's token
/// stylesheet (`components/tokens.css`, loaded with `token_assets()`) maps
/// it onto the token's custom property. So an editor can only choose what
/// the site's design defines, and a strict CSP needs no inline style.
///
/// `background` lists only surfaces text stays readable on (`primary` sets
/// the text and links to `on_primary`); `spacing` is TagBrush's scale
/// derived from the `spacing` token.
pub const TOKEN_CHOICES: &[(&str, &[&str])] = &[
    (
        "color",
        &[
            "text", "heading", "muted", "primary", "link", "error", "success", "warning",
        ],
    ),
    (
        "background",
        &["background", "surface", "highlight", "primary"],
    ),
    ("font", &["font", "font_heading", "font_mono"]),
    ("spacing", &["xs", "sm", "md", "lg", "xl"]),
];

/// The data attribute each token group is printed in, and the CSS property
/// `tokens.css` sets from it.
pub const TOKEN_ATTRIBUTES: &[(&str, &str, &str)] = &[
    ("color", "data-color", "color"),
    ("background", "data-background", "background-color"),
    ("font", "data-font", "font-family"),
    ("spacing", "data-padding", "padding"),
];

/// The names a `token` field of `group` may hold, or `None` for an unknown
/// group.
pub fn token_choices(group: &str) -> Option<&'static [&'static str]> {
    TOKEN_CHOICES
        .iter()
        .find(|(g, _)| *g == group)
        .map(|(_, names)| *names)
}

/// The custom property behind a token choice (`primary` →
/// `--color-primary`, spacing `lg` → `--spacing-lg`).
pub fn choice_property(group: &str, name: &str) -> Option<String> {
    if group == "spacing" {
        return token_choices(group)?
            .contains(&name)
            .then(|| format!("--spacing-{name}"));
    }
    if !token_choices(group)?.contains(&name) {
        return None;
    }
    TOKEN_MAP
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, p)| (*p).to_string())
}

/// The design tokens as data: every named token with its custom property,
/// kind and the site's value (`null` when the site keeps TagBrush's
/// default), then the site's raw `--custom-property` tokens; and the
/// choices `token` fields offer ([`TOKEN_CHOICES`]).
pub fn tokens_json(tokens: &BTreeMap<String, TokenValue>) -> serde_json::Value {
    let value = |v: Option<&TokenValue>| match v {
        None => serde_json::Value::Null,
        Some(TokenValue::One(s)) => serde_json::json!(s),
        Some(TokenValue::LightDark([l, d])) => serde_json::json!({ "light": l, "dark": d }),
    };
    let lookup = |name: &str| {
        tokens
            .iter()
            .find(|(k, _)| k.replace('-', "_") == name)
            .map(|(_, v)| v)
    };
    let mut list: Vec<serde_json::Value> = TOKEN_MAP
        .iter()
        .map(|(name, prop)| {
            serde_json::json!({
                "name": name,
                "property": prop,
                "kind": token_kind(name),
                "value": value(lookup(name)),
            })
        })
        .collect();
    for (k, v) in tokens.iter().filter(|(k, _)| k.starts_with("--")) {
        list.push(serde_json::json!({
            "name": k, "property": k, "kind": "raw", "value": value(Some(v)),
        }));
    }
    let choices: Vec<serde_json::Value> = TOKEN_CHOICES
        .iter()
        .map(|(group, names)| {
            let attr = TOKEN_ATTRIBUTES
                .iter()
                .find(|(g, _, _)| g == group)
                .map(|(_, a, _)| *a);
            serde_json::json!({
                "group": group,
                "attribute": attr,
                "names": names.iter().map(|n| serde_json::json!({
                    "name": n,
                    "property": choice_property(group, n),
                    "value": if *group == "spacing" { serde_json::Value::Null } else { value(lookup(n)) },
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::json!({
        "stylesheet": token_css(tokens)
            .ok()
            .flatten()
            .map(|css| crate::assets::hashed_name(THEME_CSS_PATH, css.as_bytes())),
        "tokens": list,
        "choices": choices,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Provider {
    Builtin(Asset),
    None,
    /// A site path (`/css/site.css`) or an absolute `https://` URL.
    Url(String),
}

impl Provider {
    pub fn href(&self) -> Option<&str> {
        match self {
            Provider::Builtin(a) => Some(a.path),
            Provider::None => None,
            Provider::Url(u) => Some(u),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiProviders {
    pub css: Provider,
    pub behaviour: Provider,
    /// `:root { ... }` stylesheet for the design tokens, if any are set.
    pub theme_css: Option<String>,
    /// Its content-hashed URL path.
    pub theme_href: Option<String>,
}

impl UiProviders {
    pub fn from_config(ui: &UiConfig) -> Result<Self, ConfigError> {
        let css = parse_provider("css", &ui.css, &[("tagbrush", *TAGBRUSH_CSS)])?;
        let behaviour =
            parse_provider("behaviour", &ui.behaviour, &[("tagaction", *TAGACTION_JS)])?;
        let theme_css = token_css(&ui.tokens)?;
        let theme_href = theme_css
            .as_ref()
            .map(|css| crate::assets::hashed_name(THEME_CSS_PATH, css.as_bytes()));
        Ok(Self {
            css,
            behaviour,
            theme_css,
            theme_href,
        })
    }

    /// Engine-owned files to write into the output folder (and serve).
    pub fn assets(&self) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        if let Provider::Builtin(a) = self.css {
            out.push((a.path.to_string(), a.body.to_vec()));
            out.push((
                TAGBRUSH_LICENSE.path.to_string(),
                TAGBRUSH_LICENSE.body.to_vec(),
            ));
        }
        if let Provider::Builtin(a) = self.behaviour {
            out.push((a.path.to_string(), a.body.to_vec()));
            out.push((
                TAGACTION_LICENSE.path.to_string(),
                TAGACTION_LICENSE.body.to_vec(),
            ));
        }
        if let (Some(css), Some(href)) = (&self.theme_css, &self.theme_href) {
            out.push((href.clone(), css.clone().into_bytes()));
        }
        out
    }
}

fn parse_provider(
    key: &str,
    value: &str,
    builtins: &[(&str, Asset)],
) -> Result<Provider, ConfigError> {
    let v = value.trim();
    if v == "none" {
        return Ok(Provider::None);
    }
    if let Some((_, a)) = builtins.iter().find(|(n, _)| *n == v) {
        return Ok(Provider::Builtin(*a));
    }
    let safe = !v.is_empty()
        && !v.chars().any(|c| {
            c.is_whitespace() || c.is_control() || matches!(c, '"' | '\'' | '<' | '>' | '`')
        });
    if safe && ((v.starts_with('/') && !v.starts_with("//")) || v.starts_with("https://")) {
        return Ok(Provider::Url(v.to_string()));
    }
    let names: Vec<_> = builtins.iter().map(|(n, _)| format!("\"{n}\"")).collect();
    Err(ConfigError::Invalid(format!(
        "[ui] {key} = \"{value}\" is not a known provider: use {}, \"none\", a site path such as \
         \"/css/site.css\", or an https:// URL",
        names.join(", ")
    )))
}

/// The CSS custom property a token name maps to.
pub fn token_property(name: &str) -> Result<String, ConfigError> {
    if let Some(raw) = name.strip_prefix("--") {
        if !raw.is_empty()
            && raw
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Ok(name.to_string());
        }
    } else {
        let key = name.replace('-', "_");
        if let Some((_, prop)) = TOKEN_MAP.iter().find(|(k, _)| *k == key) {
            return Ok((*prop).to_string());
        }
    }
    let known: Vec<_> = TOKEN_MAP.iter().map(|(k, _)| *k).collect();
    Err(ConfigError::Invalid(format!(
        "[ui.tokens] unknown token `{name}`; known tokens: {} (or a raw `--custom-property`)",
        known.join(", ")
    )))
}

fn check_token_value(name: &str, v: &str) -> Result<(), ConfigError> {
    let bad = v.trim().is_empty()
        || v.contains("/*")
        || v.chars()
            .any(|c| c.is_control() || matches!(c, ';' | '{' | '}' | '<' | '>' | '\\'));
    if bad {
        return Err(ConfigError::Invalid(format!(
            "[ui.tokens] `{name}` has an unsafe or empty value `{v}`"
        )));
    }
    Ok(())
}

/// Render `[ui.tokens]` as a `:root` stylesheet, or `None` when empty.
pub fn token_css(tokens: &BTreeMap<String, TokenValue>) -> Result<Option<String>, ConfigError> {
    if tokens.is_empty() {
        return Ok(None);
    }
    let mut css = String::from(
        "/* Generated by Hammerwright from [ui.tokens] in site.toml. Unlayered, so it wins over \
         TagBrush's @layer defaults. */\n:root {\n",
    );
    for (name, value) in tokens {
        let prop = token_property(name)?;
        let v = match value {
            TokenValue::One(v) => {
                check_token_value(name, v)?;
                v.trim().to_string()
            }
            TokenValue::LightDark([l, d]) => {
                check_token_value(name, l)?;
                check_token_value(name, d)?;
                format!("light-dark({}, {})", l.trim(), d.trim())
            }
        };
        css.push_str(&format!("  {prop}: {v};\n"));
    }
    css.push_str("}\n");
    Ok(Some(css))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ui(css: &str, behaviour: &str) -> UiConfig {
        UiConfig {
            css: css.into(),
            behaviour: behaviour.into(),
            tokens: BTreeMap::new(),
        }
    }

    #[test]
    fn defaults_are_the_bundled_tag_stack() {
        let p = UiProviders::from_config(&UiConfig::default()).unwrap();
        let css = p.css.href().unwrap();
        assert!(css.starts_with("/_engine/tagbrush-2.0.1.min.") && css.ends_with(".css"));
        assert!(crate::assets::is_fingerprinted(css), "{css}");
        let js = p.behaviour.href().unwrap();
        assert!(js.starts_with("/_engine/tagaction-0.2.0.min.") && js.ends_with(".js"));
        assert!(crate::assets::is_fingerprinted(js), "{js}");
        let assets = p.assets();
        assert_eq!(assets.len(), 4);
        assert!(
            String::from_utf8_lossy(&assets[0].1).starts_with("/*! TagBrush.css v2.0.1"),
            "bundled TagBrush must be the pinned release"
        );
        assert!(String::from_utf8_lossy(&assets[2].1).starts_with("/*! TagAction v0.2.0"));
    }

    #[test]
    fn none_and_custom_providers() {
        let p = UiProviders::from_config(&ui("none", "/js/site.js")).unwrap();
        assert_eq!(p.css, Provider::None);
        assert_eq!(p.behaviour.href(), Some("/js/site.js"));
        assert!(p.assets().is_empty());
        assert!(UiProviders::from_config(&ui("https://cdn.example/x.css", "none")).is_ok());
        for bad in [
            "pico",
            "//evil.example/x.css",
            "http://x/x.css",
            "/a\"><script>",
            "",
        ] {
            assert!(UiProviders::from_config(&ui(bad, "none")).is_err(), "{bad}");
        }
    }

    #[test]
    fn tokens_map_onto_tagbrush_properties() {
        let mut t = BTreeMap::new();
        t.insert("primary".into(), TokenValue::One("#b45309".into()));
        t.insert(
            "background".into(),
            TokenValue::LightDark(["#fafaf9".into(), "#0c0a09".into()]),
        );
        t.insert(
            "font-heading".into(),
            TokenValue::One("Georgia, serif".into()),
        );
        t.insert("--card-padding".into(), TokenValue::One("2rem".into()));
        let css = token_css(&t).unwrap().unwrap();
        assert!(css.contains("--color-primary: #b45309;"), "{css}");
        assert!(
            css.contains("--color-background: light-dark(#fafaf9, #0c0a09);"),
            "{css}"
        );
        assert!(css.contains("--font-family-heading: Georgia, serif;"));
        assert!(css.contains("--card-padding: 2rem;"));
    }

    #[test]
    fn tokens_as_data() {
        let mut t = BTreeMap::new();
        t.insert("primary".into(), TokenValue::One("#b45309".into()));
        t.insert(
            "font-heading".into(),
            TokenValue::LightDark(["a".into(), "b".into()]),
        );
        t.insert("--card-padding".into(), TokenValue::One("2rem".into()));
        let j = tokens_json(&t);
        let list = j["tokens"].as_array().unwrap();
        assert_eq!(list.len(), TOKEN_MAP.len() + 1);
        let get = |n: &str| list.iter().find(|x| x["name"] == n).unwrap().clone();
        assert_eq!(get("primary")["value"], "#b45309");
        assert_eq!(get("primary")["kind"], "color");
        assert_eq!(get("font_heading")["value"]["dark"], "b");
        assert_eq!(get("spacing")["value"], serde_json::Value::Null);
        assert_eq!(get("spacing")["kind"], "length");
        assert_eq!(get("--card-padding")["kind"], "raw");
    }

    #[test]
    fn token_choices_are_tokens_and_styled() {
        let css = include_str!("../builtin/components/tokens.css");
        for (group, names) in TOKEN_CHOICES {
            let (_, attr, prop) = TOKEN_ATTRIBUTES
                .iter()
                .find(|(g, _, _)| g == group)
                .unwrap();
            for n in *names {
                let custom = choice_property(group, n)
                    .unwrap_or_else(|| panic!("{group}/{n} names no token"));
                let rule = format!("[{attr}=\"{n}\"] {{ {prop}: var({custom}");
                assert!(css.contains(&rule), "tokens.css lacks `{rule}`");
            }
        }
        assert_eq!(choice_property("color", "font"), None);
        assert_eq!(choice_property("nope", "text"), None);
        let j = tokens_json(&BTreeMap::new());
        assert_eq!(j["choices"].as_array().unwrap().len(), TOKEN_CHOICES.len());
    }

    #[test]
    fn bad_tokens_are_rejected() {
        let mut t = BTreeMap::new();
        t.insert(
            "primary".into(),
            TokenValue::One("red; } body { x: y".into()),
        );
        assert!(token_css(&t).is_err());
        let mut t = BTreeMap::new();
        t.insert("colour".into(), TokenValue::One("red".into()));
        assert!(
            token_css(&t)
                .unwrap_err()
                .to_string()
                .contains("unknown token")
        );
        assert_eq!(token_css(&BTreeMap::new()).unwrap(), None);
    }

    #[test]
    fn vendor_manifest_matches_bundled_versions() {
        let manifest: toml::Table = toml::from_str(include_str!("../vendor/VENDOR.toml")).unwrap();
        let pkgs = manifest["package"].as_array().unwrap();
        let version = |name: &str| {
            pkgs.iter()
                .find(|p| p["name"].as_str() == Some(name))
                .and_then(|p| p["version"].as_str())
                .unwrap()
                .to_string()
        };
        assert_eq!(version("tagbrush"), TAGBRUSH_VERSION);
        assert_eq!(version("tagaction"), TAGACTION_VERSION);
        assert!(TAGBRUSH_CSS.path.contains(TAGBRUSH_VERSION));
        assert!(TAGACTION_JS.path.contains(TAGACTION_VERSION));
    }
}
