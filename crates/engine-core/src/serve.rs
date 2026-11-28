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
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};

use crate::precompress::Encoding;
use axum::response::{IntoResponse, Response};
use percent_encoding::percent_decode_str;
use tokio::sync::watch;

/// Whether a POST's `Origin` (when sent) is this host. Browsers send it on
/// every cross-origin POST, so this stops cross-site form posts even before
/// the CSRF token is checked. Also `web::same_origin` (the `request`
/// feature's helpers); here so every build has it (an app crate's handlers).
pub fn same_origin(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    match origin.split_once("://") {
        Some((_, rest)) => !host.is_empty() && rest.eq_ignore_ascii_case(host),
        None => false,
    }
}

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
    /// Exit after this long without a request (and while `busy` says
    /// nothing runs): the hosting admin's idle exit ([`crate::host`]).
    pub idle_exit: Option<std::time::Duration>,
    /// Work that keeps the process from being idle (running jobs).
    pub busy: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    /// Called once when the server begins to stop (a signal or the idle
    /// exit), before it waits for the requests in flight: long-lived
    /// responses (live-region event streams) end here, so the graceful
    /// shutdown does not wait for them for ever.
    pub on_shutdown: Option<Arc<dyn Fn() + Send + Sync>>,
}

struct AppState {
    root: PathBuf,
    etags: EtagCache,
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
        etags: EtagCache::new(),
        live_reload: cfg.live_reload,
        on_miss: cfg.on_miss,
        on_request: cfg.on_request,
    });
    let files = Router::new().fallback(handle).with_state(state);
    let app = match cfg.app {
        Some(app) => app.fallback_service(files),
        None => files,
    };
    // Every request counts, so the allocator is tidied only when idle.
    Ok(app
        .layer(axum::middleware::from_fn(cache_safety))
        .layer(axum::middleware::map_request(|req: Request| async move {
            crate::memory::note_request();
            req
        })))
}

/// `Cache-Control: private, no-store` for every response that is personal
/// or says nothing about caching: anything that sets a cookie, the admin
/// and account pages (except content-hashed files), and any response
/// without a `Cache-Control` of its own (engine pages, redirects, errors).
/// Shared caches and CDNs can then never store a personal page, whatever a
/// handler forgot.
async fn cache_safety(req: Request, next: axum::middleware::Next) -> Response {
    let private_area = {
        let p = req.uri().path();
        ["/admin", "/account"].iter().any(|a| {
            p.strip_prefix(a)
                .is_some_and(|r| r.is_empty() || r.starts_with('/'))
        }) && !crate::assets::is_fingerprinted(p)
    };
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    if private_area || h.contains_key(header::SET_COOKIE) || !h.contains_key(header::CACHE_CONTROL)
    {
        h.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, no-store"),
        );
    }
    resp
}

/// The answer to a request that needs the database while it is unavailable
/// (asleep, restarting, down): `503` with `Retry-After: 30`, never stored.
/// Every handler answers this, never `500`, when its error's chain says the
/// database is unavailable (`engine_db::health::is_unavailable_any`).
pub fn unavailable(body: &'static str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::RETRY_AFTER, "30"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response()
}

/// The body of [`unavailable`] for pages.
pub const UNAVAILABLE_PAGE: &str =
    "This page is not available right now. Please try again in a moment.\n";

/// Serve until `shutdown` resolves (a server with nothing to wait for: no
/// database). The socket is the one systemd passed (socket activation) or
/// bound here.
pub async fn serve(
    cfg: ServeConfig,
    addr: SocketAddr,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    Starting::listen(addr, shutdown).await?.serve(cfg).await
}

/// Where the engine's own start is (listen first: [`Starting::early`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    /// Connecting to the database and wiring the features.
    Starting,
    /// The database is unreachable; still trying.
    WaitingForDatabase,
    /// The database was put to sleep (the hosting admin's watchdog): a
    /// request that needs it wakes it and waits up to [`START_WAIT`].
    Asleep,
    /// The full server has taken over.
    Ready,
}

/// How long a request the published files cannot answer waits for the
/// engine's start before the outage answer (published copy or `503`).
/// Only while the start is under way and the database has not failed.
pub const START_WAIT: Duration = Duration::from_secs(10);

