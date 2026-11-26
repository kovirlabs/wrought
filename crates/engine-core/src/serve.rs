//! The built-in static file server (`engine serve`, `engine dev`).
//!
//! It serves a built output folder. For production the front door is Caddy
//! or nginx serving the same folder; this server covers development and
//! small installs.
//!
//! Safety: request paths are percent-decoded and split into segments; `.`,
//! `..`, dotfiles, backslashes and NUL bytes are refused, and the final file
//! is canonicalised and must still lie inside the root (so symlinks cannot
//! escape it). Only GET and HEAD are answered.

use std::future::Future;
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use percent_encoding::percent_decode_str;
use tokio::sync::watch;

/// Where a request path leads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    File(PathBuf),
    /// A folder requested without its trailing slash.
    Redirect(String),
    NotFound,
    /// The path tried to leave the root or reach a hidden file.
    Forbidden,
}

/// Map a URL path onto a file under `root`. `root` must be canonical.
pub fn resolve(root: &Path, url_path: &str) -> Resolved {
    let Ok(decoded) = percent_decode_str(url_path).decode_utf8() else {
        return Resolved::Forbidden;
    };
    if !decoded.starts_with('/') {
        return Resolved::Forbidden;
    }
    let mut path = root.to_path_buf();
    for seg in decoded.split('/') {
        if seg.is_empty() {
            continue;
        }
        if seg == "."
            || seg == ".."
            || seg.contains('\\')
            || seg.contains('\0')
            || (seg.starts_with('.') && seg != ".well-known")
        {
            return Resolved::Forbidden;
        }
        // Belt and braces: the segment must be one normal path component.
        let mut comps = Path::new(seg).components();
        match (comps.next(), comps.next()) {
            (Some(Component::Normal(_)), None) => path.push(seg),
            _ => return Resolved::Forbidden,
        }
    }

    let target = if path.is_dir() {
        if !decoded.ends_with('/') {
            return Resolved::Redirect(format!("{url_path}/"));
        }
        path.join("index.html")
    } else {
        path
    };
    match target.canonicalize() {
        Ok(real) if real.starts_with(root) && real.is_file() => Resolved::File(real),
        Ok(_) => Resolved::Forbidden,
        Err(_) => Resolved::NotFound,
    }
}

pub fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("json" | "map") => "application/json",
        Some("xml") => "application/xml; charset=utf-8",
        Some("txt" | "md") => "text/plain; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("pdf") => "application/pdf",
        Some("wasm") => "application/wasm",
        Some("webmanifest") => "application/manifest+json",
        _ => "application/octet-stream",
    }
}

/// Called with the URL path of a request that found no file. Returns
/// whether it wrote the file (the server then serves it from disk).
pub type MissHandler =
    Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync>;

/// A GET or HEAD request as the per-request render hook sees it.
#[derive(Debug, Clone)]
pub struct PageRequest {
    pub method: Method,
    pub path: String,
    pub query: Option<String>,
    pub headers: axum::http::HeaderMap,
}

/// Called for every GET/HEAD before the file lookup. Returns a response to
/// answer the request itself (`request` mode), or `None` to serve the file.
pub type RequestHandler = Arc<
    dyn Fn(PageRequest) -> Pin<Box<dyn Future<Output = Option<Response>> + Send>> + Send + Sync,
>;

/// Server settings.
#[derive(Clone, Default)]
pub struct ServeConfig {
    pub root: PathBuf,
    /// Dev mode: inject the live-reload script and disable caching.
    pub live_reload: Option<watch::Receiver<u64>>,
    /// Render-on-miss (publish routes whose file is missing).
    pub on_miss: Option<MissHandler>,
    /// Routes the engine's features answer (`/account/...`, `/admin/...`),
    /// tried before anything else. Only compiled-in features add any.
    pub app: Option<Router>,
    /// Per-request rendering (`request` mode), tried before files.
    pub on_request: Option<RequestHandler>,
}

struct AppState {
    root: PathBuf,
    live_reload: Option<watch::Receiver<u64>>,
    on_miss: Option<MissHandler>,
    on_request: Option<RequestHandler>,
}

/// The live-reload client: long-polls `/_engine/reload` and reloads the page
/// when the build version changes.
const RELOAD_SCRIPT: &str = "<script type=\"module\">\
let v=null;for(;;){try{const r=await fetch('/_engine/reload'+(v===null?'':'?since='+v));\
const t=await r.text();if(v!==null&&t!==v){location.reload();break}v=t}\
catch(e){await new Promise(r=>setTimeout(r,1000))}}</script>";

