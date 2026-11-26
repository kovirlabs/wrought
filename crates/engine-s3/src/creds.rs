//! Credentials, only ever from the environment or the machine: never from
//! `site.toml`, never printed.
//!
//! Sources (`[publish] credentials`):
//!
//! * `env`: `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` and optionally
//!   `AWS_SESSION_TOKEN` (SigV4); for Google APIs `GOOGLE_OAUTH_ACCESS_TOKEN`
//!   (a bearer token);
//! * `aws-instance`: the machine's IAM role, from the ECS container
//!   endpoint (`AWS_CONTAINER_CREDENTIALS_RELATIVE_URI` / `_FULL_URI`) when
//!   set, else EC2 instance metadata (IMDSv2, token first, at
//!   169.254.169.254);
//! * `gcp-metadata`: the VM's service account, a bearer token from the
//!   metadata server (`metadata.google.internal`). Google Cloud Storage's
//!   XML API (its S3-compatible API)
//!   and the Compute API (Cloud CDN invalidation) both take it;
//! * `auto` (the default): `env` if `AWS_ACCESS_KEY_ID` is set; otherwise
//!   `gcp-metadata` when the endpoint is `storage.googleapis.com`; otherwise
//!   `aws-instance`.
//!
//! Temporary credentials are cached and fetched again five minutes before
//! they expire.

use std::fmt;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use tokio::sync::Mutex;

/// A secret string: `Debug` and `Display` print `<redacted>`.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Secret(s.into())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// An AWS access key (with a session token for temporary credentials).
#[derive(Clone, Debug)]
pub struct AwsCredentials {
    access_key_id: String,
    secret_access_key: Secret,
    session_token: Option<Secret>,
    expires: Option<DateTime<Utc>>,
}

impl AwsCredentials {
    pub fn new(access_key_id: &str, secret_access_key: &str, session_token: Option<&str>) -> Self {
        AwsCredentials {
            access_key_id: access_key_id.to_string(),
            secret_access_key: Secret::new(secret_access_key),
            session_token: session_token.map(Secret::new),
            expires: None,
        }
    }
    pub fn access_key_id(&self) -> &str {
        &self.access_key_id
    }
    pub fn secret_access_key(&self) -> &str {
        self.secret_access_key.expose()
    }
    pub fn session_token(&self) -> Option<&str> {
        self.session_token.as_ref().map(Secret::expose)
    }
}

/// How a request is authorised.
#[derive(Clone, Debug)]
pub enum Auth {
    /// AWS Signature Version 4.
    SigV4(AwsCredentials),
    /// An OAuth 2 bearer token (Google).
    Bearer(Secret),
}

/// `[publish] credentials`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CredentialSource {
    #[default]
    Auto,
    Env,
    AwsInstance,
    GcpMetadata,
}

impl CredentialSource {
    pub const NAMES: &'static [&'static str] = &["auto", "env", "aws-instance", "gcp-metadata"];

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "auto" => CredentialSource::Auto,
            "env" => CredentialSource::Env,
            "aws-instance" => CredentialSource::AwsInstance,
            "gcp-metadata" => CredentialSource::GcpMetadata,
            _ => return None,
        })
    }
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// Shape of the IMDS and ECS credential documents.
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RoleDoc {
    access_key_id: String,
    secret_access_key: String,
    token: Option<String>,
    expiration: Option<String>,
}

#[derive(Deserialize)]
struct GoogleToken {
    access_token: String,
    expires_in: Option<i64>,
}

