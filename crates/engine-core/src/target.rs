//! Publish targets: where `build` and `publish` output goes.
//!
//! [`DiskTarget`] (the default, `[publish] target = "disk"`) writes into the
//! site's output folder. Every write goes to a hidden temporary file in the
//! same folder and is then renamed over the old file, so a reader (the
//! built-in server, Caddy, nginx) sees the old page or the new one, never a
//! partial one. Files are not fsynced: the rename is what readers need, and
//! after a power loss `engine rebuild` regenerates every page from the
//! database (an fsync per page made a 10-page publish take seconds).
//!
//! The S3 bucket target (`[publish] target = "s3"`, the `s3` feature,
//! crate engine-s3) implements the same trait: `put` uploads the object
//! whole, `delete` removes it, and `finish_batch` invalidates the changed
//! paths at the CDN. The trait lives in core so a static site (no
//! `publish` feature) can use the bucket target from `engine build`.

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail};

use crate::precompress;

pub trait PublishTarget: Send + Sync {
    /// Short name for messages (`disk`, `s3`).
    fn name(&self) -> &str;

    /// Replace the file at `rel` (e.g. `articles/hello/index.html`) whole.
    fn put(&self, rel: &Path, body: &[u8]) -> Result<()>;

    /// Replace several files (each as [`Self::put`]); targets may do it
    /// in parallel.
    fn put_many(&self, files: &[(PathBuf, Vec<u8>)]) -> Result<()> {
        for (rel, body) in files {
            self.put(rel, body)?;
        }
        Ok(())
    }

    /// Remove the file at `rel` (no error if it is already gone).
    fn delete(&self, rel: &Path) -> Result<()>;

    /// Whether a file exists at `rel`.
    fn exists(&self, rel: &Path) -> bool;

    /// Whether a re-rendered page whose bytes equal its last published
    /// version (the tag index's `sha256`) is left alone: not written, not
    /// in the batch's changed paths. Remote targets say yes (an upload and
    /// a CDN invalidation cost time and money); the disk target rewrites.
    fn skip_unchanged(&self) -> bool {
        false
    }

    /// For targets that skip unchanged pages: whether `body` is exactly
    /// what is published at `rel` (used for `build` routes, which the tag
    /// index does not hold).
    fn is_current(&self, _rel: &Path, _body: &[u8]) -> bool {
        false
    }

    /// Whether `engine serve` may render a missing publish page when it is
    /// requested. A bucket behind a CDN never asks the server, so remote
    /// targets say no and `engine build` pre-renders every page.
    fn render_on_miss(&self) -> bool {
        true
    }

    /// Called after each publish batch with the URL paths it wrote or
    /// deleted (a CDN target invalidates them here).
    fn finish_batch(&self, _changed_paths: &[String]) -> Result<()> {
        Ok(())
    }
}

/// Reject absolute paths and `..` (route paths are already checked; this is
/// the last line of defence before touching the disk).
pub fn check_rel(rel: &Path) -> Result<()> {
    let ok = !rel.as_os_str().is_empty()
        && rel.components().all(|c| match c {
            Component::Normal(s) => !s.to_string_lossy().starts_with('.'),
            _ => false,
        });
    if !ok {
        bail!("refusing to publish to unsafe path {}", rel.display());
    }
    Ok(())
}

/// Files in a local folder, written atomically.
pub struct DiskTarget {
    root: PathBuf,
}

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

impl DiskTarget {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        DiskTarget { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether `rel` already holds exactly `body` with the copies a write
    /// of it leaves (nothing to do).
    pub fn is_unchanged(&self, rel: &Path, body: &[u8]) -> bool {
        check_rel(rel).is_ok() && unchanged_with_copies(&self.root.join(rel), body)
    }

    /// [`PublishTarget::put`] with the copies already made (a bucket target
    /// uploads the same copies and mirrors them here).
    pub fn put_with(
        &self,
        rel: &Path,
        body: &[u8],
        copies: &[(precompress::Encoding, Option<Vec<u8>>)],
    ) -> Result<()> {
        check_rel(rel)?;
        let path = self.root.join(rel);
        let dir = path.parent().expect("joined path has a parent");
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let mut tmps: Vec<(PathBuf, PathBuf)> = Vec::new();
        let result = (|| -> Result<()> {
            // 1. Bytes first, copies before the file, so no copy is ever
            //    newer than the file it was made from.
            for (e, copy) in copies {
                if let Some(c) = copy {
                    let dest = e.sidecar(&path);
                    let tmp = write_tmp(dir, &dest, c)?;
                    tmps.push((tmp, dest));
                }
            }
            let tmp = write_tmp(dir, &path, body)?;
            // 2. Copies into place (or stale ones removed), then the file:
            //    the file's rename is the moment the new version is live.
            for (e, copy) in copies {
                if copy.is_none() {
                    remove_if_exists(&e.sidecar(&path))?;
                }
            }
            for (tmp, dest) in tmps.drain(..) {
                if let Err(err) = std::fs::rename(&tmp, &dest) {
                    let _ = std::fs::remove_file(&tmp);
                    return Err(err.into());
                }
            }
            tmps.push((tmp.clone(), path.clone()));
            std::fs::rename(&tmp, &path)?;
            tmps.clear();
            Ok(())
        })();
        for (tmp, _) in &tmps {
            let _ = std::fs::remove_file(tmp);
        }
        result.with_context(|| format!("writing {}", path.display()))
    }
}

impl PublishTarget for DiskTarget {
    fn name(&self) -> &str {
        "disk"
    }

