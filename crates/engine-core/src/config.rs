//! `site.toml` parsing and validation.
//!
//! The config shape follows the spec: `[site]`, `[render]`, `[[routes]]` and
//! `[ui]`. `[database]` and `[publish]` are kept as raw tables here and parsed
//! by the crates that use them (engine-db, engine-publish); in a build without
//! those features they are accepted with a warning, so a site written for a
//! fuller build still parses.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::features;

/// The render dial. Every route picks one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RenderMode {
    /// HTML made at `engine build`, stored as files on disk.
    #[default]
    Build,
    /// HTML made when content changes, stored as files on disk (v0.2).
    Publish,
    /// HTML made on every request, fragments cached in memory (v0.4).
    Request,
}

impl RenderMode {
    pub fn as_str(self) -> &'static str {
        match self {
            RenderMode::Build => "build",
            RenderMode::Publish => "publish",
            RenderMode::Request => "request",
        }
    }

    /// The Cargo feature that provides this mode, if it is not part of `core`.
    pub fn required_feature(self) -> Option<&'static str> {
        match self {
            RenderMode::Build => None,
            RenderMode::Publish => Some("publish"),
            RenderMode::Request => Some("request"),
        }
    }
}

impl fmt::Display for RenderMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Errors from loading or validating a site configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid site.toml: {0}")]
    Parse(String),
    #[error("{0}")]
    Invalid(String),
    #[error(
        "{what} uses render mode `{mode}`, which is not in this build: this engine was compiled \
         with the `{preset}` preset, which supports: {available}. Set `mode = \"build\"`, or use \
         a build with the `{feature}` feature."
    )]
    ModeUnavailable {
        what: String,
        mode: RenderMode,
        preset: &'static str,
        available: String,
        feature: &'static str,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SiteConfig {
    pub site: SiteSection,
    #[serde(default)]
    pub render: RenderSection,
    #[serde(default)]
    pub routes: Vec<RouteConfig>,
    #[serde(default)]
    pub ui: UiConfig,
    /// `[database]`, parsed by engine-db (`db-*` features).
    #[serde(default, skip_serializing)]
    pub database: Option<toml::Table>,
    /// `[publish]`, parsed by engine-publish (`publish` feature).
    #[serde(default, skip_serializing)]
    pub publish: Option<toml::Table>,
    /// `[auth]`, parsed by engine-auth (`auth` feature).
    #[serde(default, skip_serializing)]
    pub auth: Option<toml::Table>,
    /// `[cache]`: the `request` mode's fragment cache.
    #[serde(default, skip_serializing)]
    pub cache: crate::fragments::CacheConfig,
    /// `[limits]`: overrides of the limits sized from the machine.
    #[serde(default, skip_serializing)]
    pub limits: crate::limits::LimitsConfig,
    /// `[forms]`, parsed by engine-forms (`forms` feature).
    #[serde(default, skip_serializing)]
    pub forms: Option<toml::Table>,
    /// `[jobs]`, parsed by engine-jobs (`jobs` feature).
    #[serde(default, skip_serializing)]
    pub jobs: Option<toml::Table>,
    /// `[mail]`, parsed by engine-mail (`mail` feature).
    #[serde(default, skip_serializing)]
    pub mail: Option<toml::Table>,
    /// `[workflow]`, parsed by engine-admin (`workflow` feature).
    #[serde(default, skip_serializing)]
    pub workflow: Option<toml::Table>,
    /// `[media]`, parsed by engine-media (`media` feature).
    #[serde(default, skip_serializing)]
    pub media: Option<toml::Table>,
    /// `[search]`, parsed by engine-search (`search` feature).
    #[serde(default, skip_serializing)]
    pub search: Option<toml::Table>,
    /// `[api]`, parsed by engine-api (`api` feature).
    #[serde(default, skip_serializing)]
    pub api: Option<toml::Table>,
    /// `[payments]`, parsed by engine-payments (`payments` feature).
    #[serde(default, skip_serializing)]
    pub payments: Option<toml::Table>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SiteSection {
    pub name: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default = "default_output")]
    pub output: PathBuf,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default = "default_language")]
    pub language: String,
    /// Primary navigation, rendered by the `header` component.
    #[serde(default)]
    pub nav: Vec<NavLink>,
    /// Free-form values exposed to templates as `site.extra`.
    #[serde(default)]
    pub extra: BTreeMap<String, toml::Value>,
}

