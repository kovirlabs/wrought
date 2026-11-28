//! CDN invalidation behind one trait, [`Cdn`].
//!
//! Providers (`[publish.cdn] provider`):
//!
//! * `none`: nothing (HTML lifetimes alone, `html_max_age`);
//! * `log`: print the paths that would be invalidated;
//! * `file`: append each batch as one JSON line
//!   (`{"paths": [...]}`) to a file, for scripts that invalidate by other
//!   means, and the test double of the end-to-end tests;
//! * `cloudfront`: `POST /2020-05-31/distribution/{id}/invalidation`
//!   (CreateInvalidation), SigV4-signed for `cloudfront` in `us-east-1`;
//! * `google`: Cloud CDN, `POST .../projects/{p}/global/urlMaps/{m}/invalidateCache`
//!   once per path (the API takes one path per call), with the service
//!   account's bearer token.
//!
//! [`Invalidator`] expands URL paths per `index_paths` and caps them at
//! `max_paths` (one `/*` instead) before calling the provider.

use std::future::Future;
use std::io::Write;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};

use crate::client::{amz_date, host_of, http_client};
use crate::config::{CdnConfig, CdnProvider};
use crate::creds::Credentials;
use crate::endpoint::{
    CLOUDFRONT_HOST, GOOGLE_COMPUTE_HOST, check_bearer, check_fixed, check_test_override,
};
use crate::paths::invalidation_paths;
use crate::{sigv4, xml};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A CDN that can drop cached copies of paths.
pub trait Cdn: Send + Sync {
    fn name(&self) -> &str;
    /// Invalidate exactly `paths` (already expanded; never empty).
    fn invalidate<'a>(&'a self, paths: &'a [String]) -> BoxFuture<'a, Result<()>>;
}

/// `none`.
pub struct NoCdn;

impl Cdn for NoCdn {
    fn name(&self) -> &str {
        "none"
    }
    fn invalidate<'a>(&'a self, _paths: &'a [String]) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

/// `log`: print what would be invalidated.
pub struct LogCdn;

impl Cdn for LogCdn {
    fn name(&self) -> &str {
        "log"
    }
    fn invalidate<'a>(&'a self, paths: &'a [String]) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            println!("cdn (log): invalidate {}", paths.join(" "));
            Ok(())
        })
    }
}

/// `file`: one JSON line per batch.
pub struct FileCdn {
    pub path: PathBuf,
    lock: Mutex<()>,
}

impl FileCdn {
    pub fn new(path: PathBuf) -> Self {
        FileCdn {
            path,
            lock: Mutex::new(()),
        }
    }
}

impl Cdn for FileCdn {
    fn name(&self) -> &str {
        "file"
    }
    fn invalidate<'a>(&'a self, paths: &'a [String]) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
            let line = serde_json::json!({ "paths": paths }).to_string();
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
                .with_context(|| format!("opening {}", self.path.display()))?;
            writeln!(f, "{line}")?;
            Ok(())
        })
    }
}

/// A test double that keeps every batch in memory.
#[derive(Default)]
pub struct RecordingCdn {
    pub batches: Mutex<Vec<Vec<String>>>,
}

impl RecordingCdn {
    pub fn batches(&self) -> Vec<Vec<String>> {
        self.batches
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}

impl Cdn for RecordingCdn {
    fn name(&self) -> &str {
        "recording"
    }
    fn invalidate<'a>(&'a self, paths: &'a [String]) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.batches
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(paths.to_vec());
            Ok(())
        })
    }
}

/// Percent-encode what a URL path may not hold literally (CloudFront wants
/// encoded paths; `*` stays, it is the wildcard).
fn encode_url_path(p: &str) -> String {
    use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
    const SET: &AsciiSet = &CONTROLS
        .add(b' ')
        .add(b'"')
        .add(b'#')
        .add(b'%')
        .add(b'<')
        .add(b'>')
        .add(b'?')
        .add(b'`')
        .add(b'{')
        .add(b'}')
        .add(b'^')
        .add(b'|')
        .add(b'\\');
    utf8_percent_encode(p, SET).to_string()
}