/// Credentials of one site, cached. Cheap to share behind an `Arc`.
pub struct Credentials {
    source: CredentialSource,
    /// The S3 endpoint (for `auto`).
    google_endpoint: bool,
    http: reqwest::Client,
    aws: Mutex<Option<AwsCredentials>>,
    google: Mutex<Option<(Secret, Option<DateTime<Utc>>)>>,
    /// Given credentials instead of the environment (tests).
    fixed_aws: Option<AwsCredentials>,
    /// Metadata endpoints instead of the environment's (tests).
    imds_endpoint: Option<String>,
    gce_host: Option<String>,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

fn fresh(expires: Option<DateTime<Utc>>) -> bool {
    match expires {
        None => true,
        Some(e) => e - chrono::Duration::minutes(5) > Utc::now(),
    }
}

impl Credentials {
    /// `google_endpoint`: the bucket is Google Cloud Storage.
    pub fn new(source: CredentialSource, google_endpoint: bool) -> Result<Self> {
        // No redirects: a metadata server never sends any, and a token
        // request must not be bounced elsewhere.
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5))
            .connect_timeout(Duration::from_secs(2))
            .build()
            .context("building the HTTP client")?;
        Ok(Credentials {
            source,
            google_endpoint,
            http,
            aws: Mutex::new(None),
            google: Mutex::new(None),
            fixed_aws: None,
            imds_endpoint: None,
            gce_host: None,
        })
    }

    /// Use these AWS credentials instead of the environment's (tests and
    /// embedding; `site.toml` never reaches this).
    pub fn with_aws(mut self, c: AwsCredentials) -> Self {
        self.fixed_aws = Some(c);
        self
    }

    /// **Tests only**: ask metadata servers on this machine (an IMDS base
    /// URL, a GCE metadata `host:port`) instead of the real addresses.
    /// Nothing in `site.toml` or the environment reaches this; anything
    /// but a loopback address is refused.
    pub fn with_metadata_for_tests(
        mut self,
        imds: Option<&str>,
        gce_host: Option<&str>,
    ) -> Result<Self> {
        if let Some(u) = imds {
            crate::endpoint::check_test_override(u)?;
        }
        if let Some(h) = gce_host {
            crate::endpoint::check_test_override(&format!("http://{h}"))?;
        }
        self.imds_endpoint = imds.map(String::from);
        self.gce_host = gce_host.map(String::from);
        Ok(self)
    }

    fn env_aws(&self) -> Option<AwsCredentials> {
        self.fixed_aws.clone().or_else(from_env)
    }

    pub fn source(&self) -> CredentialSource {
        self.source
    }

    /// Authorisation for the bucket.
    pub async fn storage(&self) -> Result<Auth> {
        match self.source {
            CredentialSource::Env => {
                if let Some(c) = self.env_aws() {
                    return Ok(Auth::SigV4(c));
                }
                if let Some(t) = env("GOOGLE_OAUTH_ACCESS_TOKEN") {
                    return Ok(Auth::Bearer(Secret::new(t)));
                }
                bail!(
                    "credentials = \"env\": set AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY \
                     (or GOOGLE_OAUTH_ACCESS_TOKEN) in the environment"
                )
            }
            CredentialSource::AwsInstance => Ok(Auth::SigV4(self.aws().await?)),
            CredentialSource::GcpMetadata => Ok(Auth::Bearer(self.google().await?)),
            CredentialSource::Auto => {
                if let Some(c) = self.env_aws() {
                    Ok(Auth::SigV4(c))
                } else if self.google_endpoint {
                    Ok(Auth::Bearer(self.google().await?))
                } else {
                    Ok(Auth::SigV4(self.aws().await.context(
                        "no AWS_ACCESS_KEY_ID in the environment and no instance role \
                         (credentials = \"auto\")",
                    )?))
                }
            }
        }
    }

    /// AWS credentials (CloudFront): the environment, else the instance role.
    pub async fn aws(&self) -> Result<AwsCredentials> {
        if self.source != CredentialSource::AwsInstance
            && let Some(c) = self.env_aws()
        {
            return Ok(c);
        }
        if self.source == CredentialSource::Env {
            bail!("credentials = \"env\": set AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY");
        }
        let mut cached = self.aws.lock().await;
        if let Some(c) = cached.as_ref()
            && fresh(c.expires)
        {
            return Ok(c.clone());
        }
        let c = self.fetch_aws_role().await?;
        *cached = Some(c.clone());
        Ok(c)
    }

    /// A Google bearer token: `GOOGLE_OAUTH_ACCESS_TOKEN`, else the
    /// metadata server.
    pub async fn google(&self) -> Result<Secret> {
        if self.source != CredentialSource::GcpMetadata
            && let Some(t) = env("GOOGLE_OAUTH_ACCESS_TOKEN")
        {
            return Ok(Secret::new(t));
        }
        if self.source == CredentialSource::Env {
            bail!("credentials = \"env\": set GOOGLE_OAUTH_ACCESS_TOKEN");
        }
        let mut cached = self.google.lock().await;
        if let Some((t, exp)) = cached.as_ref()
            && fresh(*exp)
        {
            return Ok(t.clone());
        }
        let host = self
            .gce_host
            .clone()
            .unwrap_or_else(|| "metadata.google.internal".into());
        let url =
            format!("http://{host}/computeMetadata/v1/instance/service-accounts/default/token");
        let resp = self
            .http
            .get(&url)
            .header("Metadata-Flavor", "Google")
            .send()
            .await
            .context("asking the GCE metadata server for a service account token")?;
        if !resp.status().is_success() {
            bail!(
                "the GCE metadata server answered {} for a service account token",
                resp.status()
            );
        }
        let tok: GoogleToken = resp
            .json()
            .await
            .context("reading the service account token")?;
        let exp = tok
            .expires_in
            .map(|s| Utc::now() + chrono::Duration::seconds(s));
        let t = Secret::new(tok.access_token);
        *cached = Some((t.clone(), exp));
        Ok(t)
    }

    async fn fetch_aws_role(&self) -> Result<AwsCredentials> {
        let doc: RoleDoc = if self.imds_endpoint.is_some() {
            self.imds().await?
        } else if let Some(rel) = env("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI") {
            self.get_json(&format!("http://169.254.170.2{rel}"), None)
                .await
                .context("reading the ECS task role")?
        } else if let Some(full) = env("AWS_CONTAINER_CREDENTIALS_FULL_URI") {
            check_container_uri(&full)?;
            let token = env("AWS_CONTAINER_AUTHORIZATION_TOKEN");
            self.get_json(&full, token.as_deref().map(|t| ("authorization", t)))
                .await
                .context("reading the container credentials")?
        } else {
            self.imds().await?
        };
        let expires = doc
            .expiration
            .as_deref()
            .and_then(|e| DateTime::parse_from_rfc3339(e).ok())
            .map(|d| d.with_timezone(&Utc));
        let mut c = AwsCredentials::new(
            &doc.access_key_id,
            &doc.secret_access_key,
            doc.token.as_deref(),
        );
        c.expires = expires;
        Ok(c)
    }

    async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        header: Option<(&str, &str)>,
    ) -> Result<T> {
        let mut req = self.http.get(url);
        if let Some((k, v)) = header {
            req = req.header(k, v);
        }
        let resp = req.send().await?;
        if !resp.status().is_success() {
            bail!("{url} answered {}", resp.status());
        }
        Ok(resp.json().await?)
    }

    /// EC2 instance metadata, version 2 (a session token first).
    async fn imds(&self) -> Result<RoleDoc> {
        let base = self
            .imds_endpoint
            .clone()
            .unwrap_or_else(|| "http://169.254.169.254".into());
        let base = base.trim_end_matches('/');
        let token = self
            .http
            .put(format!("{base}/latest/api/token"))
            .header("x-aws-ec2-metadata-token-ttl-seconds", "21600")
            .send()
            .await
            .context("no instance metadata service (IMDSv2) answered")?;
        if !token.status().is_success() {
            bail!("the instance metadata service answered {}", token.status());
        }
        let token = token.text().await?;
        let list = self
            .http
            .get(format!("{base}/latest/meta-data/iam/security-credentials/"))
            .header("x-aws-ec2-metadata-token", &token)
            .send()
            .await?;
        if !list.status().is_success() {
            bail!(
                "this instance has no IAM role (instance metadata answered {})",
                list.status()
            );
        }
        let list = list.text().await?;
        let Some(role) = list.lines().map(str::trim).find(|l| !l.is_empty()) else {
            bail!("this instance has no IAM role");
        };
        let resp = self
            .http
            .get(format!(
                "{base}/latest/meta-data/iam/security-credentials/{role}"
            ))
            .header("x-aws-ec2-metadata-token", &token)
            .send()
            .await?;
        if !resp.status().is_success() {
            bail!(
                "instance metadata answered {} for the role's credentials",
                resp.status()
            );
        }
        resp.json()
            .await
            .context("reading the instance role's credentials")
    }
}

