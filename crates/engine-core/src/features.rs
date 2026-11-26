//! Which capabilities this binary was compiled with.
//!
//! Capabilities are Cargo features. `core` is always on; `db-postgres`,
//! `content` and `publish` (v0.2) are marker features of this crate that the
//! CLI switches on together with the crates that implement them.
//! [`ensure_mode`] and [`ensure_feature`] are the single place where "not in
//! this build" errors come from, so new features slot in here.

use crate::config::{ConfigError, RenderMode};

/// The preset this binary corresponds to.
pub const PRESET: &str = if cfg!(feature = "cms") {
    "cms"
} else {
    "static"
};

/// Every feature name the spec defines, for clear "not in this build" errors
/// (as opposed to "unknown feature" for typos). `request` is the render mode
/// of roadmap v0.4.
pub const KNOWN_FEATURES: &[&str] = &[
    "core",
    "static",
    "content",
    "db-sqlite",
    "db-postgres",
    "db",
    "publish",
    "request",
    "auth",
    "roles",
    "admin",
    "workflow",
    "forms",
    "jobs",
    "mail",
    "media",
    "search",
    "api",
    "payments",
    "oidc",
    "visual-editor",
];

/// Features compiled into this binary.
pub fn compiled() -> Vec<&'static str> {
    #[allow(unused_mut)]
    let mut v = vec!["core"];
    #[cfg(feature = "static")]
    v.push("static");
    #[cfg(feature = "db-postgres")]
    v.extend(["db", "db-postgres"]);
    #[cfg(feature = "content")]
    v.push("content");
    #[cfg(feature = "publish")]
    v.push("publish");
    v
}

/// Render modes this binary supports, for messages.
pub fn available_modes() -> Vec<&'static str> {
    [RenderMode::Build, RenderMode::Publish, RenderMode::Request]
        .into_iter()
        .filter(|m| mode_available(*m))
        .map(RenderMode::as_str)
        .collect()
}

pub fn has_feature(name: &str) -> bool {
    compiled().contains(&name)
}

pub fn mode_available(mode: RenderMode) -> bool {
    match mode.required_feature() {
        None => true,
        Some(f) => has_feature(f),
    }
}

pub fn ensure_mode(what: &str, mode: RenderMode) -> Result<(), ConfigError> {
    if mode_available(mode) {
        return Ok(());
    }
    Err(ConfigError::ModeUnavailable {
        what: what.to_string(),
        mode,
        preset: PRESET,
        available: available_modes().join(", "),
        feature: mode.required_feature().unwrap_or("core"),
    })
}

pub fn ensure_feature(what: &str, feature: &str) -> Result<(), ConfigError> {
    if has_feature(feature) {
        return Ok(());
    }
    if KNOWN_FEATURES.contains(&feature) {
        Err(ConfigError::Invalid(format!(
            "{what} requires the `{feature}` feature, which is not in this build (preset `{PRESET}`)"
        )))
    } else {
        Err(ConfigError::Invalid(format!(
            "{what} requires unknown feature `{feature}`"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_follow_features() {
        assert!(mode_available(RenderMode::Build));
        assert_eq!(
            mode_available(RenderMode::Publish),
            cfg!(feature = "publish")
        );
        assert!(!mode_available(RenderMode::Request));
        assert!(compiled().contains(&"core"));
        assert_eq!(compiled().contains(&"content"), cfg!(feature = "content"));
    }

    #[test]
    fn unknown_vs_missing_feature() {
        let missing = ensure_feature("x", "auth").unwrap_err().to_string();
        assert!(missing.contains("not in this build"));
        let unknown = ensure_feature("x", "teleport").unwrap_err().to_string();
        assert!(unknown.contains("unknown feature"));
    }
}
