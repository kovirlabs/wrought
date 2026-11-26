//! A small S3 client: the five object operations Wrought needs (put,
//! delete, head, list, and bucket create/delete for tests), signed with
//! SigV4 or a bearer token, over reqwest (rustls). Requests that fail with
//! a network error or a 5xx/429 answer are retried three times.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine;
use md5::{Digest, Md5};

use crate::config::S3Config;
use crate::creds::{Auth, Credentials};
use crate::{endpoint, sigv4, xml};

/// Hex MD5 (what S3 reports as the ETag of a single-part upload).
pub fn md5_hex(body: &[u8]) -> String {
    format!("{:x}", Md5::digest(body))
}

/// One object of a listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    /// Full key (with the prefix).
    pub key: String,
    /// The ETag without quotes.
    pub etag: String,
    pub size: u64,
    /// As the store reports it (ISO 8601).
    pub last_modified: String,
}

/// Headers of an upload besides the body's.
#[derive(Debug, Clone, Default)]
pub struct PutOptions {
    pub content_type: String,
    pub cache_control: String,
    pub content_disposition: Option<String>,
}

/// One bucket. Cheap to clone.
#[derive(Clone)]
pub struct Bucket {
    inner: Arc<Inner>,
}

struct Inner {
    name: String,
    endpoint: String,
    region: String,
    path_style: bool,
    creds: Arc<Credentials>,
    http: reqwest::Client,
}

impl std::fmt::Debug for Bucket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bucket")
            .field("name", &self.inner.name)
            .field("endpoint", &self.inner.endpoint)
            .finish_non_exhaustive()
    }
}

/// An error answer from the store.
#[derive(Debug)]
pub struct S3Error {
    pub status: u16,
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for S3Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HTTP {}", self.status)?;
        if !self.code.is_empty() {
            write!(f, " {}", self.code)?;
        }
        if !self.message.is_empty() {
            write!(f, ": {}", self.message)?;
        }
        Ok(())
    }
}

impl std::error::Error for S3Error {}

/// The client for credential-carrying requests: never follows redirects.
pub(crate) fn http_client() -> Result<reqwest::Client> {
    endpoint::client(Duration::from_secs(120))
}

pub(crate) fn amz_date() -> String {
    chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string()
}

/// `host[:port]` of a URL (`scheme://host[:port]/...`).
pub(crate) fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    rest.split('/').next().unwrap_or(rest)
}

impl Bucket {
    pub fn new(cfg: &S3Config, creds: Arc<Credentials>) -> Result<Self> {
        Ok(Bucket {
            inner: Arc::new(Inner {
                name: cfg.bucket.clone(),
                endpoint: cfg.endpoint.clone(),
                region: cfg.region.clone(),
                path_style: cfg.path_style,
                creds,
                http: http_client()?,
            }),
        })
    }

    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// Base URL and the path prefix for keys.
    fn base(&self) -> (String, String) {
        let i = &self.inner;
        if i.path_style {
            (i.endpoint.clone(), format!("/{}", i.name))
        } else {
            let (scheme, host) = i
                .endpoint
                .split_once("://")
                .unwrap_or(("https", &i.endpoint));
            (format!("{scheme}://{}.{host}", i.name), String::new())
        }
    }

    /// Send one request (with retries). `key` is `None` for bucket-level
    /// requests.
    async fn send(
        &self,
        method: reqwest::Method,
        key: Option<&str>,
        query: &[(String, String)],
        headers: &[(String, String)],
        body: &[u8],
    ) -> Result<reqwest::Response> {
        let (base, path_prefix) = self.base();
        let path = match key {
            Some(k) => format!("{path_prefix}/{}", sigv4::encode_path(k)),
            None if path_prefix.is_empty() => "/".to_string(),
            None => path_prefix,
        };
        let qs = sigv4::canonical_query(query);
        let url = if qs.is_empty() {
            format!("{base}{path}")
        } else {
            format!("{base}{path}?{qs}")
        };
        let host = host_of(&url).to_string();
        let payload = sigv4::sha256_hex(body);
        let mut attempt = 0;
        loop {
            attempt += 1;
            let auth = self.inner.creds.storage().await?;
            // Where may this credential go? Checked before every send.
            match &auth {
                Auth::SigV4(_) => endpoint::check_sigv4(&url),
                Auth::Bearer(_) => endpoint::check_bearer(&url, &[endpoint::GOOGLE_STORAGE_HOST]),
            }
            .with_context(|| format!("bucket {}", self.name()))?;
            let all: Vec<(String, String)> = match &auth {
                Auth::SigV4(c) => sigv4::sign(
                    &sigv4::Request {
                        method: method.as_str(),
                        host: &host,
                        path: &path,
                        query,
                        headers,
                        payload_sha256: &payload,
                    },
                    c,
                    &self.inner.region,
                    "s3",
                    &amz_date(),
                    true,
                ),
                Auth::Bearer(t) => {
                    let mut h = headers.to_vec();
                    h.push(("authorization".into(), format!("Bearer {}", t.expose())));
                    h
                }
            };
            let mut req = self.inner.http.request(method.clone(), &url);
            for (k, v) in &all {
                req = req.header(k.as_str(), v.as_str());
            }
            let result = req.body(body.to_vec()).send().await;
            let retry = match &result {
                Err(_) => true,
                Ok(r) => r.status().is_server_error() || r.status().as_u16() == 429,
            };
            if !retry || attempt >= 4 {
                return result.with_context(|| format!("{method} {}", shown(&url)));
            }
            tokio::time::sleep(Duration::from_millis(200 * (1 << attempt))).await;
        }
    }

