//! `engine dev`'s developer tools: one layer around every route (only under
//! `engine dev`, see [`engine_core::dev`]) and the pages under
//! `/_engine/dev/`.
//!
//! * **Query log.** Each request runs inside a [`RequestLog`]; every query
//!   it makes (on the request's task, or in a render it carries the log to)
//!   is recorded with its statement text (its shape: values are bound
//!   parameters, never recorded), duration, rows and call site. The
//!   terminal gets one line per request and one per query; a statement
//!   shape run at least `--n-plus-one` times (default 5) in one request is
//!   reported as a likely N+1 with the call site of its first run.
//! * **Error pages.** A `500` that carries its error ([`ErrorDetail`],
//!   attached by the handler error type and request renders while `engine
//!   dev` runs) becomes a page with the error chain, the template file and
//!   line with the source around it, the request and its queries. A `404`
//!   lists the routes nearest to the path. A refused live-region stream
//!   (`403` from `/_engine/live`) says which tags were refused and why.
//! * **Overlay.** HTML responses get a small link to their request's page
//!   (queries, time, N+1 warnings) and the live-reload script.
//! * **Mail** (`mail` feature): outgoing messages are captured instead of
//!   sent ([`engine_mail::Capture`]) and listed at `/_engine/dev/mail`;
//!   every mail template can be previewed with its sample data.
//!
//! `engine serve` never installs any of it: the layer is `ServeConfig::outer`,
//! which only `engine dev` sets, and the routes are merged only there.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::extract::{Path as UrlPath, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use engine_core::dev::{ErrorDetail, QueryRecord, RequestLog};

/// The default of `engine dev --n-plus-one`: a statement shape run this
/// many times in one request is reported.
pub const DEFAULT_N_PLUS_ONE: usize = 5;

/// Requests kept for `/_engine/dev/`.
const KEEP: usize = 100;

/// HTML bodies larger than this are passed through untouched.
const MAX_BODY: usize = 16 << 20;

/// What the tools know. Cheap to share (`Arc`).
pub struct DevTools {
    site: PathBuf,
    n_plus_one: usize,
    requests: Mutex<VecDeque<Arc<Recorded>>>,
    next: AtomicU64,
    #[cfg(feature = "mail")]
    mailer: Mutex<Option<engine_mail::Mailer>>,
    #[cfg(feature = "realtime")]
    live_public: Mutex<Option<engine_realtime::Public>>,
}

/// One request as the tools saw it.
struct Recorded {
    id: u64,
    method: Method,
    uri: String,
    headers: Vec<(String, String)>,
    status: StatusCode,
    elapsed: Duration,
    queries: Vec<QueryRecord>,
    error: Option<Arc<anyhow::Error>>,
}

impl DevTools {
    pub fn new(site: &Path, n_plus_one: usize) -> Arc<DevTools> {
        Arc::new(DevTools {
            site: site.to_path_buf(),
            n_plus_one: n_plus_one.max(2),
            requests: Mutex::new(VecDeque::new()),
            next: AtomicU64::new(0),
            #[cfg(feature = "mail")]
            mailer: Mutex::new(None),
            #[cfg(feature = "realtime")]
            live_public: Mutex::new(None),
        })
    }

    /// Capture this mailer's messages (instead of sending them) and preview
    /// its templates.
    #[cfg(feature = "mail")]
    pub fn capture_mail(&self, m: &engine_mail::Mailer) {
        m.capture_into(engine_mail::Capture::new());
        *self.mailer.lock().unwrap_or_else(|p| p.into_inner()) = Some(m.clone());
    }

    /// The rule `/_engine/live` applies, to explain a refusal.
    #[cfg(feature = "realtime")]
    pub fn set_live_public(&self, p: engine_realtime::Public) {
        *self.live_public.lock().unwrap_or_else(|p| p.into_inner()) = Some(p);
    }

    /// The layer for `ServeConfig::outer`.
    pub fn layer(self: &Arc<Self>) -> Arc<dyn Fn(Router) -> Router + Send + Sync> {
        let me = self.clone();
        Arc::new(move |r: Router| r.layer(axum::middleware::from_fn_with_state(me.clone(), layer)))
    }