fn default_output() -> PathBuf {
    PathBuf::from("public")
}

fn default_language() -> String {
    "en".to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NavLink {
    pub label: String,
    pub url: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RenderSection {
    #[serde(default)]
    pub default: RenderMode,
}

/// An explicit `[[routes]]` entry.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RouteConfig {
    pub path: String,
    #[serde(default)]
    pub template: Option<String>,
    #[serde(default)]
    pub mode: Option<RenderMode>,
    /// A feature the route needs, checked against this build. `auth` also
    /// means the page is only shown to a signed-in user: the route must use
    /// `request` mode (a published file would be readable by anyone).
    #[serde(default)]
    pub requires: Option<String>,
    /// Serve the stored file even to signed-in users and to requests with a
    /// query string (which otherwise render in `request` mode when this
    /// build has it). For pages that never show anything personal.
    #[serde(default)]
    pub always_static: bool,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// Content type (`content` feature): one page per published item, at a
    /// path with a `{slug}` or `{id}` segment, e.g. `/articles/{slug}/`.
    #[serde(default, rename = "type")]
    pub content_type: Option<String>,
    /// Taxonomy vocabulary (`content` feature): one page per term, at a path
    /// with a `{term}` segment, e.g. `/topics/{term}/`.
    #[serde(default)]
    pub taxonomy: Option<String>,
    /// Paginated list of a content type (`content` feature): the route's
    /// path is page 1, then `page/2/`, `page/3/`... Templates get `listing`.
    #[serde(default)]
    pub list: Option<ListConfig>,
}

/// `[routes.list]`: a paginated list of published items.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ListConfig {
    /// Content type to list.
    #[serde(rename = "type")]
    pub content_type: String,
    /// Items per page.
    #[serde(default = "default_per_page")]
    pub per_page: usize,
    /// Sort order, as for `list_items` (`newest` by default).
    #[serde(default)]
    pub order: Option<String>,
    /// Only items with this term (`rust` or `topics/rust`).
    #[serde(default)]
    pub term: Option<String>,
}

fn default_per_page() -> usize {
    10
}

/// URL paths the engine itself answers when a feature is compiled in. Site
/// routes may not use them.
pub fn reserved_paths() -> Vec<&'static str> {
    #[allow(unused_mut)]
    let mut v = vec!["/_engine/"];
    if features::has_feature("auth") {
        v.extend([
            "/account/login/",
            "/account/logout/",
            "/account/reset/",
            "/account/totp/",
            "/account/register/",
            "/account/profile/",
        ]);
    }
    if features::has_feature("oidc") {
        v.push("/account/oidc/");
    }
    if features::has_feature("admin") {
        v.push("/admin/");
    }
    v
}

impl RouteConfig {
    pub fn is_wildcard(&self) -> bool {
        self.path.contains('*')
    }

    /// Whether this route expands into one page per item or term.
    pub fn is_pattern(&self) -> bool {
        self.content_type.is_some() || self.taxonomy.is_some()
    }

    /// Whether the `content` feature makes this route's pages (item, term
    /// and paginated list routes).
    pub fn is_dynamic(&self) -> bool {
        self.is_pattern() || self.list.is_some()
    }

    /// Whether the page needs a signed-in user.
    pub fn requires_auth(&self) -> bool {
        self.requires.as_deref() == Some("auth")
    }

    /// For a wildcard route `/x/*`, the prefix `/x/`.
    pub fn wildcard_prefix(&self) -> Option<&str> {
        self.path.strip_suffix('*')
    }
}

