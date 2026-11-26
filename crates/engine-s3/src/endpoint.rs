//! Where credentials may be sent.
//!
//! Every request that carries a credential is checked here first:
//!
//! * a **bearer token** (a Google service account token) only goes over
//!   HTTPS to a host on an exact allow-list (`storage.googleapis.com`,
//!   `compute.googleapis.com`); no suffix or substring matching, no user
//!   info, no other port;
//! * a **SigV4 signature** only goes over HTTPS, or over plain HTTP to this
//!   machine (127.0.0.0/8, `::1`, `localhost`: a local MinIO);
//! * URLs with user info (`user:pass@`) are refused outright.
//!
//! The HTTP clients that carry credentials never follow redirects either,
//! so a store or API cannot bounce a signed request elsewhere.

use std::net::IpAddr;

use anyhow::{Result, bail};
use reqwest::Url;

/// Google hosts a bearer token may be sent to.
pub const GOOGLE_STORAGE_HOST: &str = "storage.googleapis.com";
pub const GOOGLE_COMPUTE_HOST: &str = "compute.googleapis.com";
/// The CloudFront API.
pub const CLOUDFRONT_HOST: &str = "cloudfront.amazonaws.com";

/// A URL, parsed, without user info.
fn parse(url: &str) -> Result<Url> {
    let Ok(u) = Url::parse(url) else {
        bail!("not a valid URL");
    };
    if !u.username().is_empty() || u.password().is_some() {
        bail!("URLs with a user or password are refused");
    }
    if u.host().is_none() {
        bail!("the URL has no host");
    }
    Ok(u)
}

/// Whether the URL's host is this machine.
pub fn is_loopback(u: &Url) -> bool {
    match u.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => IpAddr::V4(ip).is_loopback(),
        Some(url::Host::Ipv6(ip)) => IpAddr::V6(ip).is_loopback(),
        None => false,
    }
}

/// Check a URL a SigV4-signed request goes to: HTTPS, or HTTP to loopback.
pub fn check_sigv4(url: &str) -> Result<()> {
    let u = parse(url)?;
    match u.scheme() {
        "https" => Ok(()),
        "http" if is_loopback(&u) => Ok(()),
        "http" => bail!(
            "refusing to send signed requests over plain HTTP to {} (only to this machine, \
             for a local test server); use https://",
            u.host_str().unwrap_or("")
        ),
        s => bail!("unsupported URL scheme {s}"),
    }
}

/// Check a URL a bearer token goes to: HTTPS, default port, and a host
/// exactly in `allowed`.
pub fn check_bearer(url: &str, allowed: &[&str]) -> Result<()> {
    let u = parse(url)?;
    if u.scheme() != "https" {
        bail!("refusing to send a bearer token over plain HTTP");
    }
    let host = match u.host() {
        Some(url::Host::Domain(d)) => d.to_ascii_lowercase(),
        _ => bail!("refusing to send a bearer token to an IP address"),
    };
    if u.port().is_some() || !allowed.iter().any(|a| *a == host) {
        bail!(
            "refusing to send a bearer token to {host}: only {} may receive it",
            allowed.join(", ")
        );
    }
    Ok(())
}

/// Check a credential-carrying API URL whose host is fixed (CloudFront):
/// HTTPS to exactly `host`.
pub fn check_fixed(url: &str, host: &str) -> Result<()> {
    let u = parse(url)?;
    if u.scheme() != "https" || u.port().is_some() || u.host_str() != Some(host) {
        bail!("refusing to send credentials to anything but https://{host}");
    }
    Ok(())
}

/// Check a test endpoint override: plain loopback only (see the
/// `*_for_tests` constructors).
pub fn check_test_override(url: &str) -> Result<()> {
    let u = parse(url)?;
    if !is_loopback(&u) {
        bail!("test endpoint overrides must point at this machine");
    }
    Ok(())
}

/// An HTTP client for requests that carry credentials: no redirects.
pub fn client(timeout: std::time::Duration) -> Result<reqwest::Client> {
    use anyhow::Context;
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(10).min(timeout))
        .timeout(timeout)
        .build()
        .context("building the HTTP client")
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: &[&str] = &[GOOGLE_STORAGE_HOST];

    #[test]
    fn bearer_tokens_only_to_exact_google_hosts_over_https() {
        assert!(check_bearer("https://storage.googleapis.com/b/k", G).is_ok());
        assert!(check_bearer("https://STORAGE.googleapis.com/b", G).is_ok());
        for bad in [
            "http://storage.googleapis.com/b",
            "https://storage.googleapis.com.evil.test/b",
            "https://evil.test/storage.googleapis.com/b",
            "https://xstorage.googleapis.com/b",
            "https://bucket.storage.googleapis.com/k",
            "https://user@storage.googleapis.com/b",
            "https://u:p@storage.googleapis.com/b",
            "https://storage.googleapis.com:8443/b",
            "https://127.0.0.1/b",
            "http://127.0.0.1:59000/b",
            "https://compute.googleapis.com/x",
        ] {
            assert!(check_bearer(bad, G).is_err(), "{bad}");
        }
    }

    #[test]
    fn sigv4_over_http_only_to_loopback() {
        for ok in [
            "https://s3.eu-west-1.amazonaws.com/b",
            "https://example.r2.cloudflarestorage.com/b",
            "http://127.0.0.1:59000/b",
            "http://127.1.2.3/b",
            "http://localhost:9000/b",
            "http://[::1]:9000/b",
        ] {
            assert!(check_sigv4(ok).is_ok(), "{ok}");
        }
        for bad in [
            "http://minio.internal:9000/b",
            "http://10.0.0.5:9000/b",
            "http://localhost.evil.test/b",
            "http://127.0.0.1.evil.test/b",
            "https://key:secret@s3.amazonaws.com/b",
            "ftp://127.0.0.1/b",
        ] {
            assert!(check_sigv4(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn fixed_hosts() {
        let p = "https://cloudfront.amazonaws.com/2020-05-31/distribution/E1/invalidation";
        assert!(check_fixed(p, CLOUDFRONT_HOST).is_ok());
        for bad in [
            "http://cloudfront.amazonaws.com/x",
            "https://cloudfront.amazonaws.com.evil.test/x",
            "https://cloudfront.amazonaws.com:444/x",
            "https://a@cloudfront.amazonaws.com/x",
        ] {
            assert!(check_fixed(bad, CLOUDFRONT_HOST).is_err(), "{bad}");
        }
        assert!(check_test_override("http://127.0.0.1:1234").is_ok());
        assert!(check_test_override("https://evil.test").is_err());
    }
}
