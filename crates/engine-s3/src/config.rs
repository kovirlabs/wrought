//! `[publish]` with `target = "s3"`, and `[publish.cdn]`.
//!
//! ```toml
//! [publish]
//! target = "s3"
//! bucket = "example-site"
//! prefix = ""                 # optional key prefix, e.g. "www/"
//! region = "eu-west-1"
//! endpoint = "https://..."    # optional: R2, B2, GCS, MinIO; AWS when absent
//! path_style = true           # optional; default: true with an endpoint
//! credentials = "auto"        # auto | env | aws-instance | gcp-metadata
//! html_max_age = 60           # Cache-Control of HTML pages, seconds
//! asset_max_age = 3600        # other files that are not fingerprinted
//!
//! [publish.cdn]
//! provider = "cloudfront"     # none | log | file | cloudfront | google
//! distribution_id = "E2ABC..."
//! ```
//!
//! Keys are never accepted here: a setting that looks like one is refused
//! with a message saying where credentials come from instead.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use serde::Deserialize;

use crate::creds::CredentialSource;

/// Setting names that would hold a key, a secret or a token.
const KEY_LIKE: &[&str] = &[
    "secret",
    "access_key",
    "accesskey",
    "key_id",
    "keyid",
    "token",
    "password",
    "passwd",
    "hmac",
    "private_key",
    "api_key",
    "apikey",
    "auth",
];

fn refuse_key_like(section: &str, t: &toml::Table) -> Result<()> {
    for k in t.keys() {
        let lower = k.to_ascii_lowercase();
        if KEY_LIKE.iter().any(|w| lower.contains(w)) || lower == "key" {
            bail!(
                "[{section}] `{k}` looks like a credential. Keys and tokens never go in \
                 site.toml: set AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY (and \
                 AWS_SESSION_TOKEN) in the environment, or let Wrought use the machine's role \
                 or service account (credentials = \"auto\", \"aws-instance\" or \
                 \"gcp-metadata\")"
            );
        }
    }
    Ok(())
}

/// Which CDN to tell about changed paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CdnProvider {
    /// No CDN (or one with short lifetimes only).
    #[default]
    None,
    /// Print the paths that would be invalidated.
    Log,
    /// Append each batch as a JSON line to a file (for scripts and tests).
    File,
    /// Amazon CloudFront (`CreateInvalidation`).
    Cloudfront,
    /// Google Cloud CDN (`urlMaps.invalidateCache`).
    Google,
}

/// Which URL forms of a page to invalidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IndexPaths {
    /// `/articles/x/` and `/articles/x/index.html`.
    #[default]
    Both,
    /// `/articles/x/` only (Cloud CDN with a backend bucket: the cache key
    /// is the requested URL).
    Directory,
    /// `/articles/x/index.html` only (a CDN whose cache key is the
    /// rewritten object path).
    File,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCdn {
    #[serde(default)]
    provider: CdnProvider,
    distribution_id: Option<String>,
    project: Option<String>,
    url_map: Option<String>,
    host: Option<String>,
    file: Option<String>,
    max_paths: Option<usize>,
    #[serde(default)]
    index_paths: IndexPaths,
}

/// `[publish.cdn]`.
#[derive(Debug, Clone, Default)]
pub struct CdnConfig {
    pub provider: CdnProvider,
    /// CloudFront distribution.
    pub distribution_id: String,
    /// Google Cloud project and URL map.
    pub project: String,
    pub url_map: String,
    /// Google: only invalidate for this host (optional).
    pub host: Option<String>,
    /// `file` provider: where batches are appended.
    pub file: Option<PathBuf>,
    /// Above this many paths, one wildcard (`/*`) is sent instead.
    pub max_paths: usize,
    pub index_paths: IndexPaths,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    #[allow(dead_code)]
    target: String,
    bucket: Option<String>,
    #[serde(default)]
    prefix: String,
    endpoint: Option<String>,
    region: Option<String>,
    path_style: Option<bool>,
    #[serde(default)]
    credentials: Option<toml::Value>,
    html_max_age: Option<u32>,
    asset_max_age: Option<u32>,
    #[serde(default)]
    cdn: Option<toml::Table>,
}