/// The listening socket, taken before anything slower happens, and the
/// server that answers from published files while the engine starts.
pub struct Starting {
    listener: std::net::TcpListener,
    stop: watch::Receiver<bool>,
    phase: watch::Sender<Phase>,
    full: Arc<std::sync::OnceLock<Router>>,
    early: Option<tokio::task::JoinHandle<std::io::Result<()>>>,
    trim: tokio::task::JoinHandle<()>,
    /// A request the published files could not answer arrived.
    demand: Arc<tokio::sync::Notify>,
}

impl Starting {
    /// Take the socket (passed by systemd, or bound to `addr`) and watch
    /// for `shutdown`. Nothing is served yet.
    pub async fn listen(
        addr: SocketAddr,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> anyhow::Result<Starting> {
        let listener = match activated_listener()? {
            Some(l) => {
                println!(
                    "Listening on {} (socket passed by systemd)",
                    l.local_addr()?
                );
                l
            }
            None => {
                let l = std::net::TcpListener::bind(addr)
                    .map_err(|e| anyhow::anyhow!("cannot listen on {addr}: {e}"))?;
                l.set_nonblocking(true)?;
                l
            }
        };
        let (tx, stop) = watch::channel(false);
        tokio::spawn(async move {
            shutdown.await;
            let _ = tx.send(true);
        });
        let (phase, _) = watch::channel(Phase::Starting);
        Ok(Starting {
            listener,
            stop,
            phase,
            full: Arc::new(std::sync::OnceLock::new()),
            early: None,
            demand: Arc::new(tokio::sync::Notify::new()),
            // Free memory goes back to the system when requests stop.
            trim: tokio::spawn(crate::memory::trim_when_idle(crate::memory::IDLE_CHECK)),
        })
    }

    /// [`Self::listen`], then answer at once from the published files in
    /// `root` while the caller connects to the database and wires the
    /// features (listen first). A request the files cannot answer (a page
    /// rendered per request, a form post, the admin) waits up to
    /// [`START_WAIT`] for [`Self::serve`] and is then handed to the full
    /// server; if the database is unreachable meanwhile it gets the outage
    /// answer at once: the published copy of the page if there is one,
    /// otherwise `503` with `Retry-After`. `app` (routes that work without
    /// the database: form posts into the spool) answers instead of that
    /// `503` when given; its fallback should be the outage answer.
    pub async fn early(
        root: &Path,
        addr: SocketAddr,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
        app: Option<Router>,
    ) -> anyhow::Result<Starting> {
        let mut s = Starting::listen(addr, shutdown).await?;
        let root = root.canonicalize()?;
        let files = router(ServeConfig {
            root: root.clone(),
            ..Default::default()
        })?;
        let st = Arc::new(Early {
            root,
            files,
            app,
            full: s.full.clone(),
            phase: s.phase.subscribe(),
            demand: s.demand.clone(),
        });
        let app = Router::new().fallback(early).with_state(st);
        let listener = tokio::net::TcpListener::from_std(s.listener.try_clone()?)?;
        let mut stop = s.stop.clone();
        let mut phase = s.phase.subscribe();
        s.early = Some(tokio::spawn(
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async move {
                tokio::select! {
                    _ = stop.wait_for(|s| *s) => {}
                    _ = phase.wait_for(|p| *p == Phase::Ready) => {}
                }
            })
            .into_future(),
        ));
        Ok(s)
    }

    /// The address the socket listens on (systemd's, under activation).
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Whether a shutdown was asked for (Ctrl+C, SIGTERM).
    pub fn stopping(&self) -> bool {
        *self.stop.borrow()
    }

    /// Resolves when a shutdown is asked for.
    pub async fn stopped(&self) {
        let _ = self.stop.clone().wait_for(|s| *s).await;
    }

    /// Say whether the database is unreachable (requests then get the
    /// outage answer at once instead of waiting for the start).
    pub fn set_waiting_for_database(&self, waiting: bool) {
        let p = if waiting {
            Phase::WaitingForDatabase
        } else {
            Phase::Starting
        };
        self.phase.send_if_modified(|cur| {
            let changed = *cur != p && *cur != Phase::Ready;
            if changed {
                *cur = p;
            }
            changed
        });
    }