    /// The pages under `/_engine/dev/`.
    pub fn router(self: &Arc<Self>) -> Router {
        use axum::routing::get;
        let r = Router::new()
            .route("/_engine/dev", get(|| async { redirect("/_engine/dev/") }))
            .route("/_engine/dev/", get(index))
            .route("/_engine/dev/requests/{id}", get(request_page));
        #[cfg(feature = "mail")]
        let r = r
            .route(
                "/_engine/dev/mail",
                get(|| async { redirect("/_engine/dev/mail/") }),
            )
            .route("/_engine/dev/mail/", get(mail_list))
            .route("/_engine/dev/mail/{id}", get(mail_page))
            .route("/_engine/dev/mail/{id}/html", get(mail_html))
            .route("/_engine/dev/mail/templates/{name}", get(template_page))
            .route(
                "/_engine/dev/mail/templates/{name}/html",
                get(template_html),
            );
        r.with_state(self.clone())
    }

    fn keep(&self, r: Recorded) -> Arc<Recorded> {
        let r = Arc::new(r);
        let mut q = self.requests.lock().unwrap_or_else(|p| p.into_inner());
        if q.len() >= KEEP {
            q.pop_front();
        }
        q.push_back(r.clone());
        r
    }

    fn get(&self, id: u64) -> Option<Arc<Recorded>> {
        let q = self.requests.lock().unwrap_or_else(|p| p.into_inner());
        q.iter().find(|r| r.id == id).cloned()
    }
}

// ----- the layer ---------------------------------------------------------------

async fn layer(
    State(t): State<Arc<DevTools>>,
    req: Request,
    next: axum::middleware::Next,
) -> Response {
    let path = req.uri().path().to_string();
    if path.starts_with("/_engine/dev") || path == "/_engine/reload" {
        return next.run(req).await;
    }
    let method = req.method().clone();
    let uri = req.uri().to_string();
    let query = req.uri().query().map(str::to_string);
    let headers = shown_headers(req.headers());
    let wants_html = accepts_html(req.headers());
    let log = RequestLog::new();
    let started = Instant::now();
    let resp = engine_core::dev::scope(log.clone(), next.run(req)).await;
    let elapsed = started.elapsed();
    let status = resp.status();
    let html = is_html(resp.headers());
    let queries = log.queries();
    let error = resp.extensions().get::<ErrorDetail>().map(|d| d.0.clone());
    // Files (styles, scripts, images) that ran no query are not worth a line.
    if queries.is_empty() && !html && status.as_u16() < 400 {
        return resp;
    }
    let id = t.next.fetch_add(1, Ordering::Relaxed) + 1;
    let rec = t.keep(Recorded {
        id,
        method,
        uri,
        headers,
        status,
        elapsed,
        queries,
        error,
    });
    print_request(&t, &rec);

    if status == StatusCode::INTERNAL_SERVER_ERROR {
        return if wants_html {
            dev_page(status, &error_title(&rec), &error_body(&t, &rec))
        } else {
            plain(status, &error_text(&rec))
        };
    }
    if status == StatusCode::NOT_FOUND
        && wants_html
        && (rec.method == Method::GET || rec.method == Method::HEAD)
        && !path.starts_with("/_engine/")
    {
        return dev_page(
            status,
            &format!("404: nothing at {path}"),
            &not_found_body(&t, &path, id),
        );
    }
    if status == StatusCode::FORBIDDEN && path == "/_engine/live" {
        return plain(status, &live_refusal(&t, query.as_deref()));
    }
    if html && status.is_success() {
        return with_overlay(&t, resp, &rec).await;
    }
    resp
}

/// Whether an error is shown as a page: unless the client asked for
/// something else (JSON, an event stream) and not for HTML.
fn accepts_html(h: &HeaderMap) -> bool {
    let a = h
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("*/*");
    a.contains("text/html") || a.contains("*/*")
}

fn is_html(h: &HeaderMap) -> bool {
    h.get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|c| c.starts_with("text/html"))
}

/// The request's headers for the dev pages: secrets are left out.
fn shown_headers(h: &HeaderMap) -> Vec<(String, String)> {
    h.iter()
        .map(|(k, v)| {
            let k = k.as_str().to_string();
            let v = match k.as_str() {
                "authorization" | "proxy-authorization" => "(hidden)".to_string(),
                // Names only: the values are session tokens.
                "cookie" => v
                    .to_str()
                    .unwrap_or("")
                    .split(';')
                    .filter_map(|c| c.split('=').next())
                    .map(|n| format!("{}=(hidden)", n.trim()))
                    .collect::<Vec<_>>()
                    .join("; "),
                _ => v.to_str().unwrap_or("(not text)").to_string(),
            };
            (k, v)
        })
        .collect()
}

fn ms(d: Duration) -> String {
    format!("{:.1} ms", d.as_secs_f64() * 1000.0)
}

