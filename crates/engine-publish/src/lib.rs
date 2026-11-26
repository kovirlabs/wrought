//! Wrought's `publish` feature: pages rendered when content changes.
//!
//! * [`index`]: the persistent tag index (path → cache tags) and the cursor
//!   into the change log, in the site's database;
//! * [`target`]: where files go ([`target::DiskTarget`], atomic writes);
//! * [`publisher`]: turns changes into re-rendered pages, `engine rebuild`,
//!   and render-on-miss;
//! * [`listener`]: the Postgres NOTIFY listener `engine serve` runs.
//!
//! `[publish]` in `site.toml` (see [`PublishConfig`]):
//!
//! ```toml
//! [publish]
//! target = "disk"   # the default; "s3" is planned
//! ```

pub mod index;
pub mod listener;
pub mod publisher;
pub mod site;
pub mod target;

use anyhow::{Result, bail};
use engine_db::{Backend, Migration};
use serde::Deserialize;

pub use index::TagIndex;
pub use publisher::{PublishReport, Publisher, index_build};
pub use site::CmsSite;
pub use target::{DiskTarget, PublishTarget};

/// The `publish` feature's own schema (tag index, cursor).
pub fn migrations(backend: Backend) -> Vec<Migration> {
    match backend {
        Backend::Postgres => vec![Migration::new(
            "publish/0001_tag_index",
            include_str!("../migrations/postgres/0001_tag_index.sql"),
        )],
        Backend::Sqlite => Vec::new(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TargetKind {
    #[default]
    Disk,
    S3,
}

/// `[publish]` settings.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishConfig {
    #[serde(default)]
    pub target: TargetKind,
    // Reserved for the S3 target (Bloomery spec): accepted so a config can
    // be written ahead, refused at use.
    #[serde(default)]
    pub bucket: Option<String>,
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
}

impl PublishConfig {
    pub fn from_site(section: Option<&toml::Table>) -> Result<Self> {
        let cfg: PublishConfig = match section {
            Some(t) => t
                .clone()
                .try_into()
                .map_err(|e| anyhow::anyhow!("[publish]: {e}"))?,
            None => PublishConfig::default(),
        };
        if cfg.target == TargetKind::S3 {
            bail!(
                "[publish] target = \"s3\" is not in this release (planned; see DECISIONS.md). \
                 Use target = \"disk\""
            );
        }
        if cfg.bucket.is_some()
            || cfg.prefix.is_some()
            || cfg.endpoint.is_some()
            || cfg.region.is_some()
        {
            bail!("[publish] bucket, prefix, endpoint and region only apply to target = \"s3\"");
        }
        Ok(cfg)
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
        let err = PublishConfig::from_site(Some(&t)).unwrap_err().to_string();
        assert!(err.contains("not in this release"), "{err}");
        let t: toml::Table = toml::from_str("target = 'ftp'").unwrap();
        assert!(PublishConfig::from_site(Some(&t)).is_err());
    }
}