    /// Say that the database is asleep: requests that need it wake it
    /// ([`Self::demanded`]) and wait for the start, up to [`START_WAIT`].
    pub fn set_asleep(&self) {
        self.phase.send_if_modified(|cur| {
            let changed = *cur != Phase::Asleep && *cur != Phase::Ready;
            if changed {
                *cur = Phase::Asleep;
            }
            changed
        });
    }

    /// Resolves when a request needs more than the published files (while
    /// starting: the database is wanted).
    pub async fn demanded(&self) {
        self.demand.notified().await
    }

    /// Give up before serving (a shutdown during the start): the early
    /// server finishes its requests.
    pub async fn abandon(mut self) {
        if let Some(e) = self.early.take() {
            let _ = e.await;
        }
        self.trim.abort();
    }

    /// Serve `cfg` on the socket until the shutdown, or until the idle exit
    /// (`cfg.idle_exit`). The early server, if any, stops taking
    /// connections; requests it still holds are handed over.
    pub async fn serve(mut self, cfg: ServeConfig) -> anyhow::Result<()> {
        let idle_exit = cfg.idle_exit;
        let busy = cfg.busy.clone();
        let on_shutdown = cfg.on_shutdown.clone();
        let app = router(cfg)?;
        let _ = self.full.set(app.clone());
        let listener = tokio::net::TcpListener::from_std(self.listener)?;
        self.phase.send_replace(Phase::Ready);
        let mut stop = self.stop.clone();
        // Stop at the shutdown signal, or after the idle period: either way
        // requests in flight finish first (graceful shutdown).
        let shutdown = async move {
            match idle_exit {
                Some(d) => tokio::select! {
                    _ = stop.wait_for(|s| *s) => {}
                    _ = crate::host::idle(d, busy) => {
                        crate::host::note_idle_exit();
                        println!(
                            "Idle for {} s: exiting (socket activation starts the engine again \
                             on the next request)",
                            d.as_secs()
                        );
                    }
                },
                None => {
                    let _ = stop.wait_for(|s| *s).await;
                }
            }
            if let Some(f) = &on_shutdown {
                f();
            }
        };
        // Connection info lets handlers see the peer address (login rate limits).
        let served = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown)
        .await;
        if let Some(e) = self.early.take() {
            let _ = e.await;
        }
        self.trim.abort();
        Ok(served?)
    }
}

struct Early {
    root: PathBuf,
    files: Router,
    app: Option<Router>,
    full: Arc<std::sync::OnceLock<Router>>,
    phase: watch::Receiver<Phase>,
    demand: Arc<tokio::sync::Notify>,
}

/// A request while the engine starts (see [`Starting::early`]).
async fn early(State(st): State<Arc<Early>>, req: Request) -> Response {
    use tower::ServiceExt as _;
    if let Some(full) = st.full.get() {
        return full.clone().oneshot(req).await.into_response();
    }
    let get = req.method() == Method::GET || req.method() == Method::HEAD;
    let file = get && {
        let p = req.uri().path();
        match resolve(&st.root, p) {
            Resolved::File(f) => !crate::precompress::is_sidecar(&f),
            Resolved::Redirect(_) => true,
            _ => false,
        }
    };
    // A published file asked for plainly is what the full server would
    // send too: at once.
    if file && req.uri().query().is_none() {
        return st.files.clone().oneshot(req).await.into_response();
    }
    // The database is wanted (asleep: this wakes it).
    st.demand.notify_one();
    let mut phase = st.phase.clone();
    let now = *phase.borrow();
    // A form post while the database sleeps goes to the spool at once.
    let post = req.method() == Method::POST && st.app.is_some();
    if !(post && now == Phase::Asleep) {
        // (The borrow `wait_for` returns is dropped at once: it is not
        // `Send`.)
        let _ = tokio::time::timeout(START_WAIT, async {
            let _ = phase
                .wait_for(|p| *p != Phase::Starting && *p != Phase::Asleep)
                .await
                .is_ok();
        })
        .await;
    }
    if let Some(full) = st.full.get() {
        return full.clone().oneshot(req).await.into_response();
    }
    // The outage answer: the published copy, or 503.
    if file {
        return st.files.clone().oneshot(req).await.into_response();
    }
    if let Some(app) = &st.app {
        return app.clone().oneshot(req).await.into_response();
    }
    unavailable(UNAVAILABLE_PAGE)
}

