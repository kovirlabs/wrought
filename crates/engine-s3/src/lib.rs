//! Hammerwright's `s3` feature: publishing to an S3-compatible bucket served by
//! a CDN, with invalidation of exactly the changed paths.
//!
//! * [`config`]: `[publish] target = "s3"` and `[publish.cdn]`;
//! * [`creds`]: credentials from the environment or the machine (EC2/ECS
//!   role, GCE service account), never from `site.toml`;
//! * [`sigv4`]: AWS Signature Version 4, sans-IO (hmac + sha2);
//! * [`client`]: the S3 operations (put, delete, head, list) over reqwest;
//! * [`cdn`]: invalidation providers (CloudFront, Google Cloud CDN, a
//!   file, a log, none) behind the [`cdn::Cdn`] trait;
//! * [`sync`]: `engine build`: upload what differs, delete what is gone;
//! * [`target`]: the bucket as the publisher's
//!   [`engine_core::target::PublishTarget`].
//!
//! One S3 API covers AWS S3, Google Cloud Storage (XML API), Cloudflare
//! R2, Backblaze B2 and MinIO. Pages are stored as `<prefix><path>/index.html`
//! and media under `<prefix>media/`.

pub mod cdn;
pub mod client;
pub mod config;
pub mod creds;
pub mod endpoint;
pub mod paths;
pub mod sigv4;
pub mod sync;
pub mod target;
mod xml;

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;

pub use cdn::{Cdn, Invalidator};
pub use client::{Bucket, PutOptions};
pub use config::S3Config;
pub use creds::Credentials;
pub use sync::{SyncReport, sync};
pub use target::S3Target;

/// A configured bucket with its CDN. Cheap to clone.
#[derive(Clone)]
pub struct S3 {
    pub cfg: Arc<S3Config>,
    pub bucket: Bucket,
    pub cdn: Arc<Invalidator>,
}

impl S3 {
    /// From the site's `[publish]` section (`target = "s3"`).
    pub fn from_site(section: Option<&toml::Table>, site: &Path) -> Result<Self> {
        Self::new(S3Config::from_site(section, site)?)
    }

    pub fn new(cfg: S3Config) -> Result<Self> {
        let creds = Credentials::new(cfg.credentials, cfg.is_google())?;
        Self::with_credentials(cfg, creds)
    }

    /// With given credentials (tests).
    pub fn with_credentials(cfg: S3Config, creds: Credentials) -> Result<Self> {
        let creds = Arc::new(creds);
        let bucket = Bucket::new(&cfg, creds.clone())?;
        let cdn = Invalidator::new(&cfg.cdn, creds)?;
        Ok(S3 {
            cfg: Arc::new(cfg),
            bucket,
            cdn: Arc::new(cdn),
        })
    }

    /// With another CDN (tests).
    pub fn with_cdn(mut self, cdn: Invalidator) -> Self {
        self.cdn = Arc::new(cdn);
        self
    }

    /// `bucket/prefix` for messages.
    pub fn describe(&self) -> String {
        format!(
            "s3://{}/{} ({}, CDN: {})",
            self.cfg.bucket,
            self.cfg.prefix,
            self.cfg.endpoint,
            self.cdn.provider()
        )
    }

    /// Full key of a media file (`<id>/<file>`).
    pub fn media_key(&self, rel: &str) -> String {
        format!("{}{}{rel}", self.cfg.prefix, paths::MEDIA_PREFIX)
    }
}
