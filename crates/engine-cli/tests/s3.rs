//! The bucket publish target end to end, through the real `engine` binary
//! and the `examples/cms-site` example: `engine build` uploads to a local
//! MinIO bucket, a second build uploads nothing, an edit through the
//! publish pipeline (`engine serve`) uploads only the page whose bytes
//! changed and the CDN (the `file` provider, a recording test double)
//! receives exactly that page's paths, unpublishing deletes the object, and
//! media uploads land under `media/`.
//!
//! Needs ENGINE_TEST_DATABASE_URL and ENGINE_TEST_S3_ENDPOINT (the MinIO
//! container of deploy/compose.minio.yml; scripts/test-*.sh pass it);
//! skipped otherwise.

#![cfg(all(feature = "s3", feature = "publish", feature = "media"))]

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use common::*;
use engine_db::testing::TestDb;
use engine_s3::S3;
use engine_s3::config::S3Config;
use engine_s3::creds::{AwsCredentials, CredentialSource, Credentials};

const ARTICLE: &str = "flattening-a-plane-sole";

fn example() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/cms-site")
}

struct Minio {
    endpoint: String,
    id: String,
    secret: String,
}

impl Minio {
    fn from_env() -> Option<Minio> {
        Some(Minio {
            endpoint: std::env::var("ENGINE_TEST_S3_ENDPOINT").ok()?,
            id: std::env::var("ENGINE_TEST_S3_ACCESS_KEY_ID").unwrap_or("wrought-test".into()),
            secret: std::env::var("ENGINE_TEST_S3_SECRET_ACCESS_KEY")
                .unwrap_or("wrought-test-secret".into()),
        })
    }

    fn env(&self) -> Vec<(&str, &str)> {
        vec![
            ("AWS_ACCESS_KEY_ID", self.id.as_str()),
            ("AWS_SECRET_ACCESS_KEY", self.secret.as_str()),
        ]
    }
}

/// The example site, publishing to `bucket` with the `file` CDN provider.
fn site(dir: &Path, m: &Minio, bucket: &str) -> PathBuf {
    let site = dir.join("site");
    copy_dir(&example(), &site);
    let toml = std::fs::read_to_string(site.join("site.toml")).unwrap();
    let toml = toml.replace(
        "[publish]\ntarget = \"disk\"\n",
        &format!(
            "[publish]\ntarget = \"s3\"\nbucket = \"{bucket}\"\nendpoint = \"{}\"\n\
             region = \"us-east-1\"\ncredentials = \"env\"\n\n[publish.cdn]\nprovider = \"file\"\n\
             file = \"cdn.log\"\n",
            m.endpoint
        ),
    );
    assert!(toml.contains("target = \"s3\""));
    std::fs::write(site.join("site.toml"), toml).unwrap();
    site
}

fn s3(site: &Path, m: &Minio) -> S3 {
    let text = std::fs::read_to_string(site.join("site.toml")).unwrap();
    let t: toml::Table = toml::from_str(&text).unwrap();
    let cfg = S3Config::from_site(t.get("publish").and_then(|p| p.as_table()), site).unwrap();
    S3::with_credentials(
        cfg,
        Credentials::new(CredentialSource::Env, false)
            .unwrap()
            .with_aws(AwsCredentials::new(&m.id, &m.secret, None)),
    )
    .unwrap()
}

/// key → (etag, last modified).
fn objects(rt: &tokio::runtime::Handle, s3: &S3) -> BTreeMap<String, (String, String)> {
    rt.block_on(s3.bucket.list(""))
        .unwrap()
        .into_iter()
        .map(|o| (o.key, (o.etag, o.last_modified)))
        .collect()
}

fn changed(
    before: &BTreeMap<String, (String, String)>,
    after: &BTreeMap<String, (String, String)>,
) -> Vec<String> {
    let mut v: Vec<String> = after
        .iter()
        .filter(|(k, o)| before.get(*k) != Some(*o))
        .map(|(k, _)| k.clone())
        .collect();
    v.extend(before.keys().filter(|k| !after.contains_key(*k)).cloned());
    v.sort();
    v
}

