//! Content-hashed asset URLs and the caching rule that goes with them.
//!
//! A refresh must always be enough to see an edit, while browsers and CDNs
//! keep assets as long as they can. So:
//!
//! * every asset the engine emits (TagBrush, TagAction, the token
//!   stylesheet, component scripts and styles, and every file of a site's
//!   `static/` folder through the `asset()` template function) has a URL
//!   that carries a hash of its bytes, served
//!   `Cache-Control: public, max-age=31536000, immutable` ([`IMMUTABLE`]):
//!   changing a file changes its URL, so nothing is ever purged;
//! * everything else (HTML, feeds, sitemaps, files referenced by a
//!   hand-written path) is served `no-cache` with a strong ETag: the browser
//!   asks every time and gets a `304` when nothing changed.
//!
//! One rule decides which is which, [`is_fingerprinted`]: the name segment
//! just before the extension is 8 or more lowercase hex characters with at
//! least one digit and one letter (`site.3f2a9c1d0b.css`,
//! `video_embed-8c1e0f2a7d.js`). Dates (`photo-20240101.jpg`) and words
//! (`deadbeef.png`) do not count. The shipped Caddyfiles carry the same rule
//! as a regular expression; `engine serve` and the bucket target call this
//! function. Names the engine makes always pass it ([`content_hash`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

/// `Cache-Control` of a fingerprinted asset.
pub const IMMUTABLE: &str = "public, max-age=31536000, immutable";

/// `Cache-Control` of everything that is not fingerprinted: kept, but
/// revalidated on every use (a `304` from the ETag when unchanged).
pub const REVALIDATE: &str = "no-cache";

/// The hash an asset name carries: 10 lowercase hex characters of the
/// SHA-256 of `bytes` (40 bits: collisions between versions of one file do
/// not happen in practice). The first 10-character window that has both a
/// digit and a letter, so [`is_fingerprinted`] always recognises it.
pub fn content_hash(bytes: &[u8]) -> String {
    let hex = crate::builder::sha256_hex(bytes);
    (0..=hex.len() - 10)
        .map(|i| &hex[i..i + 10])
        .find(|w| is_hash_segment(w))
        // 64 hex characters without a 10-window holding both a digit and
        // a letter cannot happen for SHA-256 in practice; stay recognisable.
        .map_or_else(|| format!("{}a0", &hex[..8]), str::to_string)
}

/// `path` with [`content_hash`] of `bytes` inserted before the extension:
/// `/css/site.css` → `/css/site.3f2a9c1d0b.css`,
/// `/_engine/tagbrush-2.0.1.min.css` → `/_engine/tagbrush-2.0.1.min.3f2a9c1d0b.css`.
/// A name without an extension gets the hash appended (`LICENSE.3f2a9c1d0b`).
pub fn hashed_name(path: &str, bytes: &[u8]) -> String {
    let h = content_hash(bytes);
    let slash = path.rfind('/').map_or(0, |i| i + 1);
    match path[slash..].rfind('.') {
        Some(dot) if dot > 0 => {
            let at = slash + dot;
            format!("{}.{h}{}", &path[..at], &path[at..])
        }
        _ => format!("{path}.{h}"),
    }
}

fn is_hash_segment(s: &str) -> bool {
    s.len() >= 8
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && s.bytes().any(|b| b.is_ascii_digit())
        && s.bytes().any(|b| b.is_ascii_lowercase())
}

/// Whether a path's file name carries its content's hash (see the module
/// notes): the segment between the last `.` or `-` of the stem and the
/// extension is a hash. Works on URL paths and relative file paths.
pub fn is_fingerprinted(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let Some((stem, ext)) = name.rsplit_once('.') else {
        return false;
    };
    if ext.is_empty() || !ext.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return false;
    }
    match stem.rfind(['.', '-']) {
        Some(i) if i > 0 => is_hash_segment(&stem[i + 1..]),
        _ => false,
    }
}

/// The `Cache-Control` of a file served from the output folder.
pub fn cache_control(path: &str) -> &'static str {
    if is_fingerprinted(path) {
        IMMUTABLE
    } else {
        REVALIDATE
    }
}