/// The listening socket systemd passed, if this process was started by
/// socket activation (`LISTEN_PID` is this process, `LISTEN_FDS` = 1: the
/// socket is file descriptor 3). Taken once; later calls get `None`.
pub fn activated_listener() -> anyhow::Result<Option<std::net::TcpListener>> {
    use std::sync::atomic::{AtomicBool, Ordering};
    static TAKEN: AtomicBool = AtomicBool::new(false);
    let num = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<u32>().ok());
    if num("LISTEN_PID") != Some(std::process::id()) {
        return Ok(None);
    }
    match num("LISTEN_FDS") {
        None | Some(0) => return Ok(None),
        Some(1) => {}
        Some(n) => anyhow::bail!(
            "systemd passed {n} sockets (LISTEN_FDS); the engine listens on one: give its \
             .socket unit one ListenStream"
        ),
    }
    if TAKEN.swap(true, Ordering::AcqRel) {
        return Ok(None);
    }
    take_fd3()
}

#[cfg(unix)]
fn take_fd3() -> anyhow::Result<Option<std::net::TcpListener>> {
    use std::os::fd::FromRawFd as _;
    // SAFETY: under socket activation, file descriptor 3 is the listening
    // socket systemd opened for this process (checked above: LISTEN_PID is
    // this process). Nothing else in the engine opens or uses fd 3 by
    // number, and `TAKEN` makes sure it is wrapped only once, so the
    // listener is its only owner.
    let l = unsafe { std::net::TcpListener::from_raw_fd(3) };
    l.local_addr().map_err(|e| {
        anyhow::anyhow!("the socket passed by systemd (fd 3) is not a TCP socket: {e}")
    })?;
    l.set_nonblocking(true)?;
    Ok(Some(l))
}

#[cfg(not(unix))]
fn take_fd3() -> anyhow::Result<Option<std::net::TcpListener>> {
    Ok(None)
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
            None => not_found(&st, req.headers()).await,
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
        Resolved::File(f) if crate::precompress::is_sidecar(&f) => {
            // The copies are served in place of their file, never by name.
            not_found(&st, req.headers()).await
        }
        Resolved::File(f) => file_response(&st, req.headers(), &f, StatusCode::OK).await,
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
        Resolved::NotFound => not_found(&st, req.headers()).await,
        Resolved::Forbidden => (StatusCode::FORBIDDEN, "Forbidden\n").into_response(),
    }
}

async fn not_found(st: &AppState, headers: &HeaderMap) -> Response {
    let page = st.root.join("404.html");
    if page.is_file() {
        file_response(st, headers, &page, StatusCode::NOT_FOUND).await
    } else {
        (StatusCode::NOT_FOUND, "Not found\n").into_response()
    }
}

/// The encodings the request accepts among the pre-compressed ones, in the
/// server's order of preference (Brotli, then gzip). `q=0` refuses one;
/// `*` accepts any not named.
pub fn accepted_encodings(headers: &HeaderMap) -> Vec<Encoding> {
    let mut named: Vec<(String, bool)> = Vec::new();
    for v in headers.get_all(header::ACCEPT_ENCODING) {
        let Ok(v) = v.to_str() else { continue };
        for part in v.split(',') {
            let mut it = part.split(';');
            let token = it.next().unwrap_or("").trim().to_ascii_lowercase();
            if token.is_empty() {
                continue;
            }
            let q = it
                .filter_map(|p| p.trim().strip_prefix("q="))
                .filter_map(|q| q.trim().parse::<f32>().ok())
                .next()
                .unwrap_or(1.0);
            named.push((token, q > 0.0));
        }
    }
    let star = named.iter().find(|(t, _)| t == "*").map(|(_, ok)| *ok);
    Encoding::ALL
        .into_iter()
        .filter(|e| {
            let names: &[&str] = match e {
                Encoding::Br => &["br"],
                Encoding::Gzip => &["gzip", "x-gzip"],
            };
            match named.iter().find(|(t, _)| names.contains(&t.as_str())) {
                Some((_, ok)) => *ok,
                None => star.unwrap_or(false),
            }
        })
        .collect()
}