pub fn router(cfg: ServeConfig) -> std::io::Result<Router> {
    let root = cfg.root.canonicalize()?;
    let state = Arc::new(AppState {
        root,
        live_reload: cfg.live_reload,
        on_miss: cfg.on_miss,
        on_request: cfg.on_request,
    });
    let files = Router::new().fallback(handle).with_state(state);
    Ok(match cfg.app {
        Some(app) => app.fallback_service(files),
        None => files,
    })
}

/// Serve until `shutdown` resolves.
pub async fn serve(
    cfg: ServeConfig,
    addr: SocketAddr,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let app = router(cfg)?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| anyhow::anyhow!("cannot listen on {addr}: {e}"))?;
    // Connection info lets handlers see the peer address (login rate limits).
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await?;
    Ok(())
}

async fn handle(State(st): State<Arc<AppState>>, req: Request) -> Response {
    if req.method() != Method::GET && req.method() != Method::HEAD {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            [(header::ALLOW, "GET, HEAD")],
        )
            .into_response();
    }
    let path = req.uri().path().to_string();

    if let Some(on_request) = &st.on_request
        && let Some(resp) = on_request(PageRequest {
            method: req.method().clone(),
            path: path.clone(),
            query: req.uri().query().map(str::to_string),
            headers: req.headers().clone(),
        })
        .await
    {
        return resp;
    }

    if path == "/_engine/reload" {
        return match &st.live_reload {
            Some(rx) => reload(rx.clone(), req.uri().query()).await,
            None => not_found(&st).await,
        };
    }

    let mut resolved = resolve(&st.root, &path);
    if resolved == Resolved::NotFound
        && let Some(on_miss) = &st.on_miss
        && on_miss(path.clone()).await
    {
        resolved = resolve(&st.root, &path);
    }
    match resolved {
        Resolved::File(f) => file_response(&st, &f, StatusCode::OK).await,
        Resolved::Redirect(to) => {
            let to = match req.uri().query() {
                Some(q) => format!("{to}?{q}"),
                None => to,
            };
            match HeaderValue::from_str(&to) {
                Ok(v) => (StatusCode::MOVED_PERMANENTLY, [(header::LOCATION, v)]).into_response(),
                Err(_) => StatusCode::BAD_REQUEST.into_response(),
            }
        }
        Resolved::NotFound => not_found(&st).await,
        Resolved::Forbidden => (StatusCode::FORBIDDEN, "Forbidden\n").into_response(),
    }
}

async fn not_found(st: &AppState) -> Response {
    let page = st.root.join("404.html");
    if page.is_file() {
        file_response(st, &page, StatusCode::NOT_FOUND).await
    } else {
        (StatusCode::NOT_FOUND, "Not found\n").into_response()
    }
}

async fn file_response(st: &AppState, path: &Path, status: StatusCode) -> Response {
    let Ok(mut body) = tokio::fs::read(path).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let ctype = content_type(path);
    if st.live_reload.is_some() && ctype.starts_with("text/html") {
        let html = String::from_utf8_lossy(&body);
        body = match html.rfind("</body>") {
            Some(i) => format!("{}{RELOAD_SCRIPT}{}", &html[..i], &html[i..]),
            None => format!("{html}{RELOAD_SCRIPT}"),
        }
        .into_bytes();
    }
    let cache = if st.live_reload.is_some() {
        "no-store"
    } else if ctype.starts_with("text/html") {
        "no-cache"
    } else {
        "public, max-age=3600"
    };
    let mut resp = Response::new(Body::from(body));
    *resp.status_mut() = status;
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(ctype));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    resp
}