/// Hashed URLs of a site's `static/` files, for the `asset()` template
/// function.
///
/// Which files exist, and which version of each, is fixed when the renderer
/// loads (a stat of each file, no reads); a file is read and hashed the first
/// time a render asks for it. The hashed twins on disk are written for that
/// same version (by the build, or by `engine serve` right after it loads a
/// renderer), so a page never links a twin that is not there. A file edited
/// after the load is linked by its plain path until the renderer is
/// replaced (`engine serve` does that on every `static/` edit): still
/// correct, only revalidated instead of kept.
#[derive(Debug, Default)]
pub struct StaticAssets {
    dir: Option<PathBuf>,
    files: HashMap<PathBuf, Stamp>,
    urls: RwLock<HashMap<String, String>>,
}

/// A file version: length, modification time, inode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<std::time::SystemTime>,
    ino: u64,
}

impl Stamp {
    fn of(m: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        let ino = std::os::unix::fs::MetadataExt::ino(m);
        #[cfg(not(unix))]
        let ino = 0;
        Stamp {
            len: m.len(),
            modified: m.modified().ok(),
            ino,
        }
    }
}

impl StaticAssets {
    /// The assets of `dir` (a site's `static/` folder; `None`: no folder).
    pub fn new(dir: Option<PathBuf>) -> Self {
        let mut files = HashMap::new();
        if let Some(root) = &dir {
            let mut stack = vec![root.clone()];
            while let Some(d) = stack.pop() {
                let Ok(rd) = std::fs::read_dir(&d) else {
                    continue;
                };
                for e in rd.flatten() {
                    if e.file_name().to_string_lossy().starts_with('.') {
                        continue;
                    }
                    let Ok(ft) = e.file_type() else { continue };
                    if ft.is_dir() {
                        stack.push(e.path());
                    } else if ft.is_file()
                        && let (Ok(m), Ok(rel)) = (e.metadata(), e.path().strip_prefix(root))
                    {
                        files.insert(rel.to_path_buf(), Stamp::of(&m));
                    }
                }
            }
        }
        StaticAssets {
            dir,
            files,
            urls: RwLock::default(),
        }
    }

    /// The `static/` folder, if the site has one.
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// The hashed URL of the site path `url` (`/css/site.css`), or an
    /// error naming the file that is missing.
    pub fn url(&self, url: &str) -> Result<String, String> {
        if let Some(u) = self.urls.read().unwrap_or_else(|p| p.into_inner()).get(url) {
            return Ok(u.clone());
        }
        let rel = static_rel(url)?;
        let Some(dir) = &self.dir else {
            return Err(format!("asset(\"{url}\"): the site has no static/ folder"));
        };
        let Some(stamp) = self.files.get(&rel) else {
            return Err(format!(
                "asset(\"{url}\"): no file static/{}",
                rel.display()
            ));
        };
        let file = dir.join(&rel);
        let bytes = std::fs::read(&file)
            .map_err(|e| format!("asset(\"{url}\"): reading static/{}: {e}", rel.display()))?;
        let same = bytes.len() as u64 == stamp.len
            && std::fs::metadata(&file).is_ok_and(|m| Stamp::of(&m) == *stamp);
        // Changed since the load: its twin may not be written yet.
        let out = if same {
            hashed_name(url, &bytes)
        } else {
            url.to_string()
        };
        self.urls
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .insert(url.to_string(), out.clone());
        Ok(out)
    }
}

/// The path under `static/` of a site URL path: absolute, no query or
/// fragment, no `.`/`..`/hidden segments, not under `/_engine/`.
fn static_rel(url: &str) -> Result<PathBuf, String> {
    let bad = |why: &str| Err(format!("asset(\"{url}\"): {why}"));
    let Some(rest) = url.strip_prefix('/') else {
        return bad("give the file's path in the site, starting with `/`");
    };
    if url.contains(['?', '#', '\\', '\0']) {
        return bad("a plain path, without a query or fragment");
    }
    let mut rel = PathBuf::new();
    for seg in rest.split('/') {
        if seg.is_empty() || seg.starts_with('.') {
            return bad("empty, `.`, `..` and hidden path segments are not allowed");
        }
        rel.push(seg);
    }
    if rel.starts_with("_engine") {
        return bad("/_engine/ files are the engine's own and already hashed");
    }
    Ok(rel)
}

