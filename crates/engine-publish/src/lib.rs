//! Wrought's `publish` feature: pages rendered when content changes.
//!
//! * [`index`]: the persistent tag index (path → cache tags) and the cursor
//!   into the change log, in the site's database;
//! * [`target`]: where files go ([`target::DiskTarget`], atomic writes; the
//!   bucket target of the `s3` feature lives in engine-s3);
//! * [`publisher`]: turns changes into re-rendered pages, `engine rebuild`,
//!   and render-on-miss;
//! * [`listener`]: the change listener `engine serve` runs (`LISTEN` on
//!   the triggers' `NOTIFY`).
//!
//! `[publish]` in `site.toml` (see [`PublishConfig`]):
//!
//! ```toml
//! [publish]
//! target = "disk"   # the default; "s3" with the `s3` feature (engine-s3)
//! ```

pub mod index;
pub mod listener;
pub mod publisher;
pub mod site;
pub mod sql;
pub mod target;

use anyhow::{Result, bail};
use engine_db::Migration;

pub use engine_core::target::TargetKind;
pub use index::TagIndex;
pub use publisher::{PublishReport, Publisher, index_build};
pub use site::CmsSite;
pub use target::{DiskTarget, PublishTarget};

/// The `publish` feature's own schema (tag index, cursor).
pub fn migrations() -> Vec<Migration> {
    vec![Migration::new(
        "publish/0001_tag_index",
        include_str!("../migrations/postgres/0001_tag_index.sql"),
    )]
}

/// `[publish]` settings as far as this crate is concerned: the target. The
/// bucket target's own settings (`bucket`, `prefix`, `endpoint`, `region`,
/// `[publish.cdn]`...) are parsed by engine-s3.
#[derive(Debug, Clone, Default)]
pub struct PublishConfig {
    pub target: TargetKind,
}

impl PublishConfig {
    pub fn from_site(section: Option<&toml::Table>) -> Result<Self> {
        let target = engine_core::target::target_kind(section)?;
        if target == TargetKind::Disk
            && let Some(t) = section
        {
            let other: Vec<&str> = t
                .keys()
                .map(String::as_str)
                .filter(|k| *k != "target")
                .collect();
            if !other.is_empty() {
                bail!(
                    "[publish] {} only apply to target = \"s3\"",
                    other.join(", ")
                );
            }
        }
        Ok(PublishConfig { target })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_config() {
        assert_eq!(
            PublishConfig::from_site(None).unwrap().target,
            TargetKind::Disk
        );
        let t: toml::Table = toml::from_str("target = 's3'\nbucket = 'b'").unwrap();
        assert_eq!(
            PublishConfig::from_site(Some(&t)).unwrap().target,
            TargetKind::S3
        );
        let t: toml::Table = toml::from_str("bucket = 'b'").unwrap();
        let err = PublishConfig::from_site(Some(&t)).unwrap_err().to_string();
        assert!(err.contains("only apply to target"), "{err}");
        let t: toml::Table = toml::from_str("target = 'ftp'").unwrap();
        assert!(PublishConfig::from_site(Some(&t)).is_err());
    }
}
