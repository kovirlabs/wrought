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
//! Design tokens (`[ui.tokens]`) use Wrought's names and are mapped onto
//! TagBrush's custom properties (see [`TOKEN_MAP`]); `--raw-name` keys pass
//! through unchanged. A `[light, dark]` pair becomes `light-dark(a, b)`.

use std::collections::BTreeMap;

use crate::config::{ConfigError, TokenValue, UiConfig};

pub const TAGBRUSH_VERSION: &str = "2.0.1";
pub const TAGACTION_VERSION: &str = "0.2.0";

/// A file the engine serves from its own origin under `/_engine/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Asset {
    /// URL path, e.g. `/_engine/tagbrush-2.0.1.min.css`.
    pub path: &'static str,
    pub body: &'static [u8],
}

pub const TAGBRUSH_CSS: Asset = Asset {
    path: "/_engine/tagbrush-2.0.1.min.css",
    body: include_bytes!("../vendor/tagbrush/tagbrush.min.css"),
};
pub const TAGACTION_JS: Asset = Asset {
    path: "/_engine/tagaction-0.2.0.min.js",
    body: include_bytes!("../vendor/tagaction/tagaction.min.js"),
};
const TAGBRUSH_LICENSE: Asset = Asset {
    path: "/_engine/licenses/tagbrush-LICENSE.txt",
    body: include_bytes!("../vendor/tagbrush/LICENSE"),
};
const TAGACTION_LICENSE: Asset = Asset {
    path: "/_engine/licenses/tagaction-LICENSE.txt",
    body: include_bytes!("../vendor/tagaction/LICENSE"),
};

/// Path of the generated token stylesheet.
pub const THEME_CSS_PATH: &str = "/_engine/theme.css";

/// Wrought token name → TagBrush custom property.
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
}

impl UiProviders {
    pub fn from_config(ui: &UiConfig) -> Result<Self, ConfigError> {
        let css = parse_provider("css", &ui.css, &[("tagbrush", TAGBRUSH_CSS)])?;
        let behaviour = parse_provider("behaviour", &ui.behaviour, &[("tagaction", TAGACTION_JS)])?;
        let theme_css = token_css(&ui.tokens)?;
        Ok(Self {
            css,
            behaviour,
            theme_css,
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
        if let Some(css) = &self.theme_css {
            out.push((THEME_CSS_PATH.to_string(), css.clone().into_bytes()));
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
        "/* Generated by Wrought from [ui.tokens] in site.toml. Unlayered, so it wins over \
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
        assert_eq!(p.css.href(), Some("/_engine/tagbrush-2.0.1.min.css"));
        assert_eq!(p.behaviour.href(), Some("/_engine/tagaction-0.2.0.min.js"));
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
