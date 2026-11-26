//! The `s3` feature in the commands: `engine build` syncs the built output
//! (and the media library's public files) to the bucket and invalidates
//! what changed; `engine serve` and `engine rebuild` publish through the
//! bucket target; media uploads are copied to the bucket.

use std::path::Path;

use anyhow::Result;
use engine_core::builder::{self, BuildOptions};
use engine_core::config::SiteConfig;
use engine_core::target::{TargetKind, target_kind};
use engine_s3::S3;

/// The site's bucket, if `[publish] target = "s3"`.
pub fn open(site: &Path, cfg: &SiteConfig) -> Result<Option<S3>> {
    if target_kind(cfg.publish.as_ref())? != TargetKind::S3 {
        return Ok(None);
    }
    Ok(Some(S3::from_site(cfg.publish.as_ref(), site)?))
}

/// The media library's public folder, if this build has media and the
/// folder exists.
#[allow(unused_variables)]
fn media_public_dir(site: &Path, cfg: &SiteConfig) -> Result<Option<std::path::PathBuf>> {
    #[cfg(feature = "media")]
    {
        let mc = engine_media::MediaConfig::from_site(cfg.media.as_ref())?;
        let dir = mc.dir_for(site).join("public");
        if dir.is_dir() {
            return Ok(Some(dir));
        }
    }
    Ok(None)
}

/// After `engine build`: make the bucket match the output folder.
pub fn sync_after_build(site: &Path, opts: &BuildOptions) -> Result<()> {
    let (cfg, _) = builder::load_config(site)?;
    let Some(s3) = open(site, &cfg)? else {
        return Ok(());
    };
    let output = builder::output_dir(site, &cfg, opts);
    let media = media_public_dir(site, &cfg)?;
    println!("Syncing to {}", s3.describe());
    let report = crate::runtime()?.block_on(engine_s3::sync(&s3, &output, media.as_deref()))?;
    for k in &report.uploaded {
        println!("  uploaded {k}");
    }
    for k in &report.deleted {
        println!("  deleted  {k}");
    }
    if !report.invalidated.is_empty() {
        println!(
            "  cdn ({}): invalidated {}",
            s3.cdn.provider(),
            report.invalidated.join(" ")
        );
    }
    println!("{}", report.summary(&s3.cfg.bucket));
    Ok(())
}

/// The bucket's `media/` as the media library's public storage.
#[cfg(feature = "media")]
pub struct BucketMedia(pub S3);

#[cfg(feature = "media")]
impl engine_media::PublicStorage for BucketMedia {
    fn describe(&self) -> String {
        format!("s3://{}/{}", self.0.cfg.bucket, self.0.media_key(""))
    }

    fn put<'a>(
        &'a self,
        rel: &'a str,
        body: &'a [u8],
        mime: &'a str,
    ) -> engine_media::storage::BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let mut opts = engine_s3::sync::put_options(&self.0, &format!("media/{rel}"));
            opts.content_type = mime.to_string();
            self.0
                .bucket
                .put(&self.0.media_key(rel), body, &opts)
                .await?;
            Ok(())
        })
    }

    fn delete<'a>(&'a self, rel: &'a str) -> engine_media::storage::BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.0.bucket.delete(&self.0.media_key(rel)).await?;
            // A deleted file may be cached; new ones never are (new ids).
            self.0.cdn.invalidate(&[format!("/media/{rel}")]).await?;
            Ok(())
        })
    }
}

/// The publisher's target for a bucket site: the bucket, mirrored into the
/// output folder.
#[cfg(feature = "publish")]
pub fn target(
    s3: S3,
    cms: &engine_publish::CmsSite,
    rt: &tokio::runtime::Handle,
) -> Result<Box<dyn engine_publish::PublishTarget>> {
    let output = cms.built_output()?;
    Ok(Box::new(engine_s3::S3Target::new(
        s3,
        engine_publish::DiskTarget::new(output),
        rt.clone(),
    )))
}