/// Whether an `If-None-Match` header matches `etag` (weak comparison, as
/// RFC 9110 asks for GET and HEAD).
pub fn none_match(headers: &HeaderMap, etag: &str) -> bool {
    let bare = |t: &str| t.trim().trim_start_matches("W/").to_string();
    headers.get_all(header::IF_NONE_MATCH).iter().any(|v| {
        v.to_str()
            .map(|v| {
                v.split(',')
                    .any(|t| t.trim() == "*" || bare(t) == bare(etag))
            })
            .unwrap_or(false)
    })
}

async fn file_response(
    st: &AppState,
    req: &HeaderMap,
    path: &Path,
    status: StatusCode,
) -> Response {
    let Ok(meta) = tokio::fs::metadata(path).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let ctype = content_type(path);
    let live = st.live_reload.is_some();
    let varies = !live && crate::precompress::compressible(path);
    // A pre-compressed copy, if the client takes it and it is fresh (a
    // regular file no newer than the file: see `precompress`).
    let copy = if varies {
        accepted_encodings(req).into_iter().find_map(|e| {
            let side = e.sidecar(path);
            crate::precompress::fresh(&meta, &side).map(|_| (e, side))
        })
    } else {
        None
    };
    let original = match &copy {
        // The original's bytes are needed for the body or for its hash.
        None => match tokio::fs::read(path).await {
            Ok(b) => Some(b),
            Err(_) => return StatusCode::NOT_FOUND.into_response(),
        },
        Some(_) => None,
    };
    let etag = if live {
        None
    } else {
        let hash = match &original {
            Some(b) => st.etags.get(path, &meta, || Ok(b.clone())),
            None => st.etags.get(path, &meta, || std::fs::read(path)),
        };
        hash.map(|h| match &copy {
            Some((e, _)) => format!("\"{h}-{}\"", e.suffix().trim_start_matches('.')),
            None => format!("\"{h}\""),
        })
    };
    // Content-hashed names are kept for a year; everything else (pages,
    // feeds, files linked by a hand-written path) is revalidated on every
    // use, so a refresh always shows an edit (crate::assets).
    let cache = if live {
        "no-store"
    } else {
        let rel = path.strip_prefix(&st.root).unwrap_or(path);
        crate::assets::cache_control(&rel.to_string_lossy())
    };
    let mut h = HeaderMap::new();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(ctype));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    if varies {
        h.insert(header::VARY, HeaderValue::from_static("accept-encoding"));
    }
    if let Some(t) = etag.as_deref().and_then(|t| HeaderValue::from_str(t).ok()) {
        h.insert(header::ETAG, t);
    }
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    if status == StatusCode::OK
        && let Some(t) = &etag
        && none_match(req, t)
    {
        let mut resp = StatusCode::NOT_MODIFIED.into_response();
        h.remove(header::CONTENT_TYPE);
        resp.headers_mut().extend(h);
        return resp;
    }
    let body = match (&copy, original) {
        (Some((e, side)), _) => match tokio::fs::read(side).await {
            Ok(b) => {
                h.insert(
                    header::CONTENT_ENCODING,
                    HeaderValue::from_static(e.token()),
                );
                b
            }
            // Gone between the check and the read: the file itself.
            Err(_) => match tokio::fs::read(path).await {
                Ok(b) => b,
                Err(_) => return StatusCode::NOT_FOUND.into_response(),
            },
        },
        (None, Some(mut body)) => {
            if live && ctype.starts_with("text/html") {
                let html = String::from_utf8_lossy(&body);
                body = match html.rfind("</body>") {
                    Some(i) => format!("{}{RELOAD_SCRIPT}{}", &html[..i], &html[i..]),
                    None => format!("{html}{RELOAD_SCRIPT}"),
                }
                .into_bytes();
            }
            body
        }
        (None, None) => return StatusCode::NOT_FOUND.into_response(),
    };
    let mut resp = Response::new(Body::from(body));
    *resp.status_mut() = status;
    resp.headers_mut().extend(h);
    resp
}

