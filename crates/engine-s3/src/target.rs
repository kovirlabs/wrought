//! The bucket as a [`PublishTarget`] for the publisher (`publish` mode).
//!
//! Every page goes to the bucket first and then to the local output
//! folder, which stays an exact copy of what was published: `engine serve`
//! can still serve it to whoever reaches the origin directly, and it tells
//! whether a `build` route's new bytes differ from the published ones.
//! Pages whose bytes did not change are skipped (no upload, no
//! invalidation); missing pages are never rendered on request (a CDN in
//! front of a bucket never asks the server), so `engine build` pre-renders.
//!
//! The trait is synchronous (the publisher runs on a blocking thread);
//! requests run on the runtime handle given to [`S3Target::new`].

use std::path::Path;

use anyhow::{Result, bail};
use engine_core::target::{DiskTarget, PublishTarget, check_rel};

use crate::S3;
use crate::paths::MEDIA_PREFIX;
use crate::sync::put_options;

pub struct S3Target {
    s3: S3,
    mirror: DiskTarget,
    rt: tokio::runtime::Handle,
}

impl S3Target {
    /// `mirror` is the site's output folder.
    pub fn new(s3: S3, mirror: DiskTarget, rt: tokio::runtime::Handle) -> Self {
        S3Target { s3, mirror, rt }
    }

    fn key(&self, rel: &Path) -> Result<(String, String)> {
        check_rel(rel)?;
        let rel: String = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join("/");
        if rel.starts_with(MEDIA_PREFIX) {
            bail!("refusing to publish a page under {MEDIA_PREFIX} (the media library's keys)");
        }
        Ok((format!("{}{rel}", self.s3.cfg.prefix), rel))
    }
}

impl PublishTarget for S3Target {
    fn name(&self) -> &str {
        "s3"
    }

    fn put(&self, rel: &Path, body: &[u8]) -> Result<()> {
        let (key, short) = self.key(rel)?;
        let opts = put_options(&self.s3, &short);
        self.rt.block_on(self.s3.bucket.put(&key, body, &opts))?;
        self.mirror.put(rel, body)
    }

    fn delete(&self, rel: &Path) -> Result<()> {
        let (key, _) = self.key(rel)?;
        self.rt.block_on(self.s3.bucket.delete(&key))?;
        self.mirror.delete(rel)
    }

    fn exists(&self, rel: &Path) -> bool {
        self.mirror.exists(rel)
    }

    fn skip_unchanged(&self) -> bool {
        true
    }

    fn is_current(&self, rel: &Path, body: &[u8]) -> bool {
        check_rel(rel).is_ok()
            && std::fs::read(self.mirror.root().join(rel)).is_ok_and(|old| old == body)
    }

    fn render_on_miss(&self) -> bool {
        false
    }

    fn finish_batch(&self, changed_paths: &[String]) -> Result<()> {
        let sent = self.rt.block_on(self.s3.cdn.invalidate(changed_paths))?;
        if !sent.is_empty() {
            println!(
                "cdn ({}): invalidated {}",
                self.s3.cdn.provider(),
                sent.join(" ")
            );
        }
        Ok(())
    }
}