    fn put(&self, rel: &Path, body: &[u8]) -> Result<()> {
        check_rel(rel)?;
        if unchanged_with_copies(&self.root.join(rel), body) {
            return Ok(());
        }
        self.put_with(rel, body, &precompress::encode_all(rel, body))
    }

    /// Compressing is most of the cost of a write, so several files are
    /// written at once: one thread per CPU this process may use, at most
    /// one per file.
    fn put_many(&self, files: &[(PathBuf, Vec<u8>)]) -> Result<()> {
        let threads = crate::resources::get().cpus.min(files.len()).max(1);
        if threads == 1 {
            for (rel, body) in files {
                self.put(rel, body)?;
            }
            return Ok(());
        }
        let next = std::sync::atomic::AtomicUsize::new(0);
        let results: Vec<Result<()>> = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..threads)
                .map(|_| {
                    scope.spawn(|| -> Result<()> {
                        loop {
                            let i = next.fetch_add(1, Ordering::Relaxed);
                            let Some((rel, body)) = files.get(i) else {
                                return Ok(());
                            };
                            self.put(rel, body)?;
                        }
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|w| w.join().expect("a write thread panicked"))
                .collect()
        });
        results.into_iter().collect()
    }

    fn delete(&self, rel: &Path) -> Result<()> {
        check_rel(rel)?;
        let path = self.root.join(rel);
        // The copies go first: a copy never outlives its file.
        for e in precompress::Encoding::ALL {
            remove_if_exists(&e.sidecar(&path))
                .with_context(|| format!("deleting {}", e.sidecar(&path).display()))?;
        }
        remove_if_exists(&path).with_context(|| format!("deleting {}", path.display()))?;
        // Remove folders the page leaves empty (`articles/old-slug/`).
        let mut dir = path.parent();
        while let Some(d) = dir {
            if d == self.root || !d.starts_with(&self.root) {
                break;
            }
            if std::fs::remove_dir(d).is_err() {
                break;
            }
            dir = d.parent();
        }
        Ok(())
    }

    fn exists(&self, rel: &Path) -> bool {
        check_rel(rel).is_ok() && self.root.join(rel).is_file()
    }
}

/// Write `body` to a hidden temporary file beside `dest`; returns its path.
fn write_tmp(dir: &Path, dest: &Path, body: &[u8]) -> Result<PathBuf> {
    use std::io::Write;
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let tmp = dir.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let r = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(body)
    })();
    if let Err(e) = r {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(tmp)
}

fn remove_if_exists(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Whether `path` already holds exactly `body` and its copies are the ones
/// a complete write of it leaves: each present only where one is due, and
/// none newer than the file (a newer copy is from a write cut short).
fn unchanged_with_copies(path: &Path, body: &[u8]) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if meta.len() != body.len() as u64 || std::fs::read(path).ok().as_deref() != Some(body) {
        return false;
    }
    let due = precompress::compressible(path) && body.len() >= precompress::MIN_BYTES;
    precompress::Encoding::ALL.iter().all(|e| {
        let side = e.sidecar(path);
        match std::fs::symlink_metadata(&side) {
            Err(_) => true, // absent: either not due or no gain; both fine
            Ok(_) if !due => false,
            Ok(_) => precompress::fresh(&meta, &side).is_some(),
        }
    })
}

/// `[publish] target`: where pages go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TargetKind {
    /// The site's output folder (the default).
    #[default]
    Disk,
    /// An S3-compatible bucket (`s3` feature).
    S3,
}

