//! Object keys, headers and URL paths.
//!
//! * a page `/articles/x/` is the object `<prefix>articles/x/index.html`
//!   (the CDN maps the directory URL to it: a CloudFront Function on AWS,
//!   the bucket's main page suffix on Google Cloud);
//! * `Cache-Control`: HTML `public, max-age=60` (`html_max_age`) so a
//!   missed invalidation heals within a minute; fingerprinted files
//!   (`engine_core::assets::is_fingerprinted`: `app.3f2a9c1d.js`, the
//!   engine's own `_engine/tagbrush-2.0.1.min.<hash>.css`) and media a year
//!   and `immutable`; other files `asset_max_age` (an hour);
//! * the `ETag` is the bucket's (MD5 of the body for single-part uploads);
//!   uploads send `Content-MD5`, so the store rejects a corrupted body.

use std::path::Path;

use crate::config::{IndexPaths, S3Config};

/// Keys under this (relative to the prefix) belong to the media library:
/// `engine build` never deletes them.
pub const MEDIA_PREFIX: &str = "media/";

pub use engine_core::assets::{IMMUTABLE, is_fingerprinted};

/// `Content-Type` of a key (by extension).
pub fn content_type(rel: &str) -> &'static str {
    engine_core::serve::content_type(Path::new(rel))
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
        // engine_core::assets::is_fingerprinted, the rule `engine serve` and
        // the Caddyfiles use too.
        assert!(is_fingerprinted(
            "_engine/tagbrush-2.0.1.min.3f2a9c1d0b.css"
        ));
        assert!(!is_fingerprinted("_engine/tagbrush-2.0.1.min.css"));
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