fn from_env() -> Option<AwsCredentials> {
    let id = env("AWS_ACCESS_KEY_ID")?;
    let secret = env("AWS_SECRET_ACCESS_KEY")?;
    let token = env("AWS_SESSION_TOKEN");
    Some(AwsCredentials::new(&id, &secret, token.as_deref()))
}

/// Like the AWS SDKs: a full container credentials URI must be HTTPS or
/// point at this machine or the ECS/EKS agent addresses.
fn check_container_uri(uri: &str) -> Result<()> {
    if uri.starts_with("https://") {
        return Ok(());
    }
    let Some(rest) = uri.strip_prefix("http://") else {
        bail!("AWS_CONTAINER_CREDENTIALS_FULL_URI must be an http(s) URL");
    };
    let host = rest.split(['/', ':']).next().unwrap_or("");
    if matches!(
        host,
        "localhost" | "127.0.0.1" | "[::1]" | "169.254.170.2" | "169.254.170.23"
    ) {
        Ok(())
    } else {
        bail!("AWS_CONTAINER_CREDENTIALS_FULL_URI over plain HTTP must point at this machine")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_never_print() {
        let c = AwsCredentials::new("AKIDEXAMPLE", "very-secret-key", Some("session-tok"));
        let shown = format!("{c:?} {:?}", Auth::SigV4(c.clone()));
        assert!(!shown.contains("very-secret-key"), "{shown}");
        assert!(!shown.contains("session-tok"), "{shown}");
        assert!(shown.contains("<redacted>"));
        let b = format!(
            "{:?} {}",
            Auth::Bearer(Secret::new("ya29.tok")),
            Secret::new("x")
        );
        assert!(!b.contains("ya29"), "{b}");
    }

    #[test]
    fn container_uris() {
        assert!(check_container_uri("http://127.0.0.1:8080/creds").is_ok());
        assert!(check_container_uri("http://169.254.170.23/v1/credentials").is_ok());
        assert!(check_container_uri("https://example.org/creds").is_ok());
        assert!(check_container_uri("http://evil.example/creds").is_err());
    }
}
