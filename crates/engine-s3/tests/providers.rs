//! The real CDN providers and metadata credential sources against a local
//! mock HTTP server (endpoint overrides): request shape and signature.
//! Never a real cloud endpoint.

use std::path::Path;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use engine_s3::config::S3Config;
use engine_s3::creds::{AwsCredentials, CredentialSource, Credentials};
use engine_s3::{Invalidator, S3, sigv4};

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    query: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

type Responder = Arc<dyn Fn(&Seen) -> (u16, String) + Send + Sync>;

/// Start a server that records every request and answers with `respond`.
async fn mock(respond: Responder) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    let app = axum::Router::new().fallback(
        move |method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
            let s = s.clone();
            let respond = respond.clone();
            async move {
                let req = Seen {
                    method: method.to_string(),
                    path: uri.path().to_string(),
                    query: uri.query().unwrap_or("").to_string(),
                    headers: headers
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                        .collect(),
                    body: String::from_utf8_lossy(&body).to_string(),
                };
                let (status, text) = respond(&req);
                s.lock().unwrap().push(req);
                (StatusCode::from_u16(status).unwrap(), text)
            }
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), seen)
}

fn config(extra: &str) -> S3Config {
    let t: toml::Table = toml::from_str(&format!(
        "target = 's3'\nbucket = 'example-site'\nregion = 'eu-west-1'\n{extra}"
    ))
    .unwrap();
    S3Config::from_site(Some(&t), Path::new("/srv/site")).unwrap()
}

/// Recompute the SigV4 signature of a recorded request.
fn verify_sigv4(r: &Seen, creds: &AwsCredentials, region: &str, service: &str) {
    let auth = r.header("authorization").expect("signed");
    let signed: Vec<&str> = auth
        .split("SignedHeaders=")
        .nth(1)
        .unwrap()
        .split(',')
        .next()
        .unwrap()
        .split(';')
        .collect();
    let extra: Vec<(String, String)> = signed
        .iter()
        .filter(|h| {
            !matches!(
                **h,
                "host" | "x-amz-date" | "x-amz-content-sha256" | "x-amz-security-token"
            )
        })
        .map(|h| (h.to_string(), r.header(h).unwrap().to_string()))
        .collect();
    let query: Vec<(String, String)> = r
        .query
        .split('&')
        .filter(|q| !q.is_empty())
        .map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            let d = |s: &str| {
                percent_encoding::percent_decode_str(s)
                    .decode_utf8()
                    .unwrap()
                    .to_string()
            };
            (d(k), d(v))
        })
        .collect();
    let again = sigv4::sign(
        &sigv4::Request {
            method: &r.method,
            host: r.header("host").unwrap(),
            path: &r.path,
            query: &query,
            headers: &extra,
            payload_sha256: &sigv4::sha256_hex(r.body.as_bytes()),
        },
        creds,
        region,
        service,
        r.header("x-amz-date").unwrap(),
        r.header("x-amz-content-sha256").is_some(),
    );
    let want = &again.iter().find(|(k, _)| k == "authorization").unwrap().1;
    assert_eq!(auth, want, "signature of {} {}", r.method, r.path);
}

#[tokio::test]
async fn cloudfront_invalidation_is_signed_and_shaped() {
    let (url, seen) = mock(Arc::new(|_| (201, "<Invalidation/>".into()))).await;
    let cfg = config(&format!(
        "[cdn]\nprovider = 'cloudfront'\ndistribution_id = 'E2EXAMPLE'\nendpoint = '{url}'"
    ));
    let creds = AwsCredentials::new("AKIDEXAMPLE", "secret-example", None);
    let c = Arc::new(
        Credentials::new(CredentialSource::Env, false)
            .unwrap()
            .with_aws(creds.clone()),
    );
    let inv = Invalidator::new(&cfg.cdn, c).unwrap();
    let sent = inv
        .invalidate(&["/articles/x/".into(), "/".into(), "/a b.css".into()])
        .await
        .unwrap();
    assert_eq!(
        sent,
        [
            "/articles/x/",
            "/articles/x/index.html",
            "/",
            "/index.html",
            "/a b.css"
        ]
    );
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    let r = &seen[0];
    assert_eq!(r.method, "POST");
    assert_eq!(r.path, "/2020-05-31/distribution/E2EXAMPLE/invalidation");
    assert!(r.body.contains("<Quantity>5</Quantity>"), "{}", r.body);
    for p in [
        "/articles/x/",
        "/articles/x/index.html",
        "/index.html",
        "/a%20b.css",
    ] {
        assert!(
            r.body.contains(&format!("<Path>{p}</Path>")),
            "{p}: {}",
            r.body
        );
    }
    assert!(r.body.contains("<CallerReference>wrought-"));
    let auth = r.header("authorization").unwrap();
    assert!(
        auth.contains("Credential=AKIDEXAMPLE/")
            && auth.contains("/us-east-1/cloudfront/aws4_request"),
        "{auth}"
    );
    assert!(!auth.contains("secret-example"));
    verify_sigv4(r, &creds, "us-east-1", "cloudfront");
}