/// Content-derived ETags of served files: the first 128 bits of the
/// SHA-256 of the file's bytes, kept per path with the file's identity
/// (length, modification time, inode), so each version is hashed once,
/// not per request. Bounded by the machine's memory.
struct EtagCache {
    map: std::sync::RwLock<std::collections::HashMap<PathBuf, (FileId, Arc<str>)>>,
    cap: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct FileId {
    len: u64,
    modified: Option<std::time::SystemTime>,
    ino: u64,
}

impl FileId {
    fn of(m: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        let ino = std::os::unix::fs::MetadataExt::ino(m);
        #[cfg(not(unix))]
        let ino = 0;
        FileId {
            len: m.len(),
            modified: m.modified().ok(),
            ino,
        }
    }
}

impl EtagCache {
    fn new() -> Self {
        // About 200 bytes an entry (crate::limits::etag_cache_entries).
        let cap = crate::limits::current().etag_cache_entries;
        EtagCache {
            map: Default::default(),
            cap,
        }
    }

    fn get(
        &self,
        path: &Path,
        meta: &std::fs::Metadata,
        bytes: impl FnOnce() -> std::io::Result<Vec<u8>>,
    ) -> Option<Arc<str>> {
        let id = FileId::of(meta);
        if let Some((have, tag)) = self.map.read().unwrap_or_else(|p| p.into_inner()).get(path)
            && *have == id
        {
            return Some(tag.clone());
        }
        let body = bytes().ok()?;
        // Hashed bytes must be the version `meta` describes (the file may
        // have been replaced in between): otherwise no ETag this time.
        if body.len() as u64 != id.len
            || std::fs::metadata(path).map(|m| FileId::of(&m)).ok() != Some(id)
        {
            return None;
        }
        let tag: Arc<str> = crate::builder::sha256_hex(&body)[..32].into();
        let mut map = self.map.write().unwrap_or_else(|p| p.into_inner());
        if map.len() >= self.cap {
            map.clear();
        }
        map.insert(path.to_path_buf(), (id, tag.clone()));
        Some(tag)
    }
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
    async fn caching_by_name_and_never_shared_when_personal() {
        let (_d, root) = fixture();
        let hashed = crate::assets::hashed_name("/site.css", b"a{}");
        std::fs::write(root.join(hashed.trim_start_matches('/')), "a{}").unwrap();
        std::fs::write(root.join("site.css"), "a{}").unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new()
            .route(
                "/admin/x/",
                axum::routing::get(|| async {
                    ([(header::CACHE_CONTROL, "public, max-age=60")], "admin")
                }),
            )
            .route(
                "/cookie/",
                axum::routing::get(|| async {
                    (
                        [
                            (header::SET_COOKIE, "a=b"),
                            (header::CACHE_CONTROL, "no-cache"),
                        ],
                        "c",
                    )
                }),
            )
            .route("/plain/", axum::routing::get(|| async { "plain" }));
        let app = router(ServeConfig {
            root,
            app: Some(app),
            ..Default::default()
        })
        .unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let h = hashed.clone();
        let r = tokio::task::spawn_blocking(move || {
            [
                h.as_str(),
                "/site.css",
                "/",
                "/admin/x/",
                "/cookie/",
                "/plain/",
                "/missing.css",
            ]
            .map(|p| raw_get(addr, p).to_ascii_lowercase())
        })
        .await
        .unwrap();
        let cc = |resp: &str| {
            resp.lines()
                .find_map(|l| l.strip_prefix("cache-control: "))
                .unwrap_or("")
                .trim()
                .to_string()
        };
        assert_eq!(cc(&r[0]), "public, max-age=31536000, immutable", "{}", r[0]);
        assert_eq!(cc(&r[1]), "no-cache", "a hand-linked file revalidates");
        assert_eq!(cc(&r[2]), "no-cache", "pages revalidate");
        assert!(r[2].contains("etag: \""), "{}", r[2]);
        assert_eq!(cc(&r[3]), "private, no-store", "admin is never shared");
        assert_eq!(cc(&r[4]), "private, no-store", "a cookie makes it personal");
        assert_eq!(
            cc(&r[5]),
            "private, no-store",
            "no policy given: not stored"
        );
        assert_eq!(cc(&r[6]), "no-cache", "the 404 page");
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
