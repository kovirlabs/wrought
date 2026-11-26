//! The test harness shared by the end-to-end tests: the real `engine`
//! binary against a throwaway database, driven over HTTP with cookie jars.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub const ENGINE: &str = env!("CARGO_BIN_EXE_engine");

/// The site key test servers get (encrypts TOTP secrets).
pub const SITE_KEY: &str = "test site key: 0123456789abcdef0123456789abcdef";

pub fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let name = e.file_name();
        if name == "public" || name.to_string_lossy().starts_with('.') {
            continue;
        }
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &to.join(&name));
        } else {
            std::fs::copy(e.path(), to.join(&name)).unwrap();
        }
    }
}

pub fn engine(db_url: &str, args: &[&str], stdin: Option<&str>) -> String {
    engine_env(db_url, args, stdin, &[])
}

/// [`engine`] with extra environment variables (e.g. bucket credentials).
pub fn engine_env(
    db_url: &str,
    args: &[&str],
    stdin: Option<&str>,
    env: &[(&str, &str)],
) -> String {
    let mut c = Command::new(ENGINE);
    for (k, v) in env {
        c.env(k, v);
    }
    let mut c = c
        .args(args)
        .env("ENGINE_DATABASE_URL", db_url)
        .env("ENGINE_DATABASE_MAX_CONNECTIONS", "3")
        .env("ENGINE_SECRET_KEY", SITE_KEY)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(s) = stdin {
        c.stdin.take().unwrap().write_all(s.as_bytes()).unwrap();
    }
    let out = c.wait_with_output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "engine {args:?} failed:\n{text}");
    text
}

pub struct Server {
    pub child: Child,
    pub log: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // A failing test shows what the server said: the only way to tell
        // a slow publish from a lost one after the fact.
        if std::thread::panicking() {
            // Straight to stderr: the test harness's capture would swallow
            // `eprintln!` from this (non-test) thread.
            let log = self.log();
            let tail: Vec<&str> = log.lines().rev().take(80).collect();
            let mut out = format!(
                "--- last lines of the engine's log ({}):\n",
                self.log.display()
            );
            for l in tail.into_iter().rev() {
                out.push_str(l);
                out.push('\n');
            }
            let _ = std::io::stderr().write_all(out.as_bytes());
        }
    }
}

impl Server {
    pub fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

#[derive(Debug)]
pub struct Resp {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub bytes: Vec<u8>,
}

impl Resp {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    pub fn location(&self) -> &str {
        self.header("location").unwrap_or("")
    }
}

/// A browser: one cookie jar.
pub struct Browser {
    pub port: u16,
    pub jar: BTreeMap<String, String>,
    /// Every Set-Cookie header seen (for flag checks).
    pub set_cookies: Vec<String>,
}

pub fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

pub fn dechunk(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(i) = rest.windows(2).position(|w| w == b"\r\n") {
        let size =
            usize::from_str_radix(String::from_utf8_lossy(&rest[..i]).trim(), 16).unwrap_or(0);
        if size == 0 {
            break;
        }
        out.extend_from_slice(&rest[i + 2..i + 2 + size]);
        rest = &rest[i + 2 + size + 2..];
    }
    out
}

impl Browser {
    pub fn new(port: u16) -> Self {
        Browser {
            port,
            jar: BTreeMap::new(),
            set_cookies: Vec::new(),
        }
    }

    pub fn request(
        &mut self,
        method: &str,
        path: &str,
        body: Option<String>,
        extra: &[(&str, &str)],
    ) -> Resp {
        let body = body.map(|b| {
            (
                b.into_bytes(),
                "application/x-www-form-urlencoded".to_string(),
            )
        });
        self.request_bytes(method, path, body, extra)
    }

    /// A request with any body and content type.
    pub fn request_bytes(
        &mut self,
        method: &str,
        path: &str,
        body: Option<(Vec<u8>, String)>,
        extra: &[(&str, &str)],
    ) -> Resp {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        let mut req = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost:{}\r\nConnection: close\r\nUser-Agent: wrought-test\r\n",
            self.port
        );
        if !self.jar.is_empty() {
            let c: Vec<String> = self.jar.iter().map(|(k, v)| format!("{k}={v}")).collect();
            req.push_str(&format!("Cookie: {}\r\n", c.join("; ")));
        }
        for (k, v) in extra {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        if let Some((b, ct)) = &body {
            req.push_str(&format!(
                "Content-Type: {ct}\r\nContent-Length: {}\r\n",
                b.len()
            ));
        }
        req.push_str("\r\n");
        let mut bytes = req.into_bytes();
        if let Some((b, _)) = &body {
            bytes.extend_from_slice(b);
        }
        // The server may answer before reading a refused body (413).
        let _ = s.write_all(&bytes);
        let mut raw = Vec::new();
        if let Err(e) = s.read_to_end(&mut raw) {
            panic!(
                "{method} {path}: no complete answer within 30 s ({e}); got {} bytes: {}",
                raw.len(),
                String::from_utf8_lossy(&raw[..raw.len().min(300)])
            );
        }
        let split = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("http response");
        let head = String::from_utf8_lossy(&raw[..split]).to_string();
        let mut body = raw[split + 4..].to_vec();
        let mut lines = head.lines();
        let status: u16 = lines
            .next()
            .unwrap()
            .split(' ')
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        let headers: Vec<(String, String)> = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            .collect();
        if headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("transfer-encoding") && v.contains("chunked"))
        {
            body = dechunk(&body);
        }
        for (k, v) in &headers {
            if k.eq_ignore_ascii_case("set-cookie") {
                self.set_cookies.push(v.clone());
                let (kv, attrs) = v.split_once(';').unwrap_or((v, ""));
                let (name, value) = kv.split_once('=').unwrap();
                if attrs.contains("Max-Age=0") {
                    self.jar.remove(name);
                } else {
                    self.jar.insert(name.to_string(), value.to_string());
                }
            }
        }
        Resp {
            status,
            headers,
            body: String::from_utf8_lossy(&body).to_string(),
            bytes: body,
        }
    }