fn one_line(s: &str, max: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= max {
        s
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}...")
    }
}

fn rows(q: &QueryRecord) -> String {
    match (q.rows, &q.error) {
        (_, Some(_)) => "failed".into(),
        (Some(1), _) => "1 row".into(),
        (Some(n), _) => format!("{n} rows"),
        (None, _) => "-".into(),
    }
}

/// The terminal's view of a request: one line, one per query (at most 30),
/// and the N+1 warnings.
fn print_request(t: &DevTools, r: &Recorded) {
    let total: Duration = r.queries.iter().map(|q| q.duration).sum();
    let path = r.uri.split('?').next().unwrap_or(&r.uri);
    println!(
        "{} {} {} in {}, {} quer{} ({})",
        r.method,
        path,
        r.status.as_u16(),
        ms(r.elapsed),
        r.queries.len(),
        if r.queries.len() == 1 { "y" } else { "ies" },
        ms(total)
    );
    const SHOWN: usize = 30;
    for q in r.queries.iter().take(SHOWN) {
        println!(
            "  {:>8}  {:>8}  {}",
            ms(q.duration),
            rows(q),
            one_line(&q.sql, 140)
        );
    }
    if r.queries.len() > SHOWN {
        println!(
            "  ... and {} more: http://<dev server>/_engine/dev/requests/{}",
            r.queries.len() - SHOWN,
            r.id
        );
    }
    for (shape, n, site) in engine_core::dev::repeated(&r.queries, t.n_plus_one) {
        eprintln!(
            "  warning: likely N+1: the same statement ran {n} times in this request: {}{}",
            one_line(&shape, 120),
            site.map(|s| format!("\n           first from {s}"))
                .unwrap_or_default()
        );
    }
}

/// HTML with the overlay (and the live-reload script, for pages that are
/// not files: request pages and handlers).
async fn with_overlay(t: &DevTools, resp: Response, r: &Recorded) -> Response {
    if resp.headers().contains_key(header::CONTENT_ENCODING) {
        return resp;
    }
    let (mut parts, body) = resp.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_BODY).await else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "dev: response too large\n",
        )
            .into_response();
    };
    let mut html = String::from_utf8_lossy(&bytes).into_owned();
    let mut add = overlay(t, r);
    if !html.contains("/_engine/reload") {
        add.push_str(engine_core::serve::RELOAD_SCRIPT);
    }
    match html.rfind("</body>") {
        Some(i) => html.insert_str(i, &add),
        None => html.push_str(&add),
    }
    parts.headers.remove(header::CONTENT_LENGTH);
    parts.headers.remove(header::ETAG);
    parts
        .headers
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Response::from_parts(parts, Body::from(html))
}

fn overlay(t: &DevTools, r: &Recorded) -> String {
    let total: Duration = r.queries.iter().map(|q| q.duration).sum();
    let n1 = engine_core::dev::repeated(&r.queries, t.n_plus_one).len();
    let bg = if n1 > 0 { "#8a1111" } else { "#1b1b1b" };
    format!(
        "<aside aria-label=\"Hammerwright dev tools\" style=\"position:fixed;right:8px;bottom:8px;\
         z-index:2147483647;margin:0;padding:4px 10px;border-radius:6px;background:{bg};\
         font:13px/1.5 system-ui,sans-serif\"><a href=\"/_engine/dev/requests/{id}\" \
         style=\"color:#fff;text-decoration:underline\">dev: {n} quer{y}, {qt} of {rt}{warn}</a></aside>",
        id = r.id,
        n = r.queries.len(),
        y = if r.queries.len() == 1 { "y" } else { "ies" },
        qt = ms(total),
        rt = ms(r.elapsed),
        warn = if n1 > 0 { ", likely N+1" } else { "" },
    )
}

// ----- pages -------------------------------------------------------------------

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            c => o.push(c),
        }
    }
    o
}

const STYLE: &str = "body{margin:0;font:15px/1.5 system-ui,sans-serif;color:#1b1b1b;background:#fff}\
header{background:#1b1b1b;color:#fff;padding:8px 16px}header a{color:#fff;margin-right:16px}\
main{padding:16px;max-width:1200px}h1{font-size:1.4em;margin:.4em 0}h2{font-size:1.15em;margin-top:1.4em}\
pre{background:#f3f3f3;padding:8px;white-space:pre-wrap;overflow-wrap:anywhere}code{font-size:.95em}\
table{border-collapse:collapse;width:100%}th,td{border:1px solid #ccc;padding:4px 6px;text-align:left;vertical-align:top}\
.warn{background:#fff3cd;border:1px solid #8a6d00;padding:8px}.err{background:#fde7e7;border:1px solid #8a1111;padding:8px}\
.hl{background:#fde7e7;font-weight:bold}a{color:#0b4fa3}li a,td a,nav a{display:inline-block;min-height:24px;line-height:24px}iframe{width:100%;height:480px;border:1px solid #ccc}";