/// `[ui]`: which CSS and behaviour providers pages load, plus design tokens.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct UiConfig {
    #[serde(default = "default_css")]
    pub css: String,
    #[serde(default = "default_behaviour", alias = "behavior")]
    pub behaviour: String,
    /// Design tokens, emitted as CSS custom properties on `:root`.
    #[serde(default)]
    pub tokens: BTreeMap<String, TokenValue>,
}

/// A token value: one value, or a `[light, dark]` pair.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(untagged)]
pub enum TokenValue {
    One(String),
    LightDark([String; 2]),
}

fn default_css() -> String {
    "tagbrush".to_string()
}

fn default_behaviour() -> String {
    "tagaction".to_string()
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            css: default_css(),
            behaviour: default_behaviour(),
            tokens: BTreeMap::new(),
        }
    }
}

impl SiteConfig {
    /// Parse `site.toml` text. Does not validate; call [`SiteConfig::validate`].
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let mut cfg: SiteConfig =
            toml::from_str(text).map_err(|e| ConfigError::Parse(e.to_string()))?;
        cfg.site.base_url = cfg.site.base_url.trim_end_matches('/').to_string();
        Ok(cfg)
    }

    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&text)
    }

    /// The mode a route actually uses: its own, else `[render] default`.
    pub fn effective_mode(&self, mode: Option<RenderMode>) -> RenderMode {
        mode.unwrap_or(self.render.default)
    }

    /// Check the config against this build. Returns warnings on success.
    pub fn validate(&self) -> Result<Vec<String>, ConfigError> {
        let mut warnings = Vec::new();

        if self.site.name.trim().is_empty() {
            return Err(ConfigError::Invalid("[site] name must not be empty".into()));
        }
        validate_output(&self.site.output)?;
        let url = &self.site.base_url;
        let url_ok = url.is_empty() || url.starts_with("https://") || url.starts_with("http://");
        if !url_ok {
            return Err(ConfigError::Invalid(format!(
                "[site] base_url must start with http:// or https:// (got `{}`)",
                self.site.base_url
            )));
        }

        // Pages without an explicit mode use `[render] default`.
        features::ensure_mode("[render] default", self.render.default)?;

        let mut seen = std::collections::BTreeSet::new();
        for route in &self.routes {
            let what = format!("route `{}`", route.path);
            if !route.path.starts_with('/') {
                return Err(ConfigError::Invalid(format!(
                    "{what}: path must start with `/`"
                )));
            }
            if !seen.insert(crate::routes::normalize_path(&route.path)) {
                return Err(ConfigError::Invalid(format!("{what} is declared twice")));
            }
            let mode = self.effective_mode(route.mode);
            features::ensure_mode(&what, mode)?;
            if let Some(req) = &route.requires {
                features::ensure_feature(&what, req)?;
            }
            if route.requires_auth() && mode != RenderMode::Request {
                return Err(ConfigError::Invalid(format!(
                    "{what}: `requires = \"auth\"` needs `mode = \"request\"`: a `{mode}` \
                     page is written to a file that anyone could read"
                )));
            }
            let normal = crate::routes::normalize_path(&route.path);
            if let Some(r) = reserved_paths()
                .into_iter()
                .find(|r| normal.starts_with(r) || r.trim_end_matches('/') == normal)
            {
                return Err(ConfigError::Invalid(format!(
                    "{what}: `{r}` is answered by the engine in this build; choose another path"
                )));
            }
            if route.is_wildcard() {
                let ok_shape = route.path.ends_with("/*") && route.path.matches('*').count() == 1;
                if mode != RenderMode::Request || !ok_shape || route.is_dynamic() {
                    return Err(ConfigError::Invalid(format!(
                        "{what}: wildcard paths (`/prefix/*`) need `mode = \"request\"` and no \
                         `type`, `taxonomy` or `list`"
                    )));
                }
                crate::routes::check_route_path(route.path.trim_end_matches('*'))
                    .map_err(|e| ConfigError::Invalid(format!("{what}: {e}")))?;
                continue;
            }
            if let Some(list) = &route.list {
                features::ensure_feature(&what, "content")?;
                if route.is_pattern() {
                    return Err(ConfigError::Invalid(format!(
                        "{what}: `list` cannot be combined with `type` or `taxonomy`"
                    )));
                }
                if route.template.is_none() {
                    return Err(ConfigError::Invalid(format!(
                        "{what}: a list route needs a `template`"
                    )));
                }
                if !(1..=1000).contains(&list.per_page) {
                    return Err(ConfigError::Invalid(format!(
                        "{what}: `list.per_page` must be between 1 and 1000"
                    )));
                }
                if !normal.ends_with('/') {
                    return Err(ConfigError::Invalid(format!(
                        "{what}: a list route's path must be a folder like `/articles/`"
                    )));
                }
            }
            if route.is_pattern() {
                features::ensure_feature(&what, "content")?;
                let expected = match (&route.content_type, &route.taxonomy) {
                    (Some(_), None) => &["slug", "id"][..],
                    (None, Some(_)) => &["term"][..],
                    _ => {
                        return Err(ConfigError::Invalid(format!(
                            "{what}: set `type` or `taxonomy`, not both"
                        )));
                    }
                };
                if route.template.is_none() {
                    return Err(ConfigError::Invalid(format!(
                        "{what}: item and term routes need a `template`"
                    )));
                }
                crate::routes::check_route_pattern(&route.path, expected)
                    .map_err(|e| ConfigError::Invalid(format!("{what}: {e}")))?;
            } else {
                crate::routes::check_route_path(&route.path)
                    .map_err(|e| ConfigError::Invalid(format!("{what}: {e}")))?;
            }
            if let Some(t) = &route.template {
                crate::templates::validate_template_name(t)
                    .map_err(|e| ConfigError::Invalid(format!("{what}: {e}")))?;
            }
        }

        crate::ui::UiProviders::from_config(&self.ui)?;

        if self.database.is_some() && !features::has_feature("db") {
            warnings.push(
                "[database] is ignored: this build has no `db-sqlite` / `db-postgres` feature"
                    .to_string(),
            );
        }
        if self.publish.is_some()
            && !features::has_feature("publish")
            && !features::has_feature("s3")
        {
            warnings.push(
                "[publish] is ignored: this build has neither the `publish` nor the `s3` feature"
                    .to_string(),
            );
        }
        for (section, present, feature) in [
            ("auth", self.auth.is_some(), "auth"),
            ("forms", self.forms.is_some(), "forms"),
            ("jobs", self.jobs.is_some(), "jobs"),
            ("mail", self.mail.is_some(), "mail"),
            ("workflow", self.workflow.is_some(), "workflow"),
            ("media", self.media.is_some(), "media"),
            ("api", self.api.is_some(), "api"),
            ("payments", self.payments.is_some(), "payments"),
        ] {
            if present && !features::has_feature(feature) {
                warnings.push(format!(
                    "[{section}] is ignored: this build has no `{feature}` feature"
                ));
            }
        }
        let l = &self.limits;
        if [
            l.worker_threads,
            l.blocking_threads,
            l.background_threads,
            l.etag_cache_entries,
        ]
        .contains(&Some(0))
        {
            return Err(ConfigError::Invalid(
                "[limits] thread counts and etag_cache_entries must be at least 1".into(),
            ));
        }
        if self.cache.max_mb == Some(0) || self.cache.fragment_ttl_seconds == 0 {
            return Err(ConfigError::Invalid(
                "[cache] max_mb and fragment_ttl_seconds must be at least 1".into(),
            ));
        }
        Ok(warnings)
    }
}