/// Amazon CloudFront.
pub struct CloudFront {
    distribution_id: String,
    endpoint: String,
    /// Set only by [`CloudFront::with_endpoint_for_tests`].
    test_endpoint: bool,
    creds: Arc<Credentials>,
    http: reqwest::Client,
}

impl CloudFront {
    pub fn new(cfg: &CdnConfig, creds: Arc<Credentials>) -> Result<Self> {
        Ok(CloudFront {
            distribution_id: cfg.distribution_id.clone(),
            endpoint: format!("https://{CLOUDFRONT_HOST}"),
            test_endpoint: false,
            creds,
            http: http_client()?,
        })
    }

    /// **Tests only**: send to a mock server on this machine instead of
    /// the CloudFront API. Not reachable from `site.toml` or the
    /// environment; anything but a loopback URL is refused.
    pub fn with_endpoint_for_tests(mut self, url: &str) -> Result<Self> {
        check_test_override(url)?;
        self.endpoint = url.trim_end_matches('/').to_string();
        self.test_endpoint = true;
        Ok(self)
    }

    /// The request body (public for tests).
    pub fn body(paths: &[String], caller_reference: &str) -> String {
        let items: String = paths
            .iter()
            .map(|p| format!("<Path>{}</Path>", xml::escape(&encode_url_path(p))))
            .collect();
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <InvalidationBatch xmlns=\"http://cloudfront.amazonaws.com/doc/2020-05-31/\">\
             <Paths><Quantity>{}</Quantity><Items>{items}</Items></Paths>\
             <CallerReference>{}</CallerReference></InvalidationBatch>",
            paths.len(),
            xml::escape(caller_reference)
        )
    }
}

impl Cdn for CloudFront {
    fn name(&self) -> &str {
        "cloudfront"
    }
    fn invalidate<'a>(&'a self, paths: &'a [String]) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let creds = self.creds.aws().await?;
            let path = format!(
                "/2020-05-31/distribution/{}/invalidation",
                self.distribution_id
            );
            let url = format!("{}{path}", self.endpoint);
            if self.test_endpoint {
                check_test_override(&url)?;
            } else {
                check_fixed(&url, CLOUDFRONT_HOST)?;
            }
            let now = amz_date();
            let reference = format!(
                "hammerwright-{now}-{}",
                &sigv4::sha256_hex(paths.join("\n").as_bytes())[..12]
            );
            let body = Self::body(paths, &reference);
            let headers = vec![("content-type".to_string(), "text/xml".to_string())];
            let signed = sigv4::sign(
                &sigv4::Request {
                    method: "POST",
                    host: host_of(&url),
                    path: &path,
                    query: &[],
                    headers: &headers,
                    payload_sha256: &sigv4::sha256_hex(body.as_bytes()),
                },
                &creds,
                "us-east-1",
                "cloudfront",
                &now,
                false,
            );
            let mut req = self.http.post(&url);
            for (k, v) in &signed {
                req = req.header(k.as_str(), v.as_str());
            }
            let resp = req
                .body(body)
                .send()
                .await
                .context("CloudFront CreateInvalidation")?;
            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                bail!(
                    "CloudFront CreateInvalidation answered {status}: {} {}",
                    xml::first(&text, "Code").unwrap_or_default(),
                    xml::first(&text, "Message").unwrap_or_default()
                );
            }
            Ok(())
        })
    }
}

/// Google Cloud CDN (through the URL map of the load balancer).
pub struct GoogleCdn {
    project: String,
    url_map: String,
    host: Option<String>,
    endpoint: String,
    /// Set only by [`GoogleCdn::with_endpoint_for_tests`].
    test_endpoint: bool,
    creds: Arc<Credentials>,
    http: reqwest::Client,
}

impl GoogleCdn {
    pub fn new(cfg: &CdnConfig, creds: Arc<Credentials>) -> Result<Self> {
        Ok(GoogleCdn {
            project: cfg.project.clone(),
            url_map: cfg.url_map.clone(),
            host: cfg.host.clone(),
            endpoint: format!("https://{GOOGLE_COMPUTE_HOST}"),
            test_endpoint: false,
            creds,
            http: http_client()?,
        })
    }