#[tokio::test]
async fn cloudfront_error_is_reported() {
    let (url, _) = mock(Arc::new(|_| {
        (
            403,
            "<ErrorResponse><Error><Code>AccessDenied</Code><Message>no</Message></Error></ErrorResponse>"
                .into(),
        )
    }))
    .await;
    let cfg = config(&format!(
        "[cdn]\nprovider = 'cloudfront'\ndistribution_id = 'E2EXAMPLE'\nendpoint = '{url}'"
    ));
    let c = Arc::new(
        Credentials::new(CredentialSource::Env, false)
            .unwrap()
            .with_aws(AwsCredentials::new("AKID", "s", None)),
    );
    let err = Invalidator::new(&cfg.cdn, c)
        .unwrap()
        .invalidate(&["/x/".into()])
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("AccessDenied"), "{err:#}");
}

#[tokio::test]
async fn instance_role_from_imdsv2_signs_with_session_token() {
    let (url, seen) = mock(Arc::new(|r: &Seen| {
        match (r.method.as_str(), r.path.as_str()) {
            ("PUT", "/latest/api/token") => (200, "imds-session".into()),
            ("GET", "/latest/meta-data/iam/security-credentials/") => (200, "site-role\n".into()),
            ("GET", "/latest/meta-data/iam/security-credentials/site-role") => (
                200,
                r#"{"Code":"Success","AccessKeyId":"ASIAROLE","SecretAccessKey":"role-secret",
                "Token":"role-session-token","Expiration":"2099-01-01T00:00:00Z"}"#
                    .into(),
            ),
            _ => (201, String::new()),
        }
    }))
    .await;
    let cfg = config(&format!(
        "credentials = 'aws-instance'\n[cdn]\nprovider = 'cloudfront'\ndistribution_id = 'E1'\nendpoint = '{url}'"
    ));
    let c = Arc::new(
        Credentials::new(CredentialSource::AwsInstance, false)
            .unwrap()
            .with_metadata(Some(&url), None),
    );
    let inv = Invalidator::new(&cfg.cdn, c).unwrap();
    inv.invalidate(&["/a/".into()]).await.unwrap();
    // Cached: the second batch asks the metadata service nothing.
    inv.invalidate(&["/b/".into()]).await.unwrap();
    let seen = seen.lock().unwrap().clone();
    let calls: Vec<String> = seen
        .iter()
        .map(|r| format!("{} {}", r.method, r.path))
        .collect();
    assert_eq!(
        calls,
        [
            "PUT /latest/api/token",
            "GET /latest/meta-data/iam/security-credentials/",
            "GET /latest/meta-data/iam/security-credentials/site-role",
            "POST /2020-05-31/distribution/E1/invalidation",
            "POST /2020-05-31/distribution/E1/invalidation",
        ]
    );
    assert_eq!(
        seen[0].header("x-aws-ec2-metadata-token-ttl-seconds"),
        Some("21600")
    );
    assert_eq!(
        seen[1].header("x-aws-ec2-metadata-token"),
        Some("imds-session")
    );
    let post = &seen[3];
    assert_eq!(
        post.header("x-amz-security-token"),
        Some("role-session-token")
    );
    verify_sigv4(
        post,
        &AwsCredentials::new("ASIAROLE", "role-secret", Some("role-session-token")),
        "us-east-1",
        "cloudfront",
    );
}

