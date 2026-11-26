//! Reading messages back from a local Mailpit SMTP sink in tests
//! (`deploy/compose.mailpit.yml`).
//!
//! Tests that need it read `ENGINE_TEST_SMTP_URL` (where to send) and
//! `ENGINE_TEST_MAILPIT_URL` (Mailpit's HTTP API) and skip without them.
//! Recipients are unique `@example.test` addresses (a reserved domain), and
//! Mailpit relays nothing, so no test mail can leave the machine.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::Value as Json;

pub const SMTP_ENV: &str = "ENGINE_TEST_SMTP_URL";
pub const MAILPIT_ENV: &str = "ENGINE_TEST_MAILPIT_URL";

/// Mailpit's API, or `None` (after saying why) when not configured.
#[derive(Debug, Clone)]
pub struct Mailpit {
    pub smtp_url: String,
    host: String,
    port: u16,
}

/// A message as read back from Mailpit.
#[derive(Debug, Clone)]
pub struct Received {
    pub id: String,
    pub subject: String,
    pub to: Vec<String>,
    pub from: String,
    pub text: String,
    pub html: String,
}

impl Mailpit {
    pub fn from_env(test: &str) -> Option<Mailpit> {
        let (Ok(smtp_url), Ok(api)) = (std::env::var(SMTP_ENV), std::env::var(MAILPIT_ENV)) else {
            eprintln!(
                "skipping {test}: set {SMTP_ENV} and {MAILPIT_ENV} (scripts/test-postgres.sh)"
            );
            return None;
        };
        let hostport = api
            .trim_start_matches("http://")
            .trim_end_matches('/')
            .to_string();
        let (host, port) = hostport
            .rsplit_once(':')
            .map(|(h, p)| (h.to_string(), p.parse().unwrap_or(80)))
            .unwrap_or((hostport.clone(), 80));
        Some(Mailpit {
            smtp_url,
            host,
            port,
        })
    }

    /// A fresh recipient address nobody else uses.
    pub fn address(prefix: &str) -> String {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("{prefix}-{}-{n}@example.test", std::process::id())
    }

    pub fn get(&self, path: &str) -> Result<Json> {
        let mut s = TcpStream::connect((self.host.as_str(), self.port))
            .with_context(|| format!("connecting to Mailpit at {}:{}", self.host, self.port))?;
        s.set_read_timeout(Some(Duration::from_secs(10)))?;
        write!(
            s,
            "GET {path} HTTP/1.1\r\nHost: {}:{}\r\nAccept: application/json\r\nConnection: close\r\n\r\n",
            self.host, self.port
        )?;
        let mut raw = Vec::new();
        s.read_to_end(&mut raw)?;
        let split = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .context("bad HTTP response")?;
        let head = String::from_utf8_lossy(&raw[..split]).to_lowercase();
        let mut body = raw[split + 4..].to_vec();
        if head.contains("transfer-encoding: chunked") {
            body = dechunk(&body);
        }
        if !head.starts_with("http/1.1 200") {
            bail!("Mailpit {path}: {}", head.lines().next().unwrap_or(""));
        }
        Ok(serde_json::from_slice(&body)?)
    }

    /// Messages to `address`, newest first.
    pub fn messages_to(&self, address: &str) -> Result<Vec<Received>> {
        let q: String = format!("to:\"{address}\"")
            .bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_') {
                    (b as char).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect();
        let list = self.get(&format!("/api/v1/search?query={q}"))?;
        let mut out = Vec::new();
        for m in list["messages"].as_array().cloned().unwrap_or_default() {
            let id = m["ID"].as_str().unwrap_or("").to_string();
            let full = self.get(&format!("/api/v1/message/{id}"))?;
            out.push(Received {
                id,
                subject: full["Subject"].as_str().unwrap_or("").to_string(),
                to: full["To"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|t| t["Address"].as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default(),
                from: full["From"]["Address"].as_str().unwrap_or("").to_string(),
                text: full["Text"].as_str().unwrap_or("").to_string(),
                html: full["HTML"].as_str().unwrap_or("").to_string(),
            });
        }
        Ok(out)
    }

    /// Wait up to `timeout` for `n` messages to `address`.
    pub fn wait_for(&self, address: &str, n: usize, timeout: Duration) -> Result<Vec<Received>> {
        let start = Instant::now();
        loop {
            let got = self.messages_to(address)?;
            if got.len() >= n {
                return Ok(got);
            }
            if start.elapsed() > timeout {
                bail!(
                    "expected {n} message(s) to {address} within {timeout:?}, got {}",
                    got.len()
                );
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

fn dechunk(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(i) = rest.windows(2).position(|w| w == b"\r\n") {
        let size =
            usize::from_str_radix(String::from_utf8_lossy(&rest[..i]).trim(), 16).unwrap_or(0);
        if size == 0 || rest.len() < i + 2 + size {
            break;
        }
        out.extend_from_slice(&rest[i + 2..i + 2 + size]);
        rest = &rest[i + 2 + size..];
        if rest.starts_with(b"\r\n") {
            rest = &rest[2..];
        }
    }
    out
}