fn shell(title: &str, body: &str) -> String {
    #[cfg(feature = "mail")]
    let mail = "<a href=\"/_engine/dev/mail/\">Mail</a>";
    #[cfg(not(feature = "mail"))]
    let mail = "";
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{t} - engine dev</title><style>{STYLE}</style></head><body>\
         <header><nav aria-label=\"Dev tools\"><a href=\"/_engine/dev/\">Requests</a>{mail}\
         <a href=\"/\">Site</a></nav></header><main><h1>{t}</h1>{body}</main></body></html>",
        t = esc(title),
    )
}

fn dev_page(status: StatusCode, title: &str, body: &str) -> Response {
    (
        status,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        shell(title, body),
    )
        .into_response()
}

fn plain(status: StatusCode, body: &str) -> Response {
    (
        status,
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body.to_string(),
    )
        .into_response()
}

fn redirect(to: &'static str) -> Response {
    (StatusCode::SEE_OTHER, [(header::LOCATION, to)]).into_response()
}

fn not_found_page() -> Response {
    dev_page(
        StatusCode::NOT_FOUND,
        "Not kept",
        "<p>This dev server keeps the last 100 requests; this one is gone.</p>",
    )
}

async fn index(State(t): State<Arc<DevTools>>) -> Response {
    let list: Vec<Arc<Recorded>> = {
        let q = t.requests.lock().unwrap_or_else(|p| p.into_inner());
        q.iter().rev().cloned().collect()
    };
    let mut b = String::from(
        "<p>The last requests that ran queries, served a page or failed (newest first). \
         Only <code>engine dev</code> has these pages.</p>",
    );
    if list.is_empty() {
        b.push_str("<p>None yet.</p>");
    } else {
        b.push_str(
            "<table><thead><tr><th scope=\"col\">Request</th><th scope=\"col\">Status</th>\
             <th scope=\"col\">Time</th><th scope=\"col\">Queries</th><th scope=\"col\">N+1</th>\
             </tr></thead><tbody>",
        );
        for r in list {
            let n1 = engine_core::dev::repeated(&r.queries, t.n_plus_one).len();
            let _ = write!(
                b,
                "<tr><td><a href=\"/_engine/dev/requests/{}\">{} {}</a></td><td>{}</td><td>{}</td>\
                 <td>{}</td><td>{}</td></tr>",
                r.id,
                r.method,
                esc(&r.uri),
                r.status.as_u16(),
                ms(r.elapsed),
                r.queries.len(),
                if n1 > 0 { "yes" } else { "" }
            );
        }
        b.push_str("</tbody></table>");
    }
    dev_page(StatusCode::OK, "Requests", &b)
}

async fn request_page(State(t): State<Arc<DevTools>>, UrlPath(id): UrlPath<u64>) -> Response {
    let Some(r) = t.get(id) else {
        return not_found_page();
    };
    let title = format!("{} {} ({})", r.method, r.uri, r.status.as_u16());
    let mut b = String::new();
    if r.error.is_some() {
        b.push_str(&error_section(&t, &r));
    }
    b.push_str(&queries_section(&t, &r));
    b.push_str(&request_section(&r));
    dev_page(StatusCode::OK, &title, &b)
}

fn error_title(r: &Recorded) -> String {
    match &r.error {
        Some(e) => format!("500: {}", one_line(&e.to_string(), 120)),
        None => "500: the request failed".into(),
    }
}

fn error_text(r: &Recorded) -> String {
    let mut s = String::from("500 (engine dev; production answers without detail)\n\n");
    match &r.error {
        Some(e) => {
            for (i, c) in e.chain().enumerate() {
                let _ = writeln!(s, "{i}: {c}");
            }
        }
        None => s.push_str("No error was recorded with the response; see the terminal.\n"),
    }
    let _ = writeln!(s, "\nDetails: /_engine/dev/requests/{}", r.id);
    s
}