fn validate_output(output: &Path) -> Result<(), ConfigError> {
    let bad = || {
        ConfigError::Invalid(format!(
            "[site] output `{}` must be a relative folder inside the site (no `..`, not the site \
             root itself)",
            output.display()
        ))
    };
    if output.as_os_str().is_empty() || output.is_absolute() {
        return Err(bad());
    }
    let mut normal = 0;
    for c in output.components() {
        match c {
            Component::Normal(_) => normal += 1,
            Component::CurDir => {}
            _ => return Err(bad()),
        }
    }
    if normal == 0 {
        return Err(bad());
    }
    // The output folder must not swallow the site's own sources.
    let first = output
        .components()
        .find_map(|c| match c {
            Component::Normal(n) => n.to_str(),
            _ => None,
        })
        .unwrap_or("");
    if ["content", "templates", "static", "site.toml"].contains(&first) {
        return Err(ConfigError::Invalid(format!(
            "[site] output `{}` would overwrite the site's sources",
            output.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC_EXAMPLE: &str = r#"
[site]
name = "Example"
base_url = "https://example.org/"
output = "public"

[database]
url = "postgres://engine@localhost/example"

[render]
default = "publish"

[[routes]]
path = "/"
template = "home.html"
mode = "publish"

[[routes]]
path = "/account/*"
mode = "request"
requires = "auth"
"#;

    #[test]
    fn parses_spec_example() {
        let cfg = SiteConfig::parse(SPEC_EXAMPLE).unwrap();
        assert_eq!(cfg.site.name, "Example");
        assert_eq!(cfg.site.base_url, "https://example.org");
        assert_eq!(cfg.render.default, RenderMode::Publish);
        assert_eq!(cfg.routes.len(), 2);
        assert_eq!(cfg.routes[1].mode, Some(RenderMode::Request));
        assert_eq!(cfg.routes[1].requires.as_deref(), Some("auth"));
        assert!(cfg.database.is_some());
    }

    #[test]
    fn publish_and_request_parse_but_are_rejected_clearly() {
        let cfg = SiteConfig::parse(SPEC_EXAMPLE).unwrap();
        let res = cfg.validate();
        if cfg!(feature = "publish") && cfg!(feature = "request") && cfg!(feature = "auth") {
            // The spec example is a valid site for a full build.
            res.unwrap();
        } else {
            let err = res.unwrap_err().to_string();
            assert!(err.contains("not in this build"), "{err}");
            if cfg!(feature = "publish") {
                assert!(err.contains("`request`"), "{err}");
            } else {
                assert!(err.contains("`publish`"), "{err}");
            }
        }

        let cfg = SiteConfig::parse(
            "[site]\nname='x'\n[[routes]]\npath='/app'\nmode='request'\ntemplate='a.html'\n",
        )
        .unwrap();
        let res = cfg.validate();
        if cfg!(feature = "request") {
            res.unwrap();
        } else {
            let err = res.unwrap_err().to_string();
            assert!(
                err.contains("`request`") && err.contains("not in this build"),
                "{err}"
            );
        }
    }

    #[test]
    fn auth_routes_must_render_per_request() {
        if !cfg!(feature = "auth") {
            return;
        }
        let cfg = SiteConfig::parse(
            "[site]\nname='x'\n[[routes]]\npath='/me/'\ntemplate='me.html'\nrequires='auth'\n",
        )
        .unwrap();
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("mode = \"request\""), "{err}");
        for reserved in ["/account/login/", "/admin/", "/admin/x/", "/_engine/x/"] {
            let cfg = SiteConfig::parse(&format!(
                "[site]\nname='x'\n[[routes]]\npath='{reserved}'\ntemplate='a.html'\n"
            ))
            .unwrap();
            if reserved.starts_with("/admin") && !cfg!(feature = "admin") {
                continue;
            }
            let err = cfg.validate().unwrap_err().to_string();
            assert!(err.contains("answered by the engine"), "{reserved}: {err}");
        }
        // Wildcards only in request mode.
        let ok = SiteConfig::parse(
            "[site]\nname='x'\n[[routes]]\npath='/account/*'\nmode='request'\n\
             template='a.html'\nrequires='auth'\n",
        )
        .unwrap();
        ok.validate().unwrap();
        let bad = SiteConfig::parse(
            "[site]\nname='x'\n[[routes]]\npath='/a/*/b'\nmode='request'\ntemplate='a.html'\n",
        )
        .unwrap();
        assert!(bad.validate().is_err());
    }

    #[test]
    fn list_routes() {
        let text = "[site]\nname='x'\n[[routes]]\npath='/articles/'\ntemplate='a.html'\n\
                    [routes.list]\ntype='article'\nper_page=5\n";
        let cfg = SiteConfig::parse(text).unwrap();
        assert!(cfg.routes[0].is_dynamic() && !cfg.routes[0].is_pattern());
        assert_eq!(cfg.routes[0].list.as_ref().unwrap().per_page, 5);
        let res = cfg.validate();
        if cfg!(feature = "content") {
            res.unwrap();
            for bad in [
                "path='/a/'\n[routes.list]\ntype='article'",
                "path='/a/'\ntemplate='a.html'\n[routes.list]\ntype='article'\nper_page=0",
                "path='/a.xml'\ntemplate='a.html'\n[routes.list]\ntype='article'",
            ] {
                let cfg =
                    SiteConfig::parse(&format!("[site]\nname='x'\n[[routes]]\n{bad}\n")).unwrap();
                assert!(cfg.validate().is_err(), "{bad}");
            }
        } else {
            assert!(res.unwrap_err().to_string().contains("`content`"));
        }
    }

    #[test]
    fn minimal_config_defaults() {
        let cfg = SiteConfig::parse("[site]\nname = 'Mini'\n").unwrap();
        assert_eq!(cfg.site.output, PathBuf::from("public"));
        assert_eq!(cfg.site.language, "en");
        assert_eq!(cfg.render.default, RenderMode::Build);
        assert_eq!(cfg.ui.css, "tagbrush");
        assert_eq!(cfg.ui.behaviour, "tagaction");
        assert!(cfg.validate().unwrap().is_empty());
    }

    #[test]
    fn sections_of_missing_features_warn() {
        let cfg = SiteConfig::parse(
            "[site]\nname='x'\n[database]\nurl='sqlite://x'\n[publish]\ntarget='s3'\n",
        )
        .unwrap();
        let w = cfg.validate().unwrap();
        assert_eq!(
            w.iter().any(|w| w.contains("[database]")),
            !cfg!(any(feature = "db-postgres", feature = "db-sqlite"))
        );
        assert_eq!(
            w.iter().any(|w| w.contains("[publish]")),
            !cfg!(any(feature = "publish", feature = "s3"))
        );
    }

    #[test]
    fn pattern_routes() {
        let text = "[site]\nname='x'\n[[routes]]\npath='/articles/{slug}/'\ntype='article'\n\
                    template='article.html'\n";
        let cfg = SiteConfig::parse(text).unwrap();
        assert!(cfg.routes[0].is_pattern());
        let res = cfg.validate();
        if cfg!(feature = "content") {
            res.unwrap();
            for bad in [
                "path='/articles/'\ntype='article'\ntemplate='a.html'",
                "path='/a/{slug}/{id}/'\ntype='article'\ntemplate='a.html'",
                "path='/a/{term}/'\ntype='article'\ntemplate='a.html'",
                "path='/a/{slug}/'\ntype='article'",
                "path='/a/{slug}/'\ntype='article'\ntaxonomy='t'\ntemplate='a.html'",
                "path='/a/x{slug}/'\ntype='article'\ntemplate='a.html'",
            ] {
                let cfg =
                    SiteConfig::parse(&format!("[site]\nname='x'\n[[routes]]\n{bad}\n")).unwrap();
                assert!(cfg.validate().is_err(), "{bad}");
            }
        } else {
            let err = res.unwrap_err().to_string();
            assert!(err.contains("`content`"), "{err}");
        }
        // A `{slug}` segment without `type` is just an invalid path.
        let cfg = SiteConfig::parse(
            "[site]\nname='x'\n[[routes]]\npath='/a/{slug}/'\ntemplate='a.html'\n",
        )
        .unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn unknown_keys_and_modes_are_errors() {
        assert!(SiteConfig::parse("[site]\nname='x'\ntypo=1\n").is_err());
        assert!(SiteConfig::parse("[site]\nname='x'\n[sitee]\n").is_err());
        let err = SiteConfig::parse("[site]\nname='x'\n[render]\ndefault='sometimes'\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("build"), "{err}");
        assert!(SiteConfig::parse("[render]\ndefault='build'\n").is_err());
    }

    #[test]
    fn ui_section_tokens_and_behavior_alias() {
        let cfg = SiteConfig::parse(
            "[site]\nname='x'\n[ui]\ncss='none'\nbehavior='none'\n[ui.tokens]\nprimary='#c33'\n\
             background=['#fff','#111']\n",
        )
        .unwrap();
        assert_eq!(cfg.ui.css, "none");
        assert_eq!(cfg.ui.behaviour, "none");
        assert_eq!(
            cfg.ui.tokens.get("primary"),
            Some(&TokenValue::One("#c33".into()))
        );
        assert_eq!(
            cfg.ui.tokens.get("background"),
            Some(&TokenValue::LightDark(["#fff".into(), "#111".into()]))
        );
        cfg.validate().unwrap();
    }

    #[test]
    fn rejects_unknown_ui_provider() {
        let cfg = SiteConfig::parse("[site]\nname='x'\n[ui]\ncss='bootstrap'\n").unwrap();
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("bootstrap"), "{err}");
    }

    #[test]
    fn rejects_unsafe_output_dirs() {
        for out in [
            "",
            ".",
            "..",
            "../x",
            "/tmp/out",
            "a/../..",
            "content",
            "templates/x",
        ] {
            let text = format!("[site]\nname='x'\noutput='{out}'\n");
            let cfg = SiteConfig::parse(&text).unwrap();
            assert!(cfg.validate().is_err(), "output `{out}` should be rejected");
        }
        let cfg = SiteConfig::parse("[site]\nname='x'\noutput='build/site'\n").unwrap();
        cfg.validate().unwrap();
    }

    #[test]
    fn route_checks() {
        let bad_path = SiteConfig::parse("[site]\nname='x'\n[[routes]]\npath='about'\n").unwrap();
        assert!(bad_path.validate().is_err());
        let wildcard = SiteConfig::parse("[site]\nname='x'\n[[routes]]\npath='/docs/*'\n").unwrap();
        assert!(wildcard.validate().is_err());
        let traversal =
            SiteConfig::parse("[site]\nname='x'\n[[routes]]\npath='/a/../../etc'\n").unwrap();
        assert!(traversal.validate().is_err());
        let needs_auth =
            SiteConfig::parse("[site]\nname='x'\n[[routes]]\npath='/me'\nrequires='auth'\n")
                .unwrap();
        let err = needs_auth.validate().unwrap_err().to_string();
        if cfg!(feature = "auth") {
            assert!(err.contains("mode = \"request\""), "{err}");
        } else {
            assert!(
                err.contains("auth") && err.contains("not in this build"),
                "{err}"
            );
        }
        let dup =
            SiteConfig::parse("[site]\nname='x'\n[[routes]]\npath='/a'\n[[routes]]\npath='/a/'\n")
                .unwrap();
        assert!(dup.validate().is_err());
        let bad_template = SiteConfig::parse(
            "[site]\nname='x'\n[[routes]]\npath='/a'\ntemplate='../secret.html'\n",
        )
        .unwrap();
        assert!(bad_template.validate().is_err());
    }
}