fn cdn_batches(site: &Path) -> Vec<Vec<String>> {
    read(&site.join("cdn.log"))
        .lines()
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).unwrap();
            v["paths"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p.as_str().unwrap().to_string())
                .collect()
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn build_and_publish_to_a_bucket() {
    let Some(m) = Minio::from_env() else {
        eprintln!("skipped: ENGINE_TEST_S3_ENDPOINT not set");
        return;
    };
    let Some(t) = TestDb::create("v06_s3").await else {
        return;
    };
    let db = t.url.clone();
    let dir = tempfile::tempdir().unwrap();
    let bucket = format!("wrought-test-e2e-{}", std::process::id());
    let site = site(dir.path(), &m, &bucket);
    let s3 = s3(&site, &m);
    s3.bucket.create().await.unwrap();
    let s3c = s3.clone();
    let result = tokio::task::spawn_blocking(move || run(&db, &site, &m, &s3c)).await;
    s3.bucket.destroy().await.unwrap();
    t.cleanup().await;
    result.unwrap();
}

fn run(db: &str, site: &Path, m: &Minio, s3: &S3) {
    let rt = tokio::runtime::Handle::current();
    let s = site.to_str().unwrap();
    let env = m.env();
    let engine = |args: &[&str]| engine_env(db, args, None, &env);
    engine(&["migrate", "--site", s]);

    // Media go to the bucket as they are added.
    let out = engine(&[
        "media",
        "add",
        "--site",
        s,
        example()
            .join("media-samples/harbour-at-dusk.jpg")
            .to_str()
            .unwrap(),
    ]);
    assert!(out.contains("Added #1"), "{out}");
    engine(&[
        "media",
        "add",
        "--site",
        s,
        example()
            .join("media-samples/forge-yard.jpg")
            .to_str()
            .unwrap(),
    ]);
    let objs = objects(&rt, s3);
    assert!(objs.contains_key("media/1/harbour-at-dusk.jpg"), "{objs:?}");
    assert!(
        objs.keys()
            .any(|k| k.starts_with("media/1/harbour-at-dusk-")),
        "resized copies too: {objs:?}"
    );
    let (_, h) = rt
        .block_on(s3.bucket.get("media/1/harbour-at-dusk.jpg"))
        .unwrap()
        .unwrap();
    assert_eq!(h["content-type"], "image/jpeg");
    assert_eq!(h["cache-control"], "public, max-age=31536000, immutable");

    // First build: every page uploaded, and invalidated.
    engine(&["content", "seed", "--site", s]);
    let out = engine(&["build", "--site", s]);
    assert!(out.contains(&format!("Bucket {}", s3.cfg.bucket)), "{out}");
    let objs = objects(&rt, s3);
    for k in [
        "index.html",
        "404.html",
        "articles/index.html",
        &format!("articles/{ARTICLE}/index.html"),
        "colophon/index.html",
        "_engine/tagbrush-2.0.1.min.css",
        "media/1/harbour-at-dusk.jpg",
    ] {
        assert!(objs.contains_key(k), "{k} missing: {:?}", objs.keys());
    }
    assert!(!objs.contains_key(".engine-manifest.json"));
    let (body, h) = rt
        .block_on(s3.bucket.get(&format!("articles/{ARTICLE}/index.html")))
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&body).contains("Flattening a plane sole"));
    assert_eq!(h["cache-control"], "public, max-age=60");
    assert_eq!(h["content-type"], "text/html; charset=utf-8");
    let batches = cdn_batches(site);
    assert_eq!(batches.len(), 1, "{batches:?}");
    assert!(batches[0].contains(&format!("/articles/{ARTICLE}/")));
    assert!(
        !batches[0].iter().any(|p| p.starts_with("/media/")),
        "new media need no invalidation: {:?}",
        batches[0]
    );

    // A second build changes nothing (the colophon is rendered again but
    // its bytes are the same): nothing uploaded, nothing invalidated.
    let before = objects(&rt, s3);
    let out = engine(&["build", "--site", s]);
    assert!(out.contains("0 uploaded, 0 deleted"), "{out}");
    assert_eq!(changed(&before, &objects(&rt, s3)), Vec::<String>::new());
    assert_eq!(cdn_batches(site).len(), 1);

    // Publish mode: an edit to one article's body.
    let (server, port) = serve(db, site, &site.join("serve.log"), &env);
    wait_for("the publisher to start", || {
        server.log().contains("No render-on-miss")
    });
    // No render-on-miss: a publish page missing from the output folder is
    // not rendered on request.
    std::fs::remove_file(site.join("public/topics/index.html")).unwrap();
    let mut b = Browser::new(port);
    assert_eq!(b.get("/topics/").status, 404);

    let before = objects(&rt, s3);
    // LastModified has millisecond (MinIO) or second (S3) precision.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    engine(&[
        "content",
        "set",
        "--site",
        s,
        "article",
        ARTICLE,
        "--set",
        "body=Rewritten: lap the sole on glass, then check it with a straightedge.",
    ]);
    wait_for("the edit to be published", || cdn_batches(site).len() == 2);
    let after = objects(&rt, s3);
    let page = format!("articles/{ARTICLE}/index.html");
    assert_eq!(
        changed(&before, &after),
        std::slice::from_ref(&page),
        "only the edited page was uploaded\n{}",
        server.log()
    );
    assert_eq!(
        cdn_batches(site)[1],
        [format!("/articles/{ARTICLE}/"), format!("/{page}")],
        "exactly the edited page was invalidated"
    );
    let log = server.log();
    assert!(log.contains("unchanged"), "{log}");
    let (body, _) = rt.block_on(s3.bucket.get(&page)).unwrap().unwrap();
    assert!(String::from_utf8_lossy(&body).contains("Rewritten: lap the sole"));
    assert!(
        read(&site.join("public").join(&page)).contains("Rewritten: lap the sole"),
        "the output folder mirrors the bucket"
    );

    // Unpublishing deletes the object and invalidates its paths.
    engine(&[
        "content",
        "set",
        "--site",
        s,
        "article",
        ARTICLE,
        "--unpublish",
    ]);
    wait_for("the unpublish to be published", || {
        cdn_batches(site).len() == 3
    });
    let objs = objects(&rt, s3);
    assert!(!objs.contains_key(&page), "deleted from the bucket");
    let last = &cdn_batches(site)[2];
    assert!(last.contains(&format!("/articles/{ARTICLE}/")), "{last:?}");
    assert!(objs.contains_key("media/1/harbour-at-dusk.jpg"));
    drop(server);

    // A build after all that deletes nothing under media/.
    let out = engine(&["build", "--site", s]);
    assert!(out.contains("Bucket"), "{out}");
    assert!(objects(&rt, s3).contains_key("media/2/forge-yard.jpg"));
}

#[test]
fn keys_in_site_toml_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let site = dir.path().join("site");
    copy_dir(&example(), &site);
    let toml = std::fs::read_to_string(site.join("site.toml"))
        .unwrap()
        .replace(
            "[publish]\ntarget = \"disk\"\n",
            "[publish]\ntarget = \"s3\"\nbucket = \"b-x-y\"\nregion = \"eu-west-1\"\n\
         secret_access_key = \"do-not-print-me\"\n",
        );
    std::fs::write(site.join("site.toml"), toml).unwrap();
    let out = std::process::Command::new(ENGINE)
        .args(["build", "--site", site.to_str().unwrap()])
        .env("ENGINE_DATABASE_URL", "sqlite://data/site.db")
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("looks like a credential"), "{text}");
    assert!(text.contains("AWS_ACCESS_KEY_ID"), "{text}");
    assert!(!text.contains("do-not-print-me"), "{text}");
}