fn error_body(t: &DevTools, r: &Recorded) -> String {
    let mut b = format!(
        "<p>This page is shown by <code>engine dev</code> only: <code>engine serve</code> \
         answers with a bare 500 and logs the error. \
         <a href=\"/_engine/dev/requests/{}\">This request's page</a>.</p>",
        r.id
    );
    b.push_str(&error_section(t, r));
    b.push_str(&queries_section(t, r));
    b.push_str(&request_section(r));
    b
}

fn error_section(t: &DevTools, r: &Recorded) -> String {
    let Some(e) = &r.error else {
        return "<h2>Error</h2><p>No error was recorded with the response (the handler made the \
                500 itself); see the terminal.</p>"
            .into();
    };
    let mut b = String::from("<h2>Error chain</h2><ol class=\"err\">");
    for c in e.chain() {
        let _ = write!(b, "<li><pre>{}</pre></li>", esc(&c.to_string()));
    }
    b.push_str("</ol>");
    if let Some((name, line)) = engine_core::templates::error_location(e) {
        let _ = write!(
            b,
            "<h2>Template <code>{}</code>, line {line}</h2>",
            esc(&name)
        );
        match template_source(&t.site, &name) {
            Some((file, src)) => {
                let _ = write!(b, "<p>{}</p>{}", esc(&file), excerpt(&src, line, 6));
            }
            None => b.push_str("<p>(source not found)</p>"),
        }
    }
    b
}

/// Where template `name` comes from, and its text: the site's file, else
/// the built-in one.
fn template_source(site: &Path, name: &str) -> Option<(String, String)> {
    engine_core::templates::validate_template_name(name).ok()?;
    let p = site.join("templates").join(name);
    if let Ok(s) = std::fs::read_to_string(&p) {
        return Some((p.display().to_string(), s));
    }
    for d in engine_core::templates::core_dirs() {
        if let Some(s) = d.get_file(name).and_then(|f| f.contents_utf8()) {
            return Some((format!("built-in template {name}"), s.to_string()));
        }
    }
    None
}

/// Lines around `line` (1-based), numbered, the line itself marked.
fn excerpt(src: &str, line: usize, around: usize) -> String {
    let from = line.saturating_sub(around).max(1);
    let mut b = String::from("<table><tbody>");
    for (i, l) in src.lines().enumerate().skip(from - 1).take(2 * around + 1) {
        let n = i + 1;
        let cls = if n == line { " class=\"hl\"" } else { "" };
        let _ = write!(
            b,
            "<tr{cls}><th scope=\"row\">{n}</th><td><pre style=\"margin:0;padding:0;background:none\">{}</pre></td></tr>",
            esc(l)
        );
    }
    b.push_str("</tbody></table>");
    b
}

fn queries_section(t: &DevTools, r: &Recorded) -> String {
    let total: Duration = r.queries.iter().map(|q| q.duration).sum();
    let mut b = format!("<h2>Queries: {} in {}</h2>", r.queries.len(), ms(total));
    for (shape, n, site) in engine_core::dev::repeated(&r.queries, t.n_plus_one) {
        let _ = write!(
            b,
            "<p class=\"warn\">Likely N+1: this statement ran <strong>{n} times</strong> in one \
             request (threshold {}): <code>{}</code>{}. Load the rows in one query (a join, \
             <code>= ANY($1)</code>) instead of one per item.</p>",
            t.n_plus_one,
            esc(&one_line(&shape, 300)),
            site.map(|s| format!(" First run from <code>{}</code>", esc(&s)))
                .unwrap_or_default()
        );
    }
    if r.queries.is_empty() {
        b.push_str("<p>None.</p>");
        return b;
    }
    b.push_str(
        "<table><thead><tr><th scope=\"col\">#</th><th scope=\"col\">Time</th>\
         <th scope=\"col\">Rows</th><th scope=\"col\">Statement</th><th scope=\"col\">From</th>\
         </tr></thead><tbody>",
    );
    for (i, q) in r.queries.iter().enumerate() {
        let err = q
            .error
            .as_ref()
            .map(|e| format!("<p class=\"err\">{}</p>", esc(e)))
            .unwrap_or_default();
        let _ = write!(
            b,
            "<tr><td>{}</td><td>{}</td><td>{}</td><td><pre style=\"margin:0\">{}</pre>{err}</td><td>{}</td></tr>",
            i + 1,
            ms(q.duration),
            rows(q),
            esc(q.sql.trim()),
            esc(&q.call_site().unwrap_or_default())
        );
    }
    b.push_str("</tbody></table>");
    if r.queries.len() >= engine_core::dev::MAX_QUERIES {
        let _ = write!(
            b,
            "<p class=\"warn\">Only the first {} queries are kept.</p>",
            engine_core::dev::MAX_QUERIES
        );
    }
    b
}