/// The hashed twin of a `static/` file, relative to the output folder.
pub fn hashed_rel(rel: &Path, bytes: &[u8]) -> PathBuf {
    let url = format!("/{}", rel.to_string_lossy().replace('\\', "/"));
    PathBuf::from(hashed_name(&url, bytes).trim_start_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_names_are_always_recognised() {
        for i in 0..2000u32 {
            let body = i.to_le_bytes();
            let h = content_hash(&body);
            assert_eq!(h.len(), 10, "{h}");
            let name = hashed_name("/css/site.css", &body);
            assert!(is_fingerprinted(&name), "{name}");
            assert!(name.starts_with("/css/site.") && name.ends_with(".css"));
        }
        let same = hashed_name("/a.css", b"x");
        assert_eq!(same, hashed_name("/a.css", b"x"), "stable");
        assert_ne!(same, hashed_name("/a.css", b"y"), "changes with content");
    }

    #[test]
    fn hashed_names() {
        let h = content_hash(b"body");
        assert_eq!(
            hashed_name("/css/site.css", b"body"),
            format!("/css/site.{h}.css")
        );
        assert_eq!(
            hashed_name("/_engine/tagbrush-2.0.1.min.css", b"body"),
            format!("/_engine/tagbrush-2.0.1.min.{h}.css")
        );
        assert_eq!(hashed_name("/LICENSE", b"body"), format!("/LICENSE.{h}"));
        assert_eq!(hashed_name("/a.b/.env", b"body"), format!("/a.b/.env.{h}"));
        assert_eq!(
            hashed_rel(Path::new("fonts/inter.woff2"), b"body"),
            PathBuf::from(format!("fonts/inter.{h}.woff2"))
        );
    }

    #[test]
    fn fingerprint_rule() {
        assert!(is_fingerprinted("js/app.3f2a9c1d.js"));
        assert!(is_fingerprinted("/fonts/inter-9b1c0e7a.woff2"));
        assert!(is_fingerprinted(
            "/_engine/tagbrush-2.0.1.min.3f2a9c1d0b.css"
        ));
        assert!(is_fingerprinted(
            "/_engine/components/video_embed-8c1e0f2a7d.js"
        ));
        assert!(!is_fingerprinted("/_engine/tagbrush-2.0.1.min.css"));
        assert!(!is_fingerprinted("/_engine/theme.css"));
        assert!(!is_fingerprinted("css/site.css"));
        assert!(!is_fingerprinted("img/deadbeefcafe.png"), "no digit");
        assert!(!is_fingerprinted("img/photo-20240101.jpg"), "no letter");
        assert!(
            !is_fingerprinted("img/3f2a9c1d.png"),
            "no separator: the whole name"
        );
        assert!(!is_fingerprinted("app.3F2A9C1D.js"), "lowercase only");
        assert!(!is_fingerprinted("app.3f2a9c1.js"), "too short");
        assert!(
            !is_fingerprinted("app.3f2a9c1d.min.js"),
            "hash before the extension only"
        );
        assert!(!is_fingerprinted("articles/x/index.html"));
        assert!(!is_fingerprinted("/"));
        assert_eq!(cache_control("/a.3f2a9c1d0b.css"), IMMUTABLE);
        assert_eq!(cache_control("/a.css"), REVALIDATE);
    }

    #[test]
    fn static_urls() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("css")).unwrap();
        std::fs::write(d.path().join("css/site.css"), "a{}").unwrap();
        let a = StaticAssets::new(Some(d.path().to_path_buf()));
        let u = a.url("/css/site.css").unwrap();
        assert_eq!(u, hashed_name("/css/site.css", b"a{}"));
        // Cached: a change on disk is seen by the next renderer, not this one.
        std::fs::write(d.path().join("css/site.css"), "b{}").unwrap();
        assert_eq!(a.url("/css/site.css").unwrap(), u);
        // A file changed after the load, before its first use: the plain
        // path (its twin is written for the version seen at load).
        std::fs::write(d.path().join("css/late.css"), "a{}").unwrap();
        let b = StaticAssets::new(Some(d.path().to_path_buf()));
        std::fs::write(d.path().join("css/late.css"), "a{}b{}").unwrap();
        assert_eq!(b.url("/css/late.css").unwrap(), "/css/late.css");
        assert!(b.url("/css/new.css").is_err(), "not there at load");
        assert!(
            a.url("/css/missing.css")
                .unwrap_err()
                .contains("static/css/missing.css")
        );
        for bad in [
            "css/site.css",
            "/../x",
            "/a/./b",
            "/.env",
            "/x?v=1",
            "/_engine/x.css",
            "/a//b",
        ] {
            assert!(a.url(bad).is_err(), "{bad}");
        }
        assert!(StaticAssets::new(None).url("/a.css").is_err());
    }
}
