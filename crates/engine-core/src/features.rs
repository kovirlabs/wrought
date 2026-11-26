//! Which capabilities this binary was compiled with.
//!
//! Capabilities are Cargo features. v0.1 only has `core` (always on) and the
//! `static` preset. Later milestones add real features (`content`, `publish`,
//! `auth`, ...); [`ensure_mode`] and [`ensure_feature`] are the single place
//! where "not in this build" errors come from, so new features slot in here.

use crate::config::{ConfigError, RenderMode};

/// The preset this binary corresponds to.
pub const PRESET: &str = "static";

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
    v
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
    fn only_build_mode_is_available() {
        assert!(mode_available(RenderMode::Build));
        assert!(!mode_available(RenderMode::Publish));
        assert!(!mode_available(RenderMode::Request));
        assert!(compiled().contains(&"core"));
    }

    #[test]
    fn unknown_vs_missing_feature() {
        let missing = ensure_feature("x", "auth").unwrap_err().to_string();
        assert!(missing.contains("not in this build"));
        let unknown = ensure_feature("x", "teleport").unwrap_err().to_string();
        assert!(unknown.contains("unknown feature"));
    }
}