fn request_section(r: &Recorded) -> String {
    let mut b = format!(
        "<h2>Request</h2><p><code>{} {}</code>: {} in {}</p><table><tbody>",
        r.method,
        esc(&r.uri),
        r.status.as_u16(),
        ms(r.elapsed)
    );
    for (k, v) in &r.headers {
        let _ = write!(
            b,
            "<tr><th scope=\"row\">{}</th><td>{}</td></tr>",
            esc(k),
            esc(v)
        );
    }
    b.push_str("</tbody></table>");
    b
}

// ----- 404: the nearest routes ---------------------------------------------------

fn not_found_body(t: &DevTools, path: &str, id: u64) -> String {
    let mut b = format!(
        "<p>No route, page or file answers <code>{}</code>. \
         <a href=\"/_engine/dev/requests/{id}\">This request's page</a>.</p>",
        esc(path)
    );
    let listing = crate::routes::list(&t.site);
    let routes: Vec<crate::routes::Entry> = match listing {
        Ok(l) => l
            .routes
            .into_iter()
            .filter(|e| e.kind != "engine")
            .collect(),
        Err(e) => {
            let _ = write!(
                b,
                "<p class=\"err\">Listing the routes failed: {}</p>",
                esc(&format!("{e:#}"))
            );
            Vec::new()
        }
    };
    let near = nearest(path, &routes, 8);
    if near.is_empty() {
        b.push_str("<p>No route is close to it.</p>");
    } else {
        b.push_str("<h2>Did you mean</h2><ul>");
        for e in &near {
            let link = if e.path.contains('{') || e.path.contains('*') {
                format!("<code>{}</code>", esc(&e.path))
            } else {
                format!("<a href=\"{0}\"><code>{0}</code></a>", esc(&e.path))
            };
            let _ = write!(b, "<li>{link} ({}, {})</li>", esc(e.kind), esc(&e.source));
        }
        b.push_str("</ul>");
    }
    if t.site.join("templates/404.html").is_file() || t.site.join("content/404.md").is_file() {
        b.push_str("<p>Visitors get the site's own 404 page.</p>");
    }
    let _ = write!(
        b,
        "<details><summary>All {} routes (<code>engine routes</code>)</summary><ul>",
        routes.len()
    );
    for e in &routes {
        let _ = write!(
            b,
            "<li><code>{}</code> {} ({})</li>",
            esc(&e.path),
            esc(&e.methods.join(", ")),
            esc(&e.source)
        );
    }
    b.push_str("</ul></details>");
    b
}

/// The routes closest to `path`, nearest first: edit distance between the
/// paths, where a `{param}` segment of a pattern takes the requested
/// segment at its place, and a trailing slash does not count.
fn nearest<'a>(
    path: &str,
    routes: &'a [crate::routes::Entry],
    n: usize,
) -> Vec<&'a crate::routes::Entry> {
    let want = path.trim_end_matches('/').to_ascii_lowercase();
    let want_segs: Vec<&str> = want.split('/').collect();
    let mut scored: Vec<(usize, &crate::routes::Entry)> = routes
        .iter()
        .map(|e| {
            let p = e.path.trim_end_matches('/').to_ascii_lowercase();
            let filled: Vec<String> = p
                .split('/')
                .enumerate()
                .map(|(i, s)| {
                    if s.starts_with('{') && s.ends_with('}') {
                        want_segs.get(i).copied().unwrap_or(s).to_string()
                    } else {
                        s.to_string()
                    }
                })
                .collect();
            (levenshtein(&want, &filled.join("/")), e)
        })
        .collect();
    let limit = (want.chars().count() / 2).max(3);
    scored.retain(|(d, _)| *d <= limit);
    scored.sort_by_key(|(d, e)| (*d, e.path.len()));
    scored.into_iter().take(n).map(|(_, e)| e).collect()
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

// ----- 403 from /_engine/live ------------------------------------------------------