    /// **Tests only**: send to a mock server on this machine instead of
    /// the Compute API. Not reachable from `site.toml` or the environment;
    /// anything but a loopback URL is refused. The only way a bearer token
    /// goes over plain HTTP, and then only to this machine.
    pub fn with_endpoint_for_tests(mut self, url: &str) -> Result<Self> {
        check_test_override(url)?;
        self.endpoint = url.trim_end_matches('/').to_string();
        self.test_endpoint = true;
        Ok(self)
    }
}

impl Cdn for GoogleCdn {
    fn name(&self) -> &str {
        "google"
    }
    fn invalidate<'a>(&'a self, paths: &'a [String]) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let token = self.creds.google().await?;
            let url = format!(
                "{}/compute/v1/projects/{}/global/urlMaps/{}/invalidateCache",
                self.endpoint, self.project, self.url_map
            );
            if self.test_endpoint {
                check_test_override(&url)?;
            } else {
                check_bearer(&url, &[GOOGLE_COMPUTE_HOST])?;
            }
            for p in paths {
                let mut body = serde_json::json!({ "path": encode_url_path(p) });
                if let Some(h) = &self.host {
                    body["host"] = serde_json::Value::String(h.clone());
                }
                let resp = self
                    .http
                    .post(&url)
                    .header("authorization", format!("Bearer {}", token.expose()))
                    .header("content-type", "application/json")
                    .body(body.to_string())
                    .send()
                    .await
                    .context("Cloud CDN invalidateCache")?;
                if !resp.status().is_success() {
                    let status = resp.status();
                    let text = resp.text().await.unwrap_or_default();
                    let msg = serde_json::from_str::<serde_json::Value>(&text)
                        .ok()
                        .and_then(|v| v["error"]["message"].as_str().map(String::from))
                        .unwrap_or_default();
                    bail!("Cloud CDN invalidateCache {p} answered {status}: {msg}");
                }
            }
            Ok(())
        })
    }
}

/// The configured provider plus path expansion.
pub struct Invalidator {
    cdn: Arc<dyn Cdn>,
    cfg: CdnConfig,
}

impl Invalidator {
    pub fn new(cfg: &CdnConfig, creds: Arc<Credentials>) -> Result<Self> {
        let cdn: Arc<dyn Cdn> = match cfg.provider {
            CdnProvider::None => Arc::new(NoCdn),
            CdnProvider::Log => Arc::new(LogCdn),
            CdnProvider::File => Arc::new(FileCdn::new(
                cfg.file.clone().context("[publish.cdn] file")?,
            )),
            CdnProvider::Cloudfront => Arc::new(CloudFront::new(cfg, creds)?),
            CdnProvider::Google => Arc::new(GoogleCdn::new(cfg, creds)?),
        };
        Ok(Invalidator {
            cdn,
            cfg: cfg.clone(),
        })
    }

    /// With any provider (tests).
    pub fn with(cdn: Arc<dyn Cdn>, cfg: CdnConfig) -> Self {
        Invalidator { cdn, cfg }
    }

    pub fn provider(&self) -> &str {
        self.cdn.name()
    }

    /// Invalidate the changed URL paths (`/articles/x/`, `/404.html`).
    /// Returns the paths sent (empty if there was nothing to do).
    pub async fn invalidate(&self, changed: &[String]) -> Result<Vec<String>> {
        if changed.is_empty() || self.cdn.name() == "none" {
            return Ok(Vec::new());
        }
        let paths = invalidation_paths(changed, self.cfg.index_paths, self.cfg.max_paths.max(1));
        self.cdn
            .invalidate(&paths)
            .await
            .with_context(|| format!("invalidating {} path(s) at the CDN", paths.len()))?;
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloudfront_body() {
        let b = CloudFront::body(&["/a b/".into(), "/x&y".into(), "/*".into()], "ref-1");
        assert!(b.contains("<Quantity>3</Quantity>"), "{b}");
        assert!(b.contains("<Path>/a%20b/</Path>"), "{b}");
        assert!(b.contains("<Path>/x&amp;y</Path>"), "{b}");
        assert!(b.contains("<Path>/*</Path>"), "{b}");
        assert!(
            b.contains("<CallerReference>ref-1</CallerReference>"),
            "{b}"
        );
    }
}