#[tokio::test]
async fn google_cloud_cdn_with_metadata_token() {
    let (url, seen) = mock(Arc::new(|r: &Seen| {
        if r.path.starts_with("/computeMetadata/") {
            (
                200,
                r#"{"access_token":"ya29.metadata-token","expires_in":3599,"token_type":"Bearer"}"#
                    .into(),
            )
        } else {
            (
                200,
                r#"{"kind":"compute#operation","status":"DONE"}"#.into(),
            )
        }
    }))
    .await;
    let host = url.trim_start_matches("http://").to_string();
    let cfg = config(&format!(
        "credentials = 'gcp-metadata'\n[cdn]\nprovider = 'google'\nproject = 'my-proj'\n\
         url_map = 'site-lb'\nhost = 'www.example.org'\nindex_paths = 'directory'\nendpoint = '{url}'"
    ));
    let c = Arc::new(
        Credentials::new(CredentialSource::GcpMetadata, false)
            .unwrap()
            .with_metadata(None, Some(&host)),
    );
    let inv = Invalidator::new(&cfg.cdn, c).unwrap();
    let sent = inv
        .invalidate(&["/articles/x/".into(), "/".into()])
        .await
        .unwrap();
    assert_eq!(sent, ["/articles/x/", "/"]);
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 3, "{seen:?}");
    assert_eq!(
        seen[0].path,
        "/computeMetadata/v1/instance/service-accounts/default/token"
    );
    assert_eq!(seen[0].header("metadata-flavor"), Some("Google"));
    for (r, p) in seen[1..].iter().zip(["/articles/x/", "/"]) {
        assert_eq!(r.method, "POST");
        assert_eq!(
            r.path,
            "/compute/v1/projects/my-proj/global/urlMaps/site-lb/invalidateCache"
        );
        assert_eq!(
            r.header("authorization"),
            Some("Bearer ya29.metadata-token")
        );
        let body: serde_json::Value = serde_json::from_str(&r.body).unwrap();
        assert_eq!(body["path"], p);
        assert_eq!(body["host"], "www.example.org");
    }
}

#[tokio::test]
async fn s3_requests_are_signed_and_listing_pages() {
    let (url, seen) = mock(Arc::new(|r: &Seen| {
        if r.method == "PUT" {
            return (200, String::new());
        }
        if r.query.contains("continuation-token") {
            (
                200,
                "<ListBucketResult><IsTruncated>false</IsTruncated><Contents><Key>www/b.html</Key>\
                 <ETag>&quot;bb&quot;</ETag><Size>2</Size></Contents></ListBucketResult>"
                    .into(),
            )
        } else {
            (
                200,
                "<ListBucketResult><IsTruncated>true</IsTruncated>\
                 <NextContinuationToken>tok/1=</NextContinuationToken><Contents><Key>www/a&amp;.html</Key>\
                 <ETag>&quot;aa&quot;</ETag><Size>1</Size></Contents></ListBucketResult>"
                    .into(),
            )
        }
    }))
    .await;
    let cfg = config(&format!("endpoint = '{url}'\nprefix = 'www'"));
    let creds = AwsCredentials::new("AKIDEXAMPLE", "secret-example", Some("sess"));
    let s3 = S3::with_credentials(
        cfg,
        Credentials::new(CredentialSource::Env, false)
            .unwrap()
            .with_aws(creds.clone()),
    )
    .unwrap();
    let objs = s3.bucket.list("www/").await.unwrap();
    assert_eq!(
        objs.iter()
            .map(|o| (o.key.as_str(), o.etag.as_str()))
            .collect::<Vec<_>>(),
        [("www/a&.html", "aa"), ("www/b.html", "bb")]
    );
    s3.bucket
        .put(
            "www/a b/index.html",
            b"<p>hi</p>",
            &engine_s3::sync::put_options(&s3, "a b/index.html"),
        )
        .await
        .unwrap();
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 3);
    assert_eq!(seen[0].path, "/example-site");
    assert!(
        seen[1].query.contains("continuation-token=tok%2F1%3D"),
        "{}",
        seen[1].query
    );
    let put = &seen[2];
    assert_eq!(put.path, "/example-site/www/a%20b/index.html");
    assert_eq!(put.header("content-type"), Some("text/html; charset=utf-8"));
    assert_eq!(put.header("cache-control"), Some("public, max-age=60"));
    assert_eq!(put.header("content-md5"), Some("ND/HsnwblmtiOqSDyIAhDA=="));
    for r in &seen {
        verify_sigv4(r, &creds, "eu-west-1", "s3");
    }
}