    /// Turn a non-success answer into an [`S3Error`].
    async fn check(resp: reqwest::Response, what: &str) -> Result<reqwest::Response> {
        if resp.status().is_success() {
            return Ok(resp);
        }
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        Err(anyhow::Error::new(S3Error {
            status,
            code: xml::first(&text, "Code").unwrap_or_default(),
            message: xml::first(&text, "Message").unwrap_or_default(),
        })
        .context(what.to_string()))
    }

    /// Upload `body` to `key` whole. Returns the ETag.
    pub async fn put(&self, key: &str, body: &[u8], opts: &PutOptions) -> Result<String> {
        let md5 = Md5::digest(body);
        let mut headers = vec![
            (
                "content-md5".to_string(),
                base64::engine::general_purpose::STANDARD.encode(md5),
            ),
            ("content-type".to_string(), opts.content_type.clone()),
            ("cache-control".to_string(), opts.cache_control.clone()),
        ];
        if let Some(d) = &opts.content_disposition {
            headers.push(("content-disposition".into(), d.clone()));
        }
        let resp = self
            .send(reqwest::Method::PUT, Some(key), &[], &headers, body)
            .await?;
        let resp = Self::check(resp, &format!("uploading {key}")).await?;
        let etag = resp
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .trim_matches('"')
            .to_string();
        let want = format!("{md5:x}");
        if etag.len() == 32 && etag.bytes().all(|b| b.is_ascii_hexdigit()) && etag != want {
            bail!("uploading {key}: the store's ETag does not match the body's MD5");
        }
        Ok(etag)
    }

    /// Delete `key` (no error if it is already gone).
    pub async fn delete(&self, key: &str) -> Result<()> {
        let resp = self
            .send(reqwest::Method::DELETE, Some(key), &[], &[], b"")
            .await?;
        if resp.status().as_u16() == 404 {
            return Ok(());
        }
        Self::check(resp, &format!("deleting {key}")).await?;
        Ok(())
    }

    /// Whether `key` exists.
    pub async fn exists(&self, key: &str) -> Result<bool> {
        let resp = self
            .send(reqwest::Method::HEAD, Some(key), &[], &[], b"")
            .await?;
        if resp.status().as_u16() == 404 {
            return Ok(false);
        }
        Self::check(resp, &format!("checking {key}")).await?;
        Ok(true)
    }

    /// Read `key` (tests and checks).
    pub async fn get(&self, key: &str) -> Result<Option<(Vec<u8>, reqwest::header::HeaderMap)>> {
        let resp = self
            .send(reqwest::Method::GET, Some(key), &[], &[], b"")
            .await?;
        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        let resp = Self::check(resp, &format!("reading {key}")).await?;
        let headers = resp.headers().clone();
        Ok(Some((resp.bytes().await?.to_vec(), headers)))
    }

    /// Every object under `prefix` (ListObjectsV2, all pages).
    pub async fn list(&self, prefix: &str) -> Result<Vec<Object>> {
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut q = vec![
                ("list-type".to_string(), "2".to_string()),
                ("prefix".to_string(), prefix.to_string()),
            ];
            if let Some(t) = &token {
                q.push(("continuation-token".into(), t.clone()));
            }
            let resp = self.send(reqwest::Method::GET, None, &q, &[], b"").await?;
            let resp = Self::check(resp, &format!("listing bucket {}", self.name())).await?;
            let text = resp.text().await?;
            for c in xml::all(&text, "Contents") {
                out.push(Object {
                    key: xml::first(c, "Key").unwrap_or_default(),
                    etag: xml::first(c, "ETag")
                        .unwrap_or_default()
                        .trim_matches('"')
                        .to_string(),
                    size: xml::first(c, "Size")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0),
                    last_modified: xml::first(c, "LastModified").unwrap_or_default(),
                });
            }
            let truncated = xml::first(&text, "IsTruncated").as_deref() == Some("true");
            token = xml::first(&text, "NextContinuationToken");
            if !truncated || token.is_none() {
                break;
            }
        }
        Ok(out)
    }

    /// Create the bucket (tests, local MinIO). Succeeds if it exists.
    pub async fn create(&self) -> Result<()> {
        let body = if self.inner.region == "us-east-1" {
            String::new()
        } else {
            format!(
                "<CreateBucketConfiguration><LocationConstraint>{}</LocationConstraint></CreateBucketConfiguration>",
                xml::escape(&self.inner.region)
            )
        };
        let resp = self
            .send(reqwest::Method::PUT, None, &[], &[], body.as_bytes())
            .await?;
        if resp.status().as_u16() == 409 {
            return Ok(());
        }
        Self::check(resp, &format!("creating bucket {}", self.name())).await?;
        Ok(())
    }

    /// Delete every object, then the bucket (tests).
    pub async fn destroy(&self) -> Result<()> {
        for o in self.list("").await? {
            self.delete(&o.key).await?;
        }
        let resp = self
            .send(reqwest::Method::DELETE, None, &[], &[], b"")
            .await?;
        if resp.status().as_u16() == 404 {
            return Ok(());
        }
        Self::check(resp, &format!("deleting bucket {}", self.name())).await?;
        Ok(())
    }
}

/// A URL for messages: no query string (it may hold a continuation token).
fn shown(url: &str) -> &str {
    url.split('?').next().unwrap_or(url)
}
