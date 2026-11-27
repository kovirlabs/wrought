//! Against a real S3-compatible server (the local MinIO container,
//! deploy/compose.minio.yml). Skipped unless `ENGINE_TEST_S3_ENDPOINT` is
//! set (scripts/test-postgres.sh and scripts/test-sqlite.sh set it when the
//! container runs).

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use engine_core::target::{DiskTarget, PublishTarget};
use engine_s3::cdn::RecordingCdn;
use engine_s3::config::{CdnConfig, S3Config};
use engine_s3::creds::{AwsCredentials, CredentialSource, Credentials};
use engine_s3::{Invalidator, S3, S3Target};

static N: AtomicU32 = AtomicU32::new(0);

/// A fresh bucket and a recording CDN, or `None` (skip).
async fn bucket(prefix: &str) -> Option<(S3, Arc<RecordingCdn>)> {
    let endpoint = std::env::var("ENGINE_TEST_S3_ENDPOINT").ok()?;
    let id = std::env::var("ENGINE_TEST_S3_ACCESS_KEY_ID").unwrap_or("wrought-test".into());
    let secret =
        std::env::var("ENGINE_TEST_S3_SECRET_ACCESS_KEY").unwrap_or("wrought-test-secret".into());
    let name = format!(
        "wrought-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    );
    let t: toml::Table = toml::from_str(&format!(
        "target = 's3'\nbucket = '{name}'\nendpoint = '{endpoint}'\nprefix = '{prefix}'"
    ))
    .unwrap();
    let cfg = S3Config::from_site(Some(&t), Path::new(".")).unwrap();
    let creds = Credentials::new(CredentialSource::Env, false)
        .unwrap()
        .with_aws(AwsCredentials::new(&id, &secret, None));
    let rec = Arc::new(RecordingCdn::default());
    let s3 = S3::with_credentials(cfg, creds)
        .unwrap()
        .with_cdn(Invalidator::with(
            rec.clone(),
            CdnConfig {
                max_paths: 1000,
                ..Default::default()
            },
        ));
    s3.bucket.create().await.unwrap();
    Some((s3, rec))
}

fn write(dir: &Path, rel: &str, body: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

async fn keys(s3: &S3) -> Vec<String> {
    s3.bucket
        .list("")
        .await
        .unwrap()
        .into_iter()
        .map(|o| o.key)
        .collect()
}

/// Without the pre-compressed copies (`.br`, `.gz`), which every text
/// file has in the bucket and are checked on their own.
fn plain<S: AsRef<str>>(v: &[S]) -> Vec<String> {
    v.iter()
        .map(|s| s.as_ref().to_string())
        .filter(|s| !s.ends_with(".br") && !s.ends_with(".gz"))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn sync_uploads_only_what_differs() {
    let Some((s3, rec)) = bucket("www").await else {
        eprintln!("skipped: ENGINE_TEST_S3_ENDPOINT not set");
        return;
    };
    let out = tempfile::tempdir().unwrap();
    let media = tempfile::tempdir().unwrap();
    write(out.path(), "index.html", "<p>home</p>");
    write(out.path(), "articles/one/index.html", "<p>one</p>");
    write(out.path(), "articles/two/index.html", "<p>two</p>");
    write(out.path(), "_engine/tagbrush-2.0.1.min.css", "b{}");
    write(out.path(), "css/site.css", "p{}");
    write(out.path(), ".engine-manifest.json", "{}");
    write(media.path(), "7/photo.jpg", "jpeg bytes");
    write(media.path(), "8/doc.pdf", "%PDF");

    let r = engine_s3::sync(&s3, out.path(), Some(media.path()))
        .await
        .unwrap();
    assert_eq!(plain(&r.uploaded).len(), 7, "{r:?}");
    // Five text files, two copies each (made on the fly: all are small).
    assert_eq!(r.uploaded.len(), 17, "{r:?}");
    let (body, h) = s3
        .bucket
        .get("www/articles/one/index.html.br")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(h["content-encoding"], "br");
    assert_eq!(h["content-type"], "text/html; charset=utf-8");
    let mut text = String::new();
    std::io::Read::read_to_string(&mut brotli::Decompressor::new(&body[..], 4096), &mut text)
        .unwrap();
    assert_eq!(text, "<p>one</p>");
    assert!(r.deleted.is_empty());
    let mut want = vec![
        "www/_engine/tagbrush-2.0.1.min.css",
        "www/articles/one/index.html",
        "www/articles/two/index.html",
        "www/css/site.css",
        "www/index.html",
        "www/media/7/photo.jpg",
        "www/media/8/doc.pdf",
    ];
    want.sort();
    assert_eq!(plain(&keys(&s3).await), want);
    // New media are not invalidated; new pages are.
    assert_eq!(
        rec.batches().iter().map(|b| plain(b)).collect::<Vec<_>>(),
        [vec![
            "/_engine/tagbrush-2.0.1.min.css",
            "/articles/one/",
            "/articles/one/index.html",
            "/articles/two/",
            "/articles/two/index.html",
            "/css/site.css",
            "/",
            "/index.html"
        ]]
    );

    // Headers.
    let (body, h) = s3
        .bucket
        .get("www/articles/one/index.html")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(body, b"<p>one</p>");
    assert_eq!(h["content-type"], "text/html; charset=utf-8");
    assert_eq!(h["cache-control"], "public, max-age=60");
    assert_eq!(
        h["etag"].to_str().unwrap().trim_matches('"'),
        engine_s3::client::md5_hex(b"<p>one</p>")
    );
    let (_, h) = s3
        .bucket
        .get("www/_engine/tagbrush-2.0.1.min.css")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(h["cache-control"], "public, max-age=31536000, immutable");
    assert_eq!(h["content-type"], "text/css; charset=utf-8");
    let (_, h) = s3.bucket.get("www/css/site.css").await.unwrap().unwrap();
    assert_eq!(h["cache-control"], "public, max-age=3600");
    let (_, h) = s3.bucket.get("www/media/8/doc.pdf").await.unwrap().unwrap();
    assert_eq!(h["content-disposition"], "attachment");
    assert_eq!(h["cache-control"], "public, max-age=31536000, immutable");

    // Nothing changed: nothing uploaded, nothing invalidated.
    let r = engine_s3::sync(&s3, out.path(), Some(media.path()))
        .await
        .unwrap();
    assert!(r.uploaded.is_empty() && r.deleted.is_empty(), "{r:?}");
    assert_eq!(r.unchanged, 17);
    assert_eq!(rec.batches().len(), 1);

    // One page changed, one gone; media never deleted, even without the
    // local media folder.
    write(out.path(), "articles/one/index.html", "<p>one, edited</p>");
    std::fs::remove_dir_all(out.path().join("articles/two")).unwrap();
    let r = engine_s3::sync(&s3, out.path(), None).await.unwrap();
    assert_eq!(plain(&r.uploaded), ["articles/one/index.html"]);
    assert_eq!(plain(&r.deleted), ["articles/two/index.html"]);
    assert_eq!(r.deleted.len(), 3, "the copies go with the page: {r:?}");
    assert_eq!(
        plain(&rec.batches()[1]),
        [
            "/articles/one/",
            "/articles/one/index.html",
            "/articles/two/",
            "/articles/two/index.html"
        ]
    );
    assert!(
        keys(&s3)
            .await
            .contains(&"www/media/7/photo.jpg".to_string())
    );
    s3.bucket.destroy().await.unwrap();
}

#[test]
fn publish_target_puts_deletes_and_invalidates() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let Some((s3, rec)) = rt.block_on(bucket("")) else {
        eprintln!("skipped: ENGINE_TEST_S3_ENDPOINT not set");
        return;
    };
    let out = tempfile::tempdir().unwrap();
    let t = S3Target::new(s3.clone(), DiskTarget::new(out.path()), rt.handle().clone());
    let rel = Path::new("articles/x/index.html");
    // The publisher calls the target from a blocking thread.
    std::thread::scope(|sc| {
        sc.spawn(|| {
            t.put(rel, b"<p>x</p>").unwrap();
            assert!(t.exists(rel));
            assert!(t.is_current(rel, b"<p>x</p>"));
            assert!(!t.is_current(rel, b"<p>y</p>"));
            assert!(t.skip_unchanged() && !t.render_on_miss());
            assert!(t.put(Path::new("media/1/x.jpg"), b"x").is_err());
            t.finish_batch(&["/articles/x/".into()]).unwrap();
        });
    });
    assert_eq!(
        rt.block_on(keys(&s3)),
        [
            "articles/x/index.html",
            "articles/x/index.html.br",
            "articles/x/index.html.gz"
        ]
    );
    assert_eq!(
        std::fs::read(out.path().join(rel)).unwrap(),
        b"<p>x</p>",
        "the local output folder mirrors the bucket"
    );
    std::thread::scope(|sc| {
        sc.spawn(|| {
            t.delete(rel).unwrap();
            t.finish_batch(&[]).unwrap();
        });
    });
    assert!(rt.block_on(keys(&s3)).is_empty());
    assert!(!out.path().join(rel).exists());
    assert_eq!(
        rec.batches(),
        [vec!["/articles/x/", "/articles/x/index.html"]]
    );
    rt.block_on(s3.bucket.destroy()).unwrap();
}

#[tokio::test]
async fn wrong_key_is_refused_without_leaking_it() {
    let Some(endpoint) = std::env::var("ENGINE_TEST_S3_ENDPOINT").ok() else {
        return;
    };
    let t: toml::Table = toml::from_str(&format!(
        "target = 's3'\nbucket = 'wrought-test-nokey'\nendpoint = '{endpoint}'"
    ))
    .unwrap();
    let cfg = S3Config::from_site(Some(&t), Path::new(".")).unwrap();
    let s3 = S3::with_credentials(
        cfg,
        Credentials::new(CredentialSource::Env, false)
            .unwrap()
            .with_aws(AwsCredentials::new("wrought-test", "not-the-secret", None)),
    )
    .unwrap();
    let err = format!("{:#}", s3.bucket.list("").await.unwrap_err());
    assert!(err.contains("403"), "{err}");
    assert!(!err.contains("not-the-secret"), "{err}");
}
