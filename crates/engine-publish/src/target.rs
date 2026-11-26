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
//! An S3-compatible bucket target with CDN invalidation (see the Bloomery
//! deployment spec) implements the same trait: `put` uploads the object
//! whole, `delete` removes it, and `finish_batch` invalidates the changed
//! paths at the CDN. It is not in this release.

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail};

pub trait PublishTarget: Send + Sync {
    /// Short name for messages (`disk`, `s3`).
    fn name(&self) -> &str;

    /// Replace the file at `rel` (e.g. `articles/hello/index.html`) whole.
    fn put(&self, rel: &Path, body: &[u8]) -> Result<()>;

    /// Remove the file at `rel` (no error if it is already gone).
    fn delete(&self, rel: &Path) -> Result<()>;

    /// Whether a file exists at `rel`.
    fn exists(&self, rel: &Path) -> bool;

    /// Called after each publish batch with the URL paths it wrote or
    /// deleted (a CDN target invalidates them here).
    fn finish_batch(&self, _changed_paths: &[String]) -> Result<()> {
        Ok(())
    }
}

/// Reject absolute paths and `..` (route paths are already checked; this is
/// the last line of defence before touching the disk).
fn check_rel(rel: &Path) -> Result<()> {
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
}

impl PublishTarget for DiskTarget {
    fn name(&self) -> &str {
        "disk"
    }

    fn put(&self, rel: &Path, body: &[u8]) -> Result<()> {
        check_rel(rel)?;
        let path = self.root.join(rel);
        let dir = path.parent().expect("joined path has a parent");
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let tmp = dir.join(format!(
            ".{name}.{}.{}.tmp",
            std::process::id(),
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| -> Result<()> {
            use std::io::Write;
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(body)?;
            drop(f);
            std::fs::rename(&tmp, &path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result.with_context(|| format!("writing {}", path.display()))
    }

    fn delete(&self, rel: &Path) -> Result<()> {
        check_rel(rel)?;
        let path = self.root.join(rel);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("deleting {}", path.display())),
        }
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
