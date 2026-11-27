//! `engine build` with the bucket target: make the bucket hold exactly the
//! built output folder.
//!
//! The comparison is against the bucket itself, not a manifest: one
//! `ListObjectsV2` of the prefix gives every object's ETag, which for a
//! single-part upload is the MD5 of its body. Files whose MD5 matches are
//! left alone; the rest are uploaded; objects with no file are deleted,
//! except under `media/` (the media library's files, written by uploads).
//! Then the URL paths of everything uploaded or deleted are invalidated at
//! the CDN. An object with a non-MD5 ETag (multipart, SSE-KMS) is simply
//! uploaded again.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::task::JoinSet;

use crate::S3;
use crate::client::{PutOptions, md5_hex};
use crate::paths::{MEDIA_PREFIX, cache_control, content_type, url_path};

/// Uploads at a time.
const CONCURRENCY: usize = 8;

/// What a sync did. Keys are relative to the prefix.
#[derive(Debug, Default, Clone)]
pub struct SyncReport {
    pub uploaded: Vec<String>,
    pub deleted: Vec<String>,
    pub unchanged: usize,
    /// Paths sent to the CDN.
    pub invalidated: Vec<String>,
}

impl SyncReport {
    pub fn summary(&self, bucket: &str) -> String {
        let mut s = format!(
            "Bucket {bucket}: {} uploaded, {} deleted, {} unchanged",
            self.uploaded.len(),
            self.deleted.len(),
            self.unchanged
        );
        if !self.invalidated.is_empty() {
            s.push_str(&format!(
                "; {} CDN path(s) invalidated",
                self.invalidated.len()
            ));
        }
        s
    }
}

/// Every file under `dir` as (key relative to `dir`, path); hidden files
/// and folders (`.engine-manifest.json`, temporary files) are skipped.
pub fn walk(dir: &Path) -> Result<Vec<(String, PathBuf)>> {
    fn go(base: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .with_context(|| format!("reading {}", dir.display()))?
            .collect::<std::io::Result<_>>()?;
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let path = e.path();
            let ft = e.file_type()?;
            if ft.is_dir() {
                go(base, &path, out)?;
            } else if ft.is_file() {
                let rel = path
                    .strip_prefix(base)
                    .expect("walked path is under its base")
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().to_string())
                    .collect::<Vec<_>>()
                    .join("/");
                out.push((rel, path));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    if dir.is_dir() {
        go(dir, dir, &mut out)?;
    }
    Ok(out)
}

/// The headers of an upload for a key relative to the prefix.
///
/// A pre-compressed copy (`articles/x/index.html.br`) carries its file's
/// type and caching with `Content-Encoding`, so a CDN function that picks
/// the copy by `Accept-Encoding` serves it as the page (docs: buckets).
pub fn put_options(s3: &S3, rel: &str) -> PutOptions {
    use engine_core::precompress::Encoding;
    let copy = (!rel.starts_with(MEDIA_PREFIX))
        .then(|| {
            Encoding::ALL.into_iter().find_map(|e| {
                let base = rel.strip_suffix(e.suffix())?;
                engine_core::precompress::compressible(Path::new(base)).then_some((e, base))
            })
        })
        .flatten();
    let (base, encoding) = match copy {
        Some((e, base)) => (base, Some(e.token().to_string())),
        None => (rel, None),
    };
    PutOptions {
        content_type: content_type(base).to_string(),
        cache_control: cache_control(&s3.cfg, base),
        content_disposition: (rel.starts_with(MEDIA_PREFIX) && rel.ends_with(".pdf"))
            .then(|| "attachment".to_string()),
        content_encoding: encoding,
    }
}

/// Sync `output` (the built site) and, if given, `media` (the media
/// library's public folder, as `media/<id>/<file>`) to the bucket.
pub async fn sync(s3: &S3, output: &Path, media: Option<&Path>) -> Result<SyncReport> {
    let mut local: BTreeMap<String, PathBuf> = BTreeMap::new();
    for (rel, path) in walk(output)? {
        local.insert(rel, path);
    }
    if let Some(m) = media {
        for (rel, path) in walk(m)? {
            local.insert(format!("{MEDIA_PREFIX}{rel}"), path);
        }
    }
    let prefix = &s3.cfg.prefix;
    let remote: BTreeMap<String, String> = s3
        .bucket
        .list(prefix)
        .await?
        .into_iter()
        .filter_map(|o| Some((o.key.strip_prefix(prefix.as_str())?.to_string(), o.etag)))
        .collect();

    // In the bucket every text file has both copies, even where the output
    // folder has none (too small to gain): the CDN function that picks a
    // copy by Accept-Encoding cannot see whether one exists.
    let mut generated: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for (rel, path) in &local {
        if rel.starts_with(MEDIA_PREFIX)
            || !engine_core::precompress::compressible(Path::new(rel))
            || engine_core::precompress::is_sidecar(Path::new(rel))
        {
            continue;
        }
        let missing: Vec<_> = engine_core::precompress::Encoding::ALL
            .into_iter()
            .filter(|e| !local.contains_key(&format!("{rel}{}", e.suffix())))
            .collect();
        if missing.is_empty() {
            continue;
        }
        let body = tokio::fs::read(path)
            .await
            .with_context(|| format!("reading {}", path.display()))?;
        for e in missing {
            generated.insert(format!("{rel}{}", e.suffix()), e.encode(&body));
        }
    }

    let mut report = SyncReport::default();
    let mut jobs: JoinSet<Result<String>> = JoinSet::new();
    let sem = Arc::new(tokio::sync::Semaphore::new(CONCURRENCY));
    let mut changed_urls = Vec::new();
    let mut all: Vec<(&String, Option<&std::path::PathBuf>)> =
        local.iter().map(|(r, p)| (r, Some(p))).collect();
    all.extend(generated.keys().map(|r| (r, None)));
    for (rel, path) in all {
        let body = match path {
            Some(path) => tokio::fs::read(path)
                .await
                .with_context(|| format!("reading {}", path.display()))?,
            None => generated[rel].clone(),
        };
        if remote.get(rel).is_some_and(|etag| *etag == md5_hex(&body)) {
            report.unchanged += 1;
            continue;
        }
        // New media files have new names (their library number): no CDN
        // can hold an old copy. Everything else is invalidated, new pages
        // too (a CDN may have cached a "not found" for them).
        if !rel.starts_with(MEDIA_PREFIX) || remote.contains_key(rel) {
            changed_urls.push(url_path(rel));
        }
        let permit = sem.clone().acquire_owned().await?;
        let s3c = s3.clone();
        let rel = rel.clone();
        jobs.spawn(async move {
            let _permit = permit;
            let opts = put_options(&s3c, &rel);
            s3c.bucket
                .put(&format!("{}{rel}", s3c.cfg.prefix), &body, &opts)
                .await?;
            Ok(rel)
        });
    }
    while let Some(r) = jobs.join_next().await {
        report.uploaded.push(r??);
    }
    report.uploaded.sort();
    for rel in remote.keys() {
        if local.contains_key(rel) || generated.contains_key(rel) || rel.starts_with(MEDIA_PREFIX) {
            continue;
        }
        s3.bucket.delete(&format!("{prefix}{rel}")).await?;
        changed_urls.push(url_path(rel));
        report.deleted.push(rel.clone());
    }
    report.invalidated = s3.cdn.invalidate(&changed_urls).await?;
    Ok(report)
}