#[allow(unused_variables, unused_mut)]
fn live_refusal(t: &DevTools, query: Option<&str>) -> String {
    let mut s = String::from(
        "403: a tag this stream asked for is not public (engine dev explains; production says \
         only \"Not a public tag\").\n\n",
    );
    #[cfg(feature = "realtime")]
    {
        let public = t
            .live_public
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        if let Ok((tags, _)) = engine_realtime::parse_query(query) {
            for tag in tags {
                let verdict = match tag.parse::<engine_core::tags::CacheTag>() {
                    Err(_) => {
                        "refused: not a cache tag (c:<type>, i:<type>:<id>, s:<name>)".to_string()
                    }
                    Ok(ct) => match &public {
                        Some(p) if p.allows(&ct) => "public".to_string(),
                        _ => "refused: not public".to_string(),
                    },
                };
                let _ = writeln!(s, "  {tag}: {verdict}");
            }
        }
        let declared = crate::app::live_public();
        let _ = writeln!(
            s,
            "\nThe rule: /_engine/live streams public tags only, which are\n\
             - the site's content types and vocabularies: c:<type>, i:<type>:<id>, s:<singleton type>\n\
             - what the app crate declares with App::live_public: {}\n\
             Anything else (a site's own table, media, anything personal) is private: stream it from\n\
             one of the site's handlers after checking the visitor (Context::live), and point the\n\
             live_region's `endpoint` at that handler. Docs: \"Live regions\".",
            if declared.is_empty() {
                "(none)".to_string()
            } else {
                declared.join(", ")
            }
        );
    }
    s
}

// ----- mail ---------------------------------------------------------------------------

#[cfg(feature = "mail")]
fn mailer(t: &DevTools) -> Option<engine_mail::Mailer> {
    t.mailer.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

#[cfg(feature = "mail")]
async fn mail_list(State(t): State<Arc<DevTools>>) -> Response {
    let Some(m) = mailer(&t) else {
        return dev_page(
            StatusCode::OK,
            "Mail",
            "<p>This site has no mailer (the <code>jobs</code> feature runs it).</p>",
        );
    };
    let mut b = String::from(
        "<p>Under <code>engine dev</code> every message is kept here instead of sent: \
         the job queue and the templates work as in production, nothing reaches an SMTP server.</p>",
    );
    let list = m.captured().map(|c| c.list()).unwrap_or_default();
    b.push_str("<h2>Captured</h2>");
    if list.is_empty() {
        b.push_str("<p>None yet.</p>");
    } else {
        b.push_str(
            "<table><thead><tr><th scope=\"col\">Subject</th><th scope=\"col\">To</th>\
             <th scope=\"col\">Template</th><th scope=\"col\">Date</th></tr></thead><tbody>",
        );
        for c in list {
            let _ = write!(
                b,
                "<tr><td><a href=\"/_engine/dev/mail/{}\">{}</a></td><td>{}</td><td>{}</td><td>{}</td></tr>",
                c.id,
                esc(&c.subject),
                esc(&c.to),
                esc(&c.template),
                esc(&c.at)
            );
        }
        b.push_str("</tbody></table>");
    }
    b.push_str(
        "<h2>Templates</h2><p>Each rendered with its sample data \
         (<code>templates/mail/&lt;name&gt;.sample.json</code>), without sending.</p><ul>",
    );
    for n in m.templates().names() {
        let _ = write!(
            b,
            "<li><a href=\"/_engine/dev/mail/templates/{0}\">{0}</a></li>",
            esc(&n)
        );
    }
    b.push_str("</ul>");
    dev_page(StatusCode::OK, "Mail", &b)
}

#[cfg(feature = "mail")]
async fn mail_page(State(t): State<Arc<DevTools>>, UrlPath(id): UrlPath<u64>) -> Response {
    let Some(c) = mailer(&t).and_then(|m| m.captured()?.get(id)) else {
        return not_found_page();
    };
    let mut b = String::from("<h2>Headers</h2><table><tbody>");
    for (k, v) in &c.headers {
        let _ = write!(
            b,
            "<tr><th scope=\"row\">{}</th><td>{}</td></tr>",
            esc(k),
            esc(v)
        );
    }
    let _ = write!(
        b,
        "</tbody></table><p>Template <code>{}</code>.</p>",
        esc(&c.template)
    );
    b.push_str(&parts_html(
        &c.text,
        c.html.is_some(),
        &format!("/_engine/dev/mail/{id}/html"),
    ));
    dev_page(StatusCode::OK, &c.subject, &b)
}

#[cfg(feature = "mail")]
fn parts_html(text: &str, html: bool, src: &str) -> String {
    let mut b = String::new();
    if html {
        let _ = write!(
            b,
            "<h2>HTML</h2><iframe title=\"The HTML part\" sandbox src=\"{src}\"></iframe>"
        );
    } else {
        b.push_str("<h2>HTML</h2><p>No HTML part (text only).</p>");
    }
    let _ = write!(b, "<h2>Text</h2><pre>{}</pre>", esc(text));
    b
}

/// A message's HTML part, sandboxed (no scripts, its own origin).
#[cfg(feature = "mail")]
fn sandboxed(html: String) -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
            (header::CONTENT_SECURITY_POLICY, "sandbox"),
        ],
        html,
    )
        .into_response()
}