/// Bucket target settings.
#[derive(Debug, Clone)]
pub struct S3Config {
    pub bucket: String,
    /// `""` or a prefix ending in `/`.
    pub prefix: String,
    /// Scheme and host (and port), no trailing `/`.
    pub endpoint: String,
    pub region: String,
    pub path_style: bool,
    pub credentials: CredentialSource,
    pub html_max_age: u32,
    pub asset_max_age: u32,
    pub cdn: CdnConfig,
}

fn valid_bucket(b: &str) -> bool {
    (3..=63).contains(&b.len())
        && b.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'.')
        && !b.starts_with(['-', '.'])
        && !b.ends_with(['-', '.'])
}

/// Endpoints must be plain `http(s)://host[:port]`: no user, password,
/// path or query (and the value is never echoed, in case it held a key).
fn check_endpoint(what: &str, e: &str) -> Result<String> {
    let e = e.trim_end_matches('/');
    let Some(rest) = e
        .strip_prefix("https://")
        .or_else(|| e.strip_prefix("http://"))
    else {
        bail!("{what} must start with https:// (or http:// for a local server)");
    };
    if rest.is_empty() || rest.contains(['@', '/', '?', '#', ' ']) {
        bail!("{what} must be a plain scheme and host (no user, password, path or query)");
    }
    // Signed requests go over HTTPS, or plain HTTP to this machine only.
    crate::endpoint::check_sigv4(e).map_err(|err| anyhow::anyhow!("{what}: {err}"))?;
    Ok(e.to_string())
}

impl S3Config {
    /// Parse `[publish]` (its `target` must be `"s3"`). `site` resolves
    /// the `file` CDN provider's path.
    pub fn from_site(section: Option<&toml::Table>, site: &Path) -> Result<Self> {
        let Some(t) = section else {
            bail!("[publish] is missing");
        };
        refuse_key_like("publish", t)?;
        if let Some(toml::Value::Table(cdn)) = t.get("cdn") {
            refuse_key_like("publish.cdn", cdn)?;
        }
        let raw: Raw = t
            .clone()
            .try_into()
            .map_err(|e| anyhow::anyhow!("[publish]: {e}"))?;
        if raw.target != "s3" {
            bail!("[publish] target is not \"s3\"");
        }
        let Some(bucket) = raw.bucket else {
            bail!("[publish] target = \"s3\" needs `bucket`");
        };
        if !valid_bucket(&bucket) {
            bail!(
                "[publish] bucket must be 3 to 63 lowercase letters, digits, `-` and `.` \
                 (an S3 bucket name)"
            );
        }
        let prefix = raw.prefix.trim_matches('/').to_string();
        if prefix
            .split('/')
            .any(|s| s == ".." || s == "." || (s.is_empty() && !prefix.is_empty()))
            || prefix.contains(['\\', '?', '#'])
        {
            bail!("[publish] prefix must be a plain path like \"www\" or \"sites/www\"");
        }
        let prefix = if prefix.is_empty() {
            prefix
        } else {
            format!("{prefix}/")
        };
        let credentials = match raw.credentials {
            None => CredentialSource::Auto,
            Some(toml::Value::String(s)) => match CredentialSource::parse(&s) {
                Some(c) => c,
                // Not echoed: someone may have pasted a key here.
                None => bail!(
                    "[publish] credentials must be one of {} (keys never go in site.toml; \
                     set AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY in the environment)",
                    CredentialSource::NAMES.join(", ")
                ),
            },
            Some(_) => bail!(
                "[publish] credentials must be one of {}",
                CredentialSource::NAMES.join(", ")
            ),
        };
        let (endpoint, region, default_path_style) = match raw.endpoint {
            Some(e) => {
                let e = check_endpoint("[publish] endpoint", &e)?;
                (e, raw.region.unwrap_or_else(|| "us-east-1".into()), true)
            }
            None => {
                let Some(region) = raw.region else {
                    bail!(
                        "[publish] target = \"s3\" needs `region` (or `endpoint` for a store \
                         other than AWS S3)"
                    );
                };
                if !region
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
                {
                    bail!("[publish] region must look like eu-west-1");
                }
                (format!("https://s3.{region}.amazonaws.com"), region, false)
            }
        };
        // Virtual-hosted addressing breaks TLS for bucket names with dots.
        let path_style = raw.path_style.unwrap_or(default_path_style) || bucket.contains('.');
        let cdn = match raw.cdn {
            None => CdnConfig {
                max_paths: 1000,
                ..Default::default()
            },
            Some(c) => cdn_config(c, site)?,
        };
        Ok(S3Config {
            bucket,
            prefix,
            endpoint,
            region,
            path_style,
            credentials,
            html_max_age: raw.html_max_age.unwrap_or(60),
            asset_max_age: raw.asset_max_age.unwrap_or(3600),
            cdn,
        })
    }