/// Long-poll: answer with the build version once it differs from `since`
/// (or after 25 s, so proxies don't time the request out).
async fn reload(mut rx: watch::Receiver<u64>, query: Option<&str>) -> Response {
    let since: Option<u64> = query
        .and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("since=")))
        .and_then(|v| v.parse().ok());
    if let Some(since) = since {
        let wait = rx.wait_for(|v| *v != since);
        let _ = tokio::time::timeout(Duration::from_secs(25), wait).await;
    }
    let v = *rx.borrow();
    (
        [
            (header::CONTENT_TYPE, "text/plain"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        v.to_string(),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("public");
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::create_dir_all(root.join(".well-known")).unwrap();
        std::fs::write(root.join("index.html"), "<html><body>home</body></html>").unwrap();
        std::fs::write(root.join("docs/index.html"), "docs").unwrap();
        std::fs::write(root.join("404.html"), "<p>missing</p>").unwrap();
        std::fs::write(root.join(".engine-manifest.json"), "{}").unwrap();
        std::fs::write(root.join(".well-known/security.txt"), "x").unwrap();
        std::fs::write(d.path().join("secret.txt"), "TOP SECRET").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(d.path().join("secret.txt"), root.join("link.txt")).unwrap();
        let root = root.canonicalize().unwrap();
        (d, root)
    }

    #[test]
    fn resolves_files_and_folders() {
        let (_d, root) = fixture();
        assert_eq!(resolve(&root, "/"), Resolved::File(root.join("index.html")));
        assert_eq!(
            resolve(&root, "/docs/"),
            Resolved::File(root.join("docs/index.html"))
        );
        assert_eq!(resolve(&root, "/docs"), Resolved::Redirect("/docs/".into()));
        assert_eq!(resolve(&root, "/nope/"), Resolved::NotFound);
        assert!(matches!(
            resolve(&root, "/.well-known/security.txt"),
            Resolved::File(_)
        ));
    }

    #[test]
    fn path_traversal_is_refused() {
        let (_d, root) = fixture();
        for bad in [
            "/../secret.txt",
            "/docs/../../secret.txt",
            "/%2e%2e/secret.txt",
            "/%2E%2E%2Fsecret.txt",
            "/docs/%2e%2e/%2e%2e/secret.txt",
            "/..%5csecret.txt",
            "/%00",
            "/.engine-manifest.json",
            "/%ff",
            "relative",
        ] {
            assert_eq!(resolve(&root, bad), Resolved::Forbidden, "{bad}");
        }
        #[cfg(unix)]
        assert_eq!(resolve(&root, "/link.txt"), Resolved::Forbidden);
    }

    fn raw_get(addr: SocketAddr, target: &str) -> String {
        let mut s = std::net::TcpStream::connect(addr).unwrap();
        write!(
            s,
            "GET {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn http_server_refuses_traversal_and_serves_404() {
        let (_d, root) = fixture();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(ServeConfig {
            root,
            ..Default::default()
        })
        .unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let res = tokio::task::spawn_blocking(move || {
            (
                raw_get(addr, "/"),
                raw_get(addr, "/../secret.txt"),
                raw_get(addr, "/%2e%2e/%2e%2e/secret.txt"),
                raw_get(addr, "/missing/"),
                raw_get(addr, "/docs"),
            )
        })
        .await
        .unwrap();
        assert!(res.0.starts_with("HTTP/1.1 200"), "{}", res.0);
        assert!(res.0.contains("nosniff"));
        for r in [&res.1, &res.2] {
            assert!(r.starts_with("HTTP/1.1 403"), "{r}");
            assert!(!r.contains("TOP SECRET"));
        }
        assert!(res.3.starts_with("HTTP/1.1 404"), "{}", res.3);
        assert!(res.3.contains("<p>missing</p>"));
        assert!(res.4.starts_with("HTTP/1.1 301"), "{}", res.4);
        assert!(res.4.to_ascii_lowercase().contains("location: /docs/"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn miss_handler_can_write_the_file() {
        let (_d, root) = fixture();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let write_root = root.clone();
        let on_miss: MissHandler = Arc::new(move |path: String| {
            let root = write_root.clone();
            Box::pin(async move {
                if path != "/late/" {
                    return false;
                }
                std::fs::create_dir_all(root.join("late")).unwrap();
                std::fs::write(root.join("late/index.html"), "rendered late").unwrap();
                true
            })
        });
        let app = router(ServeConfig {
            root,
            on_miss: Some(on_miss),
            ..Default::default()
        })
        .unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let (late, other) = tokio::task::spawn_blocking(move || {
            (raw_get(addr, "/late/"), raw_get(addr, "/other/"))
        })
        .await
        .unwrap();
        assert!(late.starts_with("HTTP/1.1 200"), "{late}");
        assert!(late.ends_with("rendered late"), "{late}");
        assert!(other.starts_with("HTTP/1.1 404"), "{other}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dev_mode_injects_reload_script() {
        let (_d, root) = fixture();
        let (_tx, rx) = watch::channel(1u64);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(ServeConfig {
            root,
            live_reload: Some(rx),
            ..Default::default()
        })
        .unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let (home, version) = tokio::task::spawn_blocking(move || {
            (raw_get(addr, "/"), raw_get(addr, "/_engine/reload"))
        })
        .await
        .unwrap();
        assert!(home.contains("/_engine/reload"), "{home}");
        assert!(home.contains("no-store"));
        assert!(
            version.ends_with("\r\n\r\n1") || version.ends_with("\n1"),
            "{version}"
        );
    }
}