    /// POST `multipart/form-data`: text fields and files (field, file
    /// name, bytes).
    pub fn multipart(
        &mut self,
        path: &str,
        fields: &[(&str, &str)],
        files: &[(&str, &str, &[u8])],
    ) -> Resp {
        let boundary = "----wroughttestboundary7MA4YWxkTrZu0gW";
        let mut b: Vec<u8> = Vec::new();
        for (k, v) in fields {
            b.extend_from_slice(
                format!(
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n"
                )
                .as_bytes(),
            );
        }
        for (k, name, data) in files {
            b.extend_from_slice(
                format!(
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"; filename=\"{name}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
                )
                .as_bytes(),
            );
            b.extend_from_slice(data);
            b.extend_from_slice(b"\r\n");
        }
        b.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        self.request_bytes(
            "POST",
            path,
            Some((b, format!("multipart/form-data; boundary={boundary}"))),
            &[],
        )
    }

    /// A JSON API call with a bearer token.
    pub fn api(
        &mut self,
        method: &str,
        path: &str,
        token: &str,
        body: Option<&str>,
        extra: &[(&str, &str)],
    ) -> Resp {
        let auth = format!("Bearer {token}");
        let mut h: Vec<(&str, &str)> = vec![("Authorization", auth.as_str())];
        h.extend_from_slice(extra);
        self.request_bytes(
            method,
            path,
            body.map(|b| (b.as_bytes().to_vec(), "application/json".to_string())),
            &h,
        )
    }

    pub fn get(&mut self, path: &str) -> Resp {
        self.request("GET", path, None, &[])
    }

    pub fn post(&mut self, path: &str, form: &[(&str, &str)]) -> Resp {
        self.post_with(path, form, &[])
    }

    pub fn post_with(
        &mut self,
        path: &str,
        form: &[(&str, &str)],
        headers: &[(&str, &str)],
    ) -> Resp {
        let body = form
            .iter()
            .map(|(k, v)| format!("{}={}", url_encode(k), url_encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        self.request("POST", path, Some(body), headers)
    }

    /// Sign in; returns the final response of the sign-in POST.
    pub fn login(&mut self, email: &str, password: &str) -> Resp {
        let page = self.get("/account/login/");
        let csrf = hidden(&page.body, "_csrf");
        self.post(
            "/account/login/",
            &[
                ("_csrf", &csrf),
                ("email", email),
                ("password", password),
                ("next", "/admin/"),
            ],
        )
    }
}

/// Value of a hidden input `name` in a page.
pub fn hidden(html: &str, name: &str) -> String {
    let needle = format!("name=\"{name}\" value=\"");
    let i = html
        .find(&needle)
        .unwrap_or_else(|| panic!("no hidden `{name}` in:\n{html}"))
        + needle.len();
    html[i..].split('"').next().unwrap().to_string()
}

pub fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(15) {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("timed out waiting for {what}");
}

pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

pub fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

/// Start `engine serve` for `site` on a free port and wait until it
/// answers. `env` adds variables (e.g. ENGINE_SMTP_URL).
pub fn serve(db: &str, site: &Path, log: &Path, env: &[(&str, &str)]) -> (Server, u16) {
    serve_on(free_port(), db, site, log, env)
}

/// [`serve`] on a given port (tests whose redirect URIs are registered
/// elsewhere, e.g. at a sign-in provider).
pub fn serve_on(
    port: u16,
    db: &str,
    site: &Path,
    log: &Path,
    env: &[(&str, &str)],
) -> (Server, u16) {
    let logf = std::fs::File::create(log).unwrap();
    let mut cmd = Command::new(ENGINE);
    cmd.args([
        "serve",
        "--site",
        site.to_str().unwrap(),
        "--port",
        &port.to_string(),
    ])
    .env("ENGINE_DATABASE_URL", db)
    .env("ENGINE_DATABASE_MAX_CONNECTIONS", "4")
    .env("ENGINE_SECRET_KEY", SITE_KEY)
    .env("ENGINE_PUBLIC_URL", format!("http://localhost:{port}"))
    .stdout(Stdio::from(logf.try_clone().unwrap()))
    .stderr(Stdio::from(logf));
    for (k, v) in env {
        cmd.env(k, v);
    }
    let child = cmd.spawn().unwrap();
    let server = Server {
        child,
        log: log.to_path_buf(),
    };
    let start = Instant::now();
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "engine serve did not start:\n{}",
            server.log()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    (server, port)
}
