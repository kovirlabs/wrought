//! AWS Signature Version 4, sans-IO: a request description in, the
//! `Authorization` header (and the headers it signs) out. Used for S3 (and
//! every S3-compatible store that speaks SigV4: MinIO, R2, B2, GCS with HMAC
//! interoperability keys) and for the CloudFront API.

use hmac::{Hmac, Mac};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use sha2::{Digest, Sha256};

use crate::creds::AwsCredentials;

/// Characters SigV4 leaves alone: `A-Z a-z 0-9 - . _ ~`.
const UNRESERVED: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');
/// The same, keeping `/` (object key paths).
const PATH: &AsciiSet = &UNRESERVED.remove(b'/');

/// URI-encode a query component or header-free value (`/` encoded).
pub fn encode(s: &str) -> String {
    utf8_percent_encode(s, UNRESERVED).to_string()
}

/// URI-encode a path (`/` kept).
pub fn encode_path(s: &str) -> String {
    utf8_percent_encode(s, PATH).to_string()
}

pub fn sha256_hex(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut m = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes any key length");
    m.update(data);
    m.finalize().into_bytes().to_vec()
}

/// One request to sign. `path` is already URI-encoded; `query` pairs are
/// raw (encoded here); `headers` are the ones to sign besides `host`,
/// `x-amz-date`, `x-amz-content-sha256` and `x-amz-security-token`, which
/// [`sign`] adds.
pub struct Request<'a> {
    pub method: &'a str,
    /// Host as sent in the `Host` header (with `:port` if not the default).
    pub host: &'a str,
    pub path: &'a str,
    pub query: &'a [(String, String)],
    pub headers: &'a [(String, String)],
    /// Hex SHA-256 of the body.
    pub payload_sha256: &'a str,
}

/// The canonical query string: pairs sorted, both sides encoded.
pub fn canonical_query(query: &[(String, String)]) -> String {
    let mut q: Vec<(String, String)> = query.iter().map(|(k, v)| (encode(k), encode(v))).collect();
    q.sort();
    q.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// Sign `req` at `amz_date` (`YYYYMMDDTHHMMSSZ`). Returns every header to
/// send (the signed ones and `authorization`); `x-amz-content-sha256` is
/// only included for S3 (`include_content_sha` true).
pub fn sign(
    req: &Request<'_>,
    creds: &AwsCredentials,
    region: &str,
    service: &str,
    amz_date: &str,
    include_content_sha: bool,
) -> Vec<(String, String)> {
    let date = &amz_date[..8];
    let mut headers: Vec<(String, String)> = req
        .headers
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    headers.push(("host".into(), req.host.to_string()));
    headers.push(("x-amz-date".into(), amz_date.to_string()));
    if include_content_sha {
        headers.push((
            "x-amz-content-sha256".into(),
            req.payload_sha256.to_string(),
        ));
    }
    if let Some(t) = creds.session_token() {
        headers.push(("x-amz-security-token".into(), t.to_string()));
    }
    headers.sort();
    let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed_headers = headers
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let canonical = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        req.method,
        req.path,
        canonical_query(req.query),
        canonical_headers,
        signed_headers,
        req.payload_sha256
    );
    let scope = format!("{date}/{region}/{service}/aws4_request");
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        sha256_hex(canonical.as_bytes())
    );
    let k_date = hmac(
        format!("AWS4{}", creds.secret_access_key()).as_bytes(),
        date.as_bytes(),
    );
    let k_region = hmac(&k_date, region.as_bytes());
    let k_service = hmac(&k_region, service.as_bytes());
    let k_signing = hmac(&k_service, b"aws4_request");
    let signature: String = hmac(&k_signing, to_sign.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let auth = format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        creds.access_key_id()
    );
    headers.retain(|(k, _)| k != "host");
    headers.push(("authorization".into(), auth));
    headers
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth_of(h: &[(String, String)]) -> &str {
        &h.iter().find(|(k, _)| k == "authorization").unwrap().1
    }

    /// `get-vanilla` from the AWS SigV4 test suite.
    #[test]
    fn aws_test_suite_get_vanilla() {
        let creds = AwsCredentials::new(
            "AKIDEXAMPLE",
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            None,
        );
        let h = sign(
            &Request {
                method: "GET",
                host: "example.amazonaws.com",
                path: "/",
                query: &[],
                headers: &[],
                payload_sha256: &sha256_hex(b""),
            },
            &creds,
            "us-east-1",
            "service",
            "20150830T123600Z",
            false,
        );
        assert_eq!(
            auth_of(&h),
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, \
             SignedHeaders=host;x-amz-date, \
             Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
    }

    /// The "GET Object" example of the S3 SigV4 documentation.
    #[test]
    fn s3_documentation_get_object() {
        let creds = AwsCredentials::new(
            "AKIAIOSFODNN7EXAMPLE",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            None,
        );
        let h = sign(
            &Request {
                method: "GET",
                host: "examplebucket.s3.amazonaws.com",
                path: "/test.txt",
                query: &[],
                headers: &[("Range".into(), "bytes=0-9".into())],
                payload_sha256: &sha256_hex(b""),
            },
            &creds,
            "us-east-1",
            "s3",
            "20130524T000000Z",
            true,
        );
        assert_eq!(
            auth_of(&h),
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, \
             SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, \
             Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
    }

    #[test]
    fn encoding() {
        assert_eq!(encode("a b/c~"), "a%20b%2Fc~");
        assert_eq!(encode_path("/a b/é.html"), "/a%20b/%C3%A9.html");
        assert_eq!(
            canonical_query(&[
                ("prefix".into(), "x/".into()),
                ("list-type".into(), "2".into())
            ]),
            "list-type=2&prefix=x%2F"
        );
    }
}