/// Read `[publish] target` from the raw section: `"disk"` (the default) or
/// `"s3"`. The other settings belong to the target that uses them.
pub fn target_kind(section: Option<&toml::Table>) -> Result<TargetKind> {
    match section.and_then(|t| t.get("target")) {
        None => Ok(TargetKind::Disk),
        Some(toml::Value::String(s)) if s == "disk" => Ok(TargetKind::Disk),
        Some(toml::Value::String(s)) if s == "s3" => Ok(TargetKind::S3),
        Some(_) => bail!("[publish] target must be \"disk\" or \"s3\""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_kinds() {
        assert_eq!(target_kind(None).unwrap(), TargetKind::Disk);
        let t: toml::Table = toml::from_str("target = 's3'").unwrap();
        assert_eq!(target_kind(Some(&t)).unwrap(), TargetKind::S3);
        let t: toml::Table = toml::from_str("target = 'ftp'").unwrap();
        assert!(target_kind(Some(&t)).is_err());
    }

    #[test]
    fn writes_atomically_and_cleans_up() {
        let d = tempfile::tempdir().unwrap();
        let t = DiskTarget::new(d.path());
        t.put(Path::new("a/b/index.html"), b"one").unwrap();
        t.put(Path::new("a/b/index.html"), b"two").unwrap();
        assert_eq!(
            std::fs::read(d.path().join("a/b/index.html")).unwrap(),
            b"two"
        );
        let leftovers: Vec<_> = std::fs::read_dir(d.path().join("a/b"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers.len(), 1, "no temp files left: {leftovers:?}");
        assert!(t.exists(Path::new("a/b/index.html")));
        t.delete(Path::new("a/b/index.html")).unwrap();
        assert!(!d.path().join("a").exists(), "empty folders removed");
        t.delete(Path::new("a/b/index.html")).unwrap();
        for bad in ["../x", "/etc/passwd", "a/../../x", ".hidden/x", ""] {
            assert!(t.put(Path::new(bad), b"x").is_err(), "{bad}");
        }
    }

    fn get_with(addr: std::net::SocketAddr, target: &str, extra: &str) -> (String, Vec<u8>) {
        use std::io::{Read, Write};
        let mut s = std::net::TcpStream::connect(addr).unwrap();
        write!(
            s,
            "GET {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n{extra}\r\n"
        )
        .unwrap();
        let mut out = Vec::new();
        s.read_to_end(&mut out).unwrap();
        let at = out.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        (
            String::from_utf8_lossy(&out[..at]).to_ascii_lowercase(),
            out[at + 4..].to_vec(),
        )
    }

    fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
        head.lines()
            .find_map(|l| l.strip_prefix(&format!("{name}: ")))
            .map(str::trim)
    }

    fn decode(enc: Option<&str>, body: &[u8]) -> String {
        use std::io::Read;
        let mut out = String::new();
        match enc {
            Some("br") => {
                brotli::Decompressor::new(body, 4096)
                    .read_to_string(&mut out)
                    .unwrap();
            }
            Some("gzip") => {
                flate2::read::GzDecoder::new(body)
                    .read_to_string(&mut out)
                    .unwrap();
            }
            _ => out = String::from_utf8(body.to_vec()).unwrap(),
        }
        out
    }

    /// Pre-compressed copies through a publish target and the engine's own
    /// server: the right body and headers per encoding, and never a stale
    /// copy after an edit, a shrink below the threshold, a delete or a
    /// write cut short.
    #[tokio::test(flavor = "multi_thread")]
    async fn compressed_copies_follow_every_write_and_are_never_stale() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("public");
        std::fs::create_dir_all(&root).unwrap();
        let t = DiskTarget::new(&root);
        let page = Path::new("a/index.html");
        let v1 = format!(
            "<html><body>{}</body></html>",
            "<p>first version</p>".repeat(100)
        );
        t.put(page, v1.as_bytes()).unwrap();
        assert!(root.join("a/index.html.br").is_file() && root.join("a/index.html.gz").is_file());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = crate::serve::router(crate::serve::ServeConfig {
            root: root.clone(),
            ..Default::default()
        })
        .unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let get = move |extra: &'static str| {
            let extra = extra.to_string();
            async move {
                tokio::task::spawn_blocking(move || get_with(addr, "/a/", &extra))
                    .await
                    .unwrap()
            }
        };

        // Headers and bodies per encoding.
        let mut etags = Vec::new();
        for (accept, enc) in [
            ("Accept-Encoding: gzip, deflate, br\r\n", Some("br")),
            ("Accept-Encoding: gzip\r\n", Some("gzip")),
            ("Accept-Encoding: br;q=0, gzip;q=0.5\r\n", Some("gzip")),
            ("", None),
            ("Accept-Encoding: identity\r\n", None),
        ] {
            let (head, body) = get(accept).await;
            assert!(head.starts_with("http/1.1 200"), "{head}");
            assert_eq!(header(&head, "content-encoding"), enc, "{accept}: {head}");
            assert_eq!(
                header(&head, "content-type"),
                Some("text/html; charset=utf-8")
            );
            assert_eq!(header(&head, "vary"), Some("accept-encoding"));
            assert_eq!(header(&head, "cache-control"), Some("no-cache"));
            assert_eq!(decode(enc, &body), v1, "{accept}");
            etags.push(header(&head, "etag").unwrap().to_string());
        }
        assert!(
            etags[0] != etags[1] && etags[1] != etags[3],
            "an ETag per encoding: {etags:?}"
        );
        assert_eq!(etags[1], etags[2]);
        // Revalidation answers 304 for the same version and encoding.
        let inm: &'static str = Box::leak(
            format!("Accept-Encoding: br\r\nIf-None-Match: {}\r\n", etags[0]).into_boxed_str(),
        );
        let (head, body) = get(inm).await;
        assert!(head.starts_with("http/1.1 304"), "{head}");
        assert!(body.is_empty());
        // The copies are never served by their own names.
        let (head, _) = tokio::task::spawn_blocking(move || get_with(addr, "/a/index.html.br", ""))
            .await
            .unwrap();
        assert!(head.starts_with("http/1.1 404"), "{head}");

        // An edit: every encoding serves the new bytes, with new ETags.
        let v2 = v1.replace("first", "second");
        t.put(page, v2.as_bytes()).unwrap();
        for (accept, enc) in [
            ("Accept-Encoding: br\r\n", Some("br")),
            ("Accept-Encoding: gzip\r\n", Some("gzip")),
            ("", None),
        ] {
            let (head, body) = get(accept).await;
            assert_eq!(decode(enc, &body), v2, "{accept}");
            assert!(!etags.contains(&header(&head, "etag").unwrap().to_string()));
        }
        let (head, _) = get(inm).await;
        assert!(
            head.starts_with("http/1.1 200"),
            "old ETag no longer matches: {head}"
        );

        // Shrunk below the threshold: the copies go, identity is served.
        t.put(page, b"<p>tiny</p>").unwrap();
        assert!(!root.join("a/index.html.br").exists() && !root.join("a/index.html.gz").exists());
        let (head, body) = get("Accept-Encoding: br, gzip\r\n").await;
        assert_eq!(header(&head, "content-encoding"), None);
        assert_eq!(body, b"<p>tiny</p>");

        // A write cut short (copies renamed, file not): the newer copy is
        // not served, and the next write of the same bytes heals it.
        t.put(page, v2.as_bytes()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(
            root.join("a/index.html.br"),
            crate::precompress::Encoding::Br.encode(b"<p>never published</p>"),
        )
        .unwrap();
        let (head, body) = get("Accept-Encoding: br\r\n").await;
        assert_ne!(header(&head, "content-encoding"), Some("br"), "{head}");
        assert_eq!(decode(header(&head, "content-encoding"), &body), v2);
        t.put(page, v2.as_bytes()).unwrap();
        let (head, body) = get("Accept-Encoding: br\r\n").await;
        assert_eq!(header(&head, "content-encoding"), Some("br"));
        assert_eq!(decode(Some("br"), &body), v2);

        // A copy that is a symlink is never followed.
        #[cfg(unix)]
        {
            std::fs::write(d.path().join("secret"), "TOP SECRET").unwrap();
            std::fs::remove_file(root.join("a/index.html.gz")).unwrap();
            std::os::unix::fs::symlink(d.path().join("secret"), root.join("a/index.html.gz"))
                .unwrap();
            let (head, body) = get("Accept-Encoding: gzip\r\n").await;
            assert_eq!(header(&head, "content-encoding"), None, "{head}");
            assert_eq!(decode(None, &body), v2);
        }

        // A delete takes the copies with it.
        t.delete(page).unwrap();
        assert!(
            !root.join("a").exists(),
            "{:?}",
            std::fs::read_dir(&root).unwrap().count()
        );
        let (head, _) = get("Accept-Encoding: br\r\n").await;
        assert!(head.starts_with("http/1.1 404"), "{head}");
    }

    #[cfg(unix)]
    #[test]
    fn identical_bytes_are_not_rewritten() {
        let d = tempfile::tempdir().unwrap();
        let t = DiskTarget::new(d.path());
        let body = "<p>same</p>".repeat(100);
        t.put(Path::new("x.html"), body.as_bytes()).unwrap();
        let ino = |p: &str| {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(d.path().join(p)).unwrap().ino()
        };
        let before = (ino("x.html"), ino("x.html.br"));
        t.put(Path::new("x.html"), body.as_bytes()).unwrap();
        assert_eq!((ino("x.html"), ino("x.html.br")), before);
        t.put(Path::new("x.html"), (body.clone() + " ").as_bytes())
            .unwrap();
        assert_ne!(ino("x.html"), before.0);
    }
}
