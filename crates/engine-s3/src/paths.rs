//! Object keys, headers and URL paths.
//!
//! * a page `/articles/x/` is the object `<prefix>articles/x/index.html`
//!   (the CDN maps the directory URL to it: a CloudFront Function on AWS,
//!   the bucket's main page suffix on Google Cloud);
//! * `Cache-Control`: HTML `public, max-age=60` (`html_max_age`) so a
//!   missed invalidation heals within a minute; fingerprinted files
//!   (`_engine/tagbrush-2.0.1.min.css`, `app.3f2a9c1d.js`) and media a year
//!   and `immutable`; other files `asset_max_age` (an hour);
//! * the `ETag` is the bucket's (MD5 of the body for single-part uploads);
//!   uploads send `Content-MD5`, so the store rejects a corrupted body.

use std::path::Path;

use crate::config::{IndexPaths, S3Config};

/// Keys under this (relative to the prefix) belong to the media library:
/// `engine build` never deletes them.
pub const MEDIA_PREFIX: &str = "media/";

pub const IMMUTABLE: &str = "public, max-age=31536000, immutable";

/// `Content-Type` of a key (by extension).
pub fn content_type(rel: &str) -> &'static str {
    engine_core::serve::content_type(Path::new(rel))
}

fn is_hex_hash(s: &str) -> bool {
    // Both a digit and a letter, so dates (`20240101`) and words
    // (`deadbeef`) do not count.
    s.len() >= 8
        && s.bytes().all(|b| b.is_ascii_hexdigit())
        && s.bytes().any(|b| b.is_ascii_digit())
        && s.bytes().any(|b| b.is_ascii_alphabetic())
}

/// Whether a file name carries its content's version: a hash segment of 8+
/// hex characters (`app.3f2a9c1d.js`, `font-9b1c0e7a.woff2`), or a version
/// number in the engine's own assets (`_engine/tagbrush-2.0.1.min.css`).
pub fn is_fingerprinted(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name);
    if stem.split(['.', '-', '_']).any(is_hex_hash) {
        return true;
    }
    rel.starts_with("_engine/")
        && !rel.starts_with("_engine/licenses/")
        && name.split('-').skip(1).any(|p| {
            p.split('.')
                .next()
                .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        })
}

/// `Cache-Control` for a key relative to the prefix.
pub fn cache_control(cfg: &S3Config, rel: &str) -> String {
    if rel.starts_with(MEDIA_PREFIX) || is_fingerprinted(rel) {
        IMMUTABLE.to_string()
    } else if content_type(rel).starts_with("text/html") {
        format!("public, max-age={}", cfg.html_max_age)
    } else {
        format!("public, max-age={}", cfg.asset_max_age)
    }
}

/// The URL path an object is served at: `articles/x/index.html` →
/// `/articles/x/`, `index.html` → `/`, anything else `/<rel>`.
pub fn url_path(rel: &str) -> String {
    if rel == "index.html" {
        "/".into()
    } else if let Some(dir) = rel.strip_suffix("/index.html") {
        format!("/{dir}/")
    } else {
        format!("/{rel}")
    }
}

/// The paths to invalidate for changed URL paths, per `index_paths`, in
/// order and without duplicates; one `/*` when there are more than
/// `max_paths`.
pub fn invalidation_paths(
    changed: &[String],
    index_paths: IndexPaths,
    max_paths: usize,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in changed {
        let forms: Vec<String> = if p.ends_with('/') {
            match index_paths {
                IndexPaths::Both => vec![p.clone(), format!("{p}index.html")],
                IndexPaths::Directory => vec![p.clone()],
                IndexPaths::File => vec![format!("{p}index.html")],
            }
        } else {
            vec![p.clone()]
        };
        for f in forms {
            if !out.contains(&f) {
                out.push(f);
            }
        }
    }
    if out.len() > max_paths {
        return vec!["/*".into()];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints() {
        assert!(is_fingerprinted("_engine/tagbrush-2.0.1.min.css"));
        assert!(is_fingerprinted("_engine/tagaction-0.2.0.min.js"));
        assert!(!is_fingerprinted("_engine/theme.css"));
        assert!(!is_fingerprinted("_engine/licenses/tagbrush-LICENSE.txt"));
        assert!(is_fingerprinted("js/app.3f2a9c1d.js"));
        assert!(is_fingerprinted("fonts/inter-9b1c0e7a.woff2"));
        assert!(!is_fingerprinted("css/site.css"));
        assert!(!is_fingerprinted("img/deadbeefcafe.png"));
        assert!(!is_fingerprinted("img/photo-20240101.jpg"));
        assert!(!is_fingerprinted("articles/x/index.html"));
    }

    #[test]
    fn url_paths_and_invalidation() {
        assert_eq!(url_path("index.html"), "/");
        assert_eq!(url_path("articles/x/index.html"), "/articles/x/");
        assert_eq!(url_path("404.html"), "/404.html");
        let changed = vec!["/".to_string(), "/a/".into(), "/sitemap.xml".into()];
        assert_eq!(
            invalidation_paths(&changed, IndexPaths::Both, 100),
            ["/", "/index.html", "/a/", "/a/index.html", "/sitemap.xml"]
        );
        assert_eq!(
            invalidation_paths(&changed, IndexPaths::Directory, 100),
            ["/", "/a/", "/sitemap.xml"]
        );
        assert_eq!(
            invalidation_paths(&changed, IndexPaths::File, 100),
            ["/index.html", "/a/index.html", "/sitemap.xml"]
        );
        assert_eq!(invalidation_paths(&changed, IndexPaths::Both, 4), ["/*"]);
    }
}