    /// Whether the bucket is Google Cloud Storage (bearer tokens work).
    pub fn is_google(&self) -> bool {
        self.endpoint == format!("https://{}", crate::endpoint::GOOGLE_STORAGE_HOST)
    }
}

fn cdn_config(t: toml::Table, site: &Path) -> Result<CdnConfig> {
    let raw: RawCdn = t
        .try_into()
        .map_err(|e| anyhow::anyhow!("[publish.cdn]: {e}"))?;
    let need = |v: Option<String>, name: &str| -> Result<String> {
        match v.filter(|s| !s.trim().is_empty()) {
            Some(s) => Ok(s),
            None => bail!(
                "[publish.cdn] provider = \"{}\" needs `{name}`",
                format!("{:?}", raw.provider).to_lowercase()
            ),
        }
    };
    let mut c = CdnConfig {
        provider: raw.provider,
        max_paths: raw.max_paths.unwrap_or(1000).clamp(1, 3000),
        index_paths: raw.index_paths,
        host: raw.host.clone(),
        ..Default::default()
    };
    match raw.provider {
        CdnProvider::None | CdnProvider::Log => {}
        CdnProvider::File => {
            let f = PathBuf::from(need(raw.file.clone(), "file")?);
            c.file = Some(if f.is_absolute() { f } else { site.join(f) });
        }
        CdnProvider::Cloudfront => {
            let id = need(raw.distribution_id.clone(), "distribution_id")?;
            if !id.bytes().all(|b| b.is_ascii_alphanumeric()) {
                bail!("[publish.cdn] distribution_id must be letters and digits");
            }
            c.distribution_id = id;
        }
        CdnProvider::Google => {
            let ok = |s: &str| {
                s.bytes().all(|b| {
                    b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b':'
                })
            };
            c.project = need(raw.project.clone(), "project")?;
            c.url_map = need(raw.url_map.clone(), "url_map")?;
            if !ok(&c.project) || !ok(&c.url_map) {
                bail!("[publish.cdn] project and url_map must be plain names");
            }
        }
    }
    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<S3Config> {
        let t: toml::Table = toml::from_str(s).unwrap();
        S3Config::from_site(Some(&t), Path::new("/srv/site"))
    }

    #[test]
    fn minimal_aws_and_endpoint() {
        let c = parse("target='s3'\nbucket='example-site'\nregion='eu-west-1'").unwrap();
        assert_eq!(c.endpoint, "https://s3.eu-west-1.amazonaws.com");
        assert!(!c.path_style);
        assert_eq!(c.prefix, "");
        assert_eq!(c.cdn.provider, CdnProvider::None);
        assert_eq!((c.html_max_age, c.asset_max_age), (60, 3600));
        let c = parse(
            "target='s3'\nbucket='b.example'\nendpoint='http://127.0.0.1:59000/'\nprefix='/www/'",
        )
        .unwrap();
        assert_eq!(c.endpoint, "http://127.0.0.1:59000");
        assert!(c.path_style);
        assert_eq!(c.prefix, "www/");
        assert_eq!(c.region, "us-east-1");
    }

    #[test]
    fn refuses_bad_settings() {
        for (toml, want) in [
            ("target='s3'\nregion='x'", "needs `bucket`"),
            ("target='s3'\nbucket='b1x'", "needs `region`"),
            ("target='s3'\nbucket='B'\nregion='x'", "bucket must"),
            (
                "target='s3'\nbucket='bbb'\nregion='x'\nprefix='../x'",
                "prefix",
            ),
            (
                "target='s3'\nbucket='bbb'\nendpoint='https://AKIA:secret@host'",
                "plain scheme and host",
            ),
            (
                "target='s3'\nbucket='bbb'\nregion='x'\n[cdn]\nprovider='cloudfront'",
                "needs `distribution_id`",
            ),
            (
                "target='s3'\nbucket='bbb'\nregion='x'\n[cdn]\nprovider='google'\nproject='p'",
                "needs `url_map`",
            ),
            (
                "target='s3'\nbucket='bbb'\nregion='x'\ntypo=1",
                "unknown field",
            ),
        ] {
            let err = format!("{:#}", parse(toml).unwrap_err());
            assert!(err.contains(want), "{toml}: {err}");
            assert!(!err.contains("secret@"), "{err}");
        }
    }

    #[test]
    fn refuses_keys_without_echoing_them() {
        for toml in [
            "target='s3'\nbucket='bbb'\nregion='x'\naccess_key_id='AKIAEXAMPLE'",
            "target='s3'\nbucket='bbb'\nregion='x'\nsecret_access_key='s3cr3t'",
            "target='s3'\nbucket='bbb'\nregion='x'\nsession_token='s3cr3t'",
            "target='s3'\nbucket='bbb'\nregion='x'\n[cdn]\nprovider='google'\naccess_token='s3cr3t'",
            "target='s3'\nbucket='bbb'\nregion='x'\ncredentials='AKIAEXAMPLE:s3cr3t'",
            "target='s3'\nbucket='bbb'\nregion='x'\n[credentials]\nkey='s3cr3t'",
        ] {
            let err = format!("{:#}", parse(toml).unwrap_err());
            assert!(
                err.contains("AWS_ACCESS_KEY_ID") || err.contains("credentials must"),
                "{toml}: {err}"
            );
            assert!(
                !err.contains("s3cr3t") && !err.contains("AKIAEXAMPLE"),
                "{err}"
            );
        }
    }

    #[test]
    fn cdn_settings() {
        let c = parse(
            "target='s3'\nbucket='bbb'\nregion='x'\n[cdn]\nprovider='file'\nfile='cdn.log'\n\
             index_paths='directory'\nmax_paths=5",
        )
        .unwrap();
        assert_eq!(c.cdn.provider, CdnProvider::File);
        assert_eq!(c.cdn.file.as_deref(), Some(Path::new("/srv/site/cdn.log")));
        assert_eq!(c.cdn.index_paths, IndexPaths::Directory);
        assert_eq!(c.cdn.max_paths, 5);
        let c = parse(
            "target='s3'\nbucket='bbb'\nregion='x'\ncredentials='gcp-metadata'\n[cdn]\n\
             provider='google'\nproject='my-proj'\nurl_map='site-map'",
        )
        .unwrap();
        assert_eq!(c.credentials, CredentialSource::GcpMetadata);
        assert_eq!(c.cdn.url_map, "site-map");
    }
}