#[cfg(feature = "mail")]
async fn mail_html(State(t): State<Arc<DevTools>>, UrlPath(id): UrlPath<u64>) -> Response {
    match mailer(&t).and_then(|m| m.captured()?.get(id)?.html) {
        Some(h) => sandboxed(h),
        None => not_found_page(),
    }
}

#[cfg(feature = "mail")]
fn preview(t: &DevTools, name: &str) -> Option<anyhow::Result<engine_mail::Outgoing>> {
    let m = mailer(t)?;
    Some(
        m.templates()
            .sample(name)
            .and_then(|data| m.render("ada@example.org", "Ada", name, data)),
    )
}

#[cfg(feature = "mail")]
async fn template_page(State(t): State<Arc<DevTools>>, UrlPath(name): UrlPath<String>) -> Response {
    match preview(&t, &name) {
        None => not_found_page(),
        Some(Err(e)) => dev_page(
            StatusCode::OK,
            &format!("Template {name}"),
            &format!(
                "<p class=\"err\">It does not render with its sample data:</p><pre>{}</pre>",
                esc(&format!("{e:#}"))
            ),
        ),
        Some(Ok(o)) => {
            let mut b = format!(
                "<p>A preview with the sample data of <code>templates/mail/{0}.sample.json</code> \
                 (the site's file, else the built-in one, else no data), to Ada \
                 &lt;ada@example.org&gt;. Nothing is sent.</p><p>Subject: <strong>{1}</strong></p>",
                esc(&name),
                esc(&o.subject)
            );
            b.push_str(&parts_html(
                &o.text,
                o.html.is_some(),
                &format!("/_engine/dev/mail/templates/{}/html", esc(&name)),
            ));
            dev_page(StatusCode::OK, &format!("Template {name}"), &b)
        }
    }
}

#[cfg(feature = "mail")]
async fn template_html(State(t): State<Arc<DevTools>>, UrlPath(name): UrlPath<String>) -> Response {
    match preview(&t, &name) {
        Some(Ok(o)) => match o.html {
            Some(h) => sandboxed(h),
            None => not_found_page(),
        },
        _ => not_found_page(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str) -> crate::routes::Entry {
        crate::routes::Entry {
            kind: "page",
            methods: vec!["GET"],
            path: path.into(),
            mode: None,
            template: None,
            source: "test".into(),
            tags: vec![],
            note: None,
        }
    }

    #[test]
    fn nearest_routes_first_and_patterns_fill_in() {
        let routes = vec![
            entry("/about/"),
            entry("/blog/"),
            entry("/blog/{slug}/"),
            entry("/contact/"),
        ];
        let near: Vec<&str> = nearest("/abuot/", &routes, 3)
            .iter()
            .map(|e| e.path.as_str())
            .collect();
        assert_eq!(near.first(), Some(&"/about/"), "{near:?}");
        let near: Vec<&str> = nearest("/blgo/hello/", &routes, 3)
            .iter()
            .map(|e| e.path.as_str())
            .collect();
        assert_eq!(near.first(), Some(&"/blog/{slug}/"), "{near:?}");
        assert!(nearest("/completely/unrelated/thing/", &routes, 3).is_empty());
    }

    #[test]
    fn secrets_are_not_shown() {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("hw_session=abc; theme=dark"),
        );
        h.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer xyz"),
        );
        let shown = shown_headers(&h);
        let all = format!("{shown:?}");
        assert!(
            !all.contains("abc") && !all.contains("xyz") && !all.contains("dark"),
            "{all}"
        );
        assert!(all.contains("hw_session=(hidden)"), "{all}");
    }

    #[test]
    fn excerpt_marks_the_line() {
        let src = "a\nb\nc\nd\ne\n";
        let x = excerpt(src, 3, 1);
        assert!(
            x.contains("<tr class=\"hl\"><th scope=\"row\">3</th>"),
            "{x}"
        );
        assert!(
            x.contains(">2<") && x.contains(">4<") && !x.contains(">5<"),
            "{x}"
        );
        assert_eq!(
            esc("<a href=\"x\">&'"),
            "&lt;a href=&quot;x&quot;&gt;&amp;&#39;"
        );
    }
}
