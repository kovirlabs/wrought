//! Hammerwright's `media` feature: uploads, image variants on disk, the
//! `media()` template function and the public `/media/` route.
//!
//! **Safety rules**, all enforced here so every caller (admin library, form
//! file fields, API) gets them:
//!
//! * the type is decided by the file's *content* (magic bytes), never by
//!   its name or the browser's `Content-Type`; only JPEG, PNG, GIF, WebP and
//!   PDF are accepted. SVG, HTML, XML and anything text-like are refused
//!   (an SVG can carry script), as is every other format;
//! * a size limit (`max_mb`) and an image pixel limit (`max_megapixels`,
//!   against decompression bombs) apply before anything is decoded;
//! * stored names are made by the engine: `<dir>/<public|private>/<id>/
//!   <slug>.<ext>`, the slug from the uploaded name (lowercase letters,
//!   digits, `-`), the extension from the sniffed type. No part of a path
//!   comes from the request, so `../` and friends have nowhere to go;
//! * JPEG, PNG and WebP are decoded and re-encoded, which drops EXIF (GPS,
//!   camera serials), XMP, ICC and comments; the EXIF orientation is applied
//!   first. GIFs keep their bytes (re-encoding would lose animation; GIF has
//!   no EXIF). PDFs are stored as uploaded and always downloaded, never shown
//!   inline;
//! * files are served with `X-Content-Type-Options: nosniff` and a
//!   `sandbox` Content-Security-Policy, so even a file a browser might
//!   misread cannot run script on the site's origin. Private files (form
//!   attachments) are only reachable through the admin.
//!
//! Resized copies (`widths`, default 320 to 1920 px) are written at upload
//! time next to the original, for `srcset`.
//!
//! `[media]` in `site.toml` (all optional, see [`MediaConfig`]):
//!
//! ```toml
//! [media]
//! dir = "media"          # relative to the site folder; keep it out of git
//! max_mb = 10
//! max_megapixels = 40
//! widths = [320, 640, 960, 1280, 1920]
//! jpeg_quality = 82
//! ```

pub mod ext;
pub mod process;
pub mod sniff;
pub mod sql;
pub mod storage;
pub mod store;
pub mod upload;
pub mod web;

use std::path::{Path, PathBuf};

use anyhow::Result;
use engine_db::Migration;
use serde::Deserialize;

pub use ext::{ImageData, MediaExtension};
pub use sniff::Kind;
pub use storage::PublicStorage;
pub use store::{Media, MediaError, MediaStore, Upload};

/// The `media` feature's schema.
pub fn migrations() -> Vec<Migration> {
    vec![Migration::new(
        "media/0001_media",
        include_str!("../migrations/postgres/0001_media.sql"),
    )]
}

/// With `content`: media changes feed the change log, so pages showing an
/// image are re-rendered when its text changes or it is deleted.
pub fn change_log_migrations() -> Vec<Migration> {
    vec![Migration::new(
        "media/0002_change_log",
        "CREATE TRIGGER engine_media_changes
                AFTER INSERT OR UPDATE OR DELETE ON engine_media
                FOR EACH ROW EXECUTE FUNCTION engine_record_change('media');",
    )]
}

fn d_dir() -> String {
    "media".into()
}
fn d_max_mb() -> u64 {
    10
}
fn d_megapixels() -> u64 {
    40
}
fn d_widths() -> Vec<u32> {
    vec![320, 640, 960, 1280, 1920]
}
fn d_quality() -> u8 {
    82
}

/// `[media]` settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaConfig {
    /// Where files are kept, relative to the site folder (or absolute).
    #[serde(default = "d_dir")]
    pub dir: String,
    /// Largest accepted upload.
    #[serde(default = "d_max_mb")]
    pub max_mb: u64,
    /// Largest accepted image, in millions of pixels.
    #[serde(default = "d_megapixels")]
    pub max_megapixels: u64,
    /// Widths of the resized copies (only those smaller than the original
    /// are made).
    #[serde(default = "d_widths")]
    pub widths: Vec<u32>,
    #[serde(default = "d_quality")]
    pub jpeg_quality: u8,
}

impl Default for MediaConfig {
    fn default() -> Self {
        toml::from_str("").expect("defaults")
    }
}

impl MediaConfig {
    pub fn from_site(section: Option<&toml::Table>) -> Result<Self> {
        let cfg: MediaConfig = match section {
            Some(t) => t
                .clone()
                .try_into()
                .map_err(|e| anyhow::anyhow!("[media]: {e}"))?,
            None => MediaConfig::default(),
        };
        if cfg.max_mb == 0 || cfg.max_mb > 1024 {
            anyhow::bail!("[media] max_mb must be between 1 and 1024");
        }
        if cfg.max_megapixels == 0 || cfg.max_megapixels > 500 {
            anyhow::bail!("[media] max_megapixels must be between 1 and 500");
        }
        if cfg.widths.iter().any(|w| !(16..=8192).contains(w)) || cfg.widths.len() > 12 {
            anyhow::bail!("[media] widths must be 16 to 8192 pixels, at most 12 of them");
        }
        if !(30..=100).contains(&cfg.jpeg_quality) {
            anyhow::bail!("[media] jpeg_quality must be between 30 and 100");
        }
        if cfg.dir.trim().is_empty() {
            anyhow::bail!("[media] dir must not be empty");
        }
        Ok(cfg)
    }

    pub fn max_bytes(&self) -> u64 {
        self.max_mb * 1024 * 1024
    }

    /// The media folder for a site.
    pub fn dir_for(&self, site: &Path) -> PathBuf {
        let d = Path::new(&self.dir);
        if d.is_absolute() {
            d.to_path_buf()
        } else {
            site.join(d)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_and_limits() {
        let c = MediaConfig::from_site(None).unwrap();
        assert_eq!(c.max_bytes(), 10 * 1024 * 1024);
        assert_eq!(c.widths, [320, 640, 960, 1280, 1920]);
        let t: toml::Table = toml::from_str("max_mb = 0").unwrap();
        assert!(MediaConfig::from_site(Some(&t)).is_err());
        let t: toml::Table = toml::from_str("widths = [5]").unwrap();
        assert!(MediaConfig::from_site(Some(&t)).is_err());
        let t: toml::Table = toml::from_str("typo = 1").unwrap();
        assert!(MediaConfig::from_site(Some(&t)).is_err());
        assert_eq!(
            c.dir_for(Path::new("/srv/site")),
            PathBuf::from("/srv/site/media")
        );
    }
}
