//! Fragments: parts of a page rendered once and reused, the `request`
//! render mode's cache.
//!
//! A template marks a fragment with
//!
//! ```jinja
//! {{ fragment("components/latest.html", key="home", limit=5) }}
//! {{ fragment("components/my_orders.html", per_user=true) }}
//! ```
//!
//! `fragment(template, key=None, per_user=false, **args)` renders
//! `template` with `args` plus `site`, `page`, `ui`, `engine` and `route`.
//! In a `request`-mode render the HTML is kept in an in-memory cache
//! ([`FragmentCache`], `moka`) under the template name, `key` (without a
//! key, the page's URL), the args and a *scope*, together with the cache
//! tags the fragment's render recorded.
//! A cache hit re-records those tags into the page, so pages still know what
//! they show.
//!
//! **Personal data never crosses users:**
//!
//! * A shared fragment (the default) is rendered *without* `user`,
//!   `request` or any CSRF token in its context, so it cannot show anything
//!   personal even by mistake; one copy serves everyone.
//! * `per_user=true` gives the fragment `user`, and its scope is the
//!   signed-in account's id (or `anon` for visitors), so each account has its
//!   own copy. Forms, which carry a per-session token, do not belong in
//!   fragments.
//!
//! **Eviction** is by the same cache tags (`i:`, `c:`, `s:`) and the same
//! change notifications that drive `publish`: when content changes, the
//! publisher reports the changed tags and every fragment carrying one is
//! dropped ([`FragmentCache::evict`]). Fragments also expire after
//! `[cache] fragment_ttl_seconds` and the cache is bounded by
//! `[cache] max_mb`. Template changes start a fresh cache.
//!
//! Outside `request` renders (`engine build`, `publish`) `fragment()` simply
//! renders the template: files on disk are not built from cached parts.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use minijinja::value::{Kwargs, Value};
use minijinja::{Error, ErrorKind, State};
use serde::{Deserialize, Serialize};

use crate::tags::{self, CacheTag};

/// `[cache]` in `site.toml`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CacheConfig {
    /// Keep fragments at all (in `request` renders).
    #[serde(default = "yes")]
    pub fragments: bool,
    /// Seconds a fragment lives at most (also without any change).
    #[serde(default = "d_ttl")]
    pub fragment_ttl_seconds: u64,
    /// Memory budget of the fragment cache, in MiB (unset: sized from the
    /// machine, see [`crate::limits::fragment_cache_bytes`]).
    #[serde(default)]
    pub max_mb: Option<u64>,
}

fn yes() -> bool {
    true
}
fn d_ttl() -> u64 {
    600
}

impl Default for CacheConfig {
    fn default() -> Self {
        CacheConfig {
            fragments: true,
            fragment_ttl_seconds: d_ttl(),
            max_mb: None,
        }
    }
}

/// A cached fragment.
#[derive(Debug)]
pub struct Entry {
    pub html: String,
    pub tags: Vec<CacheTag>,
}

/// A `request`-mode page's ETag and the tags its render recorded.
#[derive(Debug)]
pub struct PageEntry {
    pub etag: Arc<str>,
    pub tags: Vec<CacheTag>,
}

/// Hits and misses of one render (see [`track`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenderStats {
    pub hits: u32,
    pub misses: u32,
}

thread_local! {
    static STATS: Cell<Option<RenderStats>> = const { Cell::new(None) };
}

/// Run a (synchronous) render and count its fragment hits and misses.
pub fn track<T>(f: impl FnOnce() -> T) -> (T, RenderStats) {
    let prev = STATS.with(|s| s.replace(Some(RenderStats::default())));
    let out = f();
    let stats = STATS.with(|s| s.replace(prev)).unwrap_or_default();
    (out, stats)
}

fn count(hit: bool) {
    STATS.with(|s| {
        if let Some(mut st) = s.get() {
            if hit {
                st.hits += 1;
            } else {
                st.misses += 1;
            }
            s.set(Some(st));
        }
    });
}

/// The in-memory fragment cache.
///
/// It also keeps the ETags of whole `request`-mode pages with the cache tags
/// their render recorded ([`Self::page_etag`]), evicted by the same tags, so
/// a browser revalidating an unchanged page gets a `304` without a render.
pub struct FragmentCache {
    #[cfg(feature = "request")]
    cache: moka::sync::Cache<String, Arc<Entry>>,
    #[cfg(feature = "request")]
    pages: moka::sync::Cache<String, Arc<PageEntry>>,
    /// Counts evictions; a page render that saw it change does not keep its
    /// ETag (it may have read data from before the change).
    epoch: AtomicU64,
    hits: AtomicU64,
    misses: AtomicU64,
    evictions: AtomicU64,
}

impl std::fmt::Debug for FragmentCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FragmentCache")
            .field("hits", &self.hits)
            .field("misses", &self.misses)
            .finish()
    }
}

/// Totals since start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CacheStats {
    pub entries: u64,
    pub hits: u64,
    pub misses: u64,
    pub evicted: u64,
}

impl FragmentCache {
    #[cfg(feature = "request")]
    pub fn new(cfg: &CacheConfig) -> Self {
        let cache = moka::sync::Cache::builder()
            .max_capacity(cfg.max_mb.map_or_else(
                || crate::limits::fragment_cache_bytes(crate::resources::get()),
                |m| m.max(1) * 1024 * 1024,
            ))
            .weigher(|k: &String, v: &Arc<Entry>| {
                u32::try_from(k.len() + v.html.len() + 64 * v.tags.len()).unwrap_or(u32::MAX)
            })
            .time_to_live(std::time::Duration::from_secs(
                cfg.fragment_ttl_seconds.max(1),
            ))
            .support_invalidation_closures()
            .build();
        // Page entries are small (a key, an ETag, a few tags): bounded by
        // count, like the file ETag cache, and living as long as fragments.
        let pages = moka::sync::Cache::builder()
            .max_capacity(crate::limits::current().etag_cache_entries as u64)
            .time_to_live(std::time::Duration::from_secs(
                cfg.fragment_ttl_seconds.max(1),
            ))
            .support_invalidation_closures()
            .build();
        FragmentCache {
            cache,
            pages,
            epoch: AtomicU64::new(0),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
        }
    }

    pub fn get(&self, key: &str) -> Option<Arc<Entry>> {
        #[cfg(feature = "request")]
        {
            let e = self.cache.get(key);
            if e.is_some() {
                self.hits.fetch_add(1, Ordering::Relaxed);
            } else {
                self.misses.fetch_add(1, Ordering::Relaxed);
            }
            e
        }
        #[cfg(not(feature = "request"))]
        {
            let _ = key;
            None
        }
    }

    pub fn insert(&self, key: String, html: String, tags: Vec<CacheTag>) {
        #[cfg(feature = "request")]
        self.cache.insert(key, Arc::new(Entry { html, tags }));
        #[cfg(not(feature = "request"))]
        let _ = (key, html, tags);
    }

    /// The current eviction count: read it before a page render and give
    /// it to [`Self::remember_page`].
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }

    /// The ETag of the page at `key` (path and query) as last rendered, if
    /// nothing it read has changed since.
    pub fn page_etag(&self, key: &str) -> Option<Arc<str>> {
        #[cfg(feature = "request")]
        {
            self.pages.get(key).map(|e| e.etag.clone())
        }
        #[cfg(not(feature = "request"))]
        {
            let _ = key;
            None
        }
    }

    /// Keep a rendered page's ETag with its tags, unless an eviction
    /// happened since `epoch` (the render may predate the change).
    pub fn remember_page(&self, key: String, etag: Arc<str>, tags: Vec<CacheTag>, epoch: u64) {
        #[cfg(feature = "request")]
        {
            if self.epoch() != epoch {
                return;
            }
            self.pages
                .insert(key.clone(), Arc::new(PageEntry { etag, tags }));
            // An eviction between the check and the insert may have missed
            // the new entry: drop it then.
            if self.epoch() != epoch {
                self.pages.invalidate(&key);
            }
        }
        #[cfg(not(feature = "request"))]
        let _ = (key, etag, tags, epoch);
    }

    /// Drop every fragment and page ETag that recorded one of `tags`
    /// (strings like `i:article:3`; unparsable ones are ignored). `*` drops
    /// everything.
    pub fn evict(&self, changed: &[String]) {
        if changed.is_empty() {
            return;
        }
        self.epoch.fetch_add(1, Ordering::SeqCst);
        if changed.iter().any(|t| t == "*") {
            self.clear();
            return;
        }
        let set: std::collections::BTreeSet<CacheTag> =
            changed.iter().filter_map(|t| t.parse().ok()).collect();
        if set.is_empty() {
            return;
        }
        self.evictions.fetch_add(1, Ordering::Relaxed);
        #[cfg(feature = "request")]
        {
            let set = Arc::new(set);
            let s = set.clone();
            if self
                .cache
                .invalidate_entries_if(move |_, e| e.tags.iter().any(|t| s.contains(t)))
                .is_err()
            {
                self.cache.invalidate_all();
            }
            if self
                .pages
                .invalidate_entries_if(move |_, e| e.tags.iter().any(|t| set.contains(t)))
                .is_err()
            {
                self.pages.invalidate_all();
            }
            self.cache.run_pending_tasks();
            self.pages.run_pending_tasks();
        }
    }

    pub fn clear(&self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
        #[cfg(feature = "request")]
        {
            self.cache.invalidate_all();
            self.pages.invalidate_all();
            self.cache.run_pending_tasks();
            self.pages.run_pending_tasks();
        }
    }

    pub fn stats(&self) -> CacheStats {
        CacheStats {
            #[cfg(feature = "request")]
            entries: self.cache.entry_count(),
            #[cfg(not(feature = "request"))]
            entries: 0,
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            evicted: self.evictions.load(Ordering::Relaxed),
        }
    }
}

/// Keys of the context a fragment may see. `user` only with `per_user`;
/// `request` and CSRF tokens never.
const SHARED_CONTEXT: &[&str] = &["site", "page", "ui", "engine", "route", "listing"];

/// The `fragment()` template function, caching into `cache` (when given)
/// during `request` renders.
pub fn fragment_fn(
    cache: Option<Arc<FragmentCache>>,
) -> impl Fn(&State, String, Kwargs) -> Result<Value, Error> + Send + Sync + 'static {
    move |state: &State, template: String, kwargs: Kwargs| {
        crate::templates::validate_template_name(&template)
            .map_err(|e| Error::new(ErrorKind::InvalidOperation, e))?;
        let key: Option<String> = kwargs.get("key")?;
        let per_user: Option<bool> = kwargs.get("per_user")?;
        let per_user = per_user.unwrap_or(false);
        let mut ctx = std::collections::BTreeMap::<String, Value>::new();
        for k in SHARED_CONTEXT {
            if let Some(v) = state.lookup(k) {
                ctx.insert((*k).to_string(), v);
            }
        }
        let mut args = std::collections::BTreeMap::<String, Value>::new();
        for k in kwargs.args() {
            if k == "key" || k == "per_user" {
                continue;
            }
            args.insert(k.to_string(), kwargs.get::<Value>(k)?);
        }
        let user = state
            .lookup("user")
            .filter(|u| !u.is_undefined() && !u.is_none());
        let scope = if per_user {
            // The account id is the scope; never the name or email.
            let id = user
                .as_ref()
                .and_then(|u| u.get_attr("id").ok())
                .filter(|v| !v.is_undefined() && !v.is_none());
            if let Some(u) = &user {
                ctx.insert("user".into(), strip_csrf(u));
            }
            match id {
                Some(id) => format!("u:{id}"),
                None => "anon".to_string(),
            }
        } else {
            "shared".to_string()
        };
        ctx.extend(args.clone());
        // Only in `request` renders, and never in previews of drafts.
        let in_request = state
            .lookup("request")
            .is_some_and(|r| !r.is_undefined() && !r.is_none())
            && !state.lookup("preview").is_some_and(|p| p.is_true());
        let cache = cache.as_ref().filter(|_| in_request);
        let ckey = cache.map(|_| {
            format!(
                "{template}\u{1f}{}\u{1f}{}\u{1f}{scope}\u{1f}{}",
                key.as_deref().unwrap_or(""),
                serde_json::to_string(&args).unwrap_or_default(),
                // Without a key the fragment belongs to its page.
                if key.is_some() {
                    String::new()
                } else {
                    state
                        .lookup("page")
                        .and_then(|p| p.get_attr("url").ok())
                        .map(|u| u.to_string())
                        .unwrap_or_default()
                }
            )
        });
        if let (Some(c), Some(k)) = (cache, &ckey)
            && let Some(e) = c.get(k)
        {
            count(true);
            for t in &e.tags {
                tags::record(t.clone());
            }
            return Ok(Value::from_safe_string(e.html.clone()));
        }
        let tmpl = state.env().get_template(&template)?;
        let (html, recorded) = tags::collect(|| tmpl.render(Value::from(ctx)));
        let html = html?;
        if let (Some(c), Some(k)) = (cache, ckey) {
            count(false);
            c.insert(k, html.clone(), recorded.into_vec());
        }
        Ok(Value::from_safe_string(html))
    }
}

/// The user value without its session token.
fn strip_csrf(u: &Value) -> Value {
    let mut m = crate::templates::value_map(u);
    m.remove("csrf");
    Value::from(m)
}

#[cfg(all(test, feature = "request"))]
mod tests {
    use super::*;
    use minijinja::{Environment, context};

    fn env(cache: Arc<FragmentCache>) -> Environment<'static> {
        let mut env = Environment::new();
        env.set_undefined_behavior(minijinja::UndefinedBehavior::Chainable);
        env.add_template(
            "f.html",
            "[{% set _ = tag('menu') %}{{ label }}|{{ user.name }}|{{ request.path }}|{{ user.csrf }}]",
        )
        .unwrap();
        env.add_function("tag", |n: String| tags::record(CacheTag::singleton(n)));
        env.add_function("fragment", fragment_fn(Some(cache)));
        env
    }

    #[test]
    fn page_etags_follow_tags_and_skip_racing_renders() {
        let c = FragmentCache::new(&CacheConfig::default());
        let e = c.epoch();
        let tags = vec![
            CacheTag::collection("search"),
            CacheTag::item("article", "1"),
        ];
        c.remember_page("/search/?q=a".into(), "\"r1\"".into(), tags, e);
        assert_eq!(c.page_etag("/search/?q=a").as_deref(), Some("\"r1\""));
        c.evict(&["i:article:2".into()]);
        assert!(
            c.page_etag("/search/?q=a").is_some(),
            "an unrelated change keeps it"
        );
        c.evict(&["c:search".into()]);
        assert!(
            c.page_etag("/search/?q=a").is_none(),
            "a change it read drops it"
        );
        // A render that started before an eviction keeps nothing.
        let e = c.epoch();
        c.evict(&["i:article:9".into()]);
        c.remember_page("/x/".into(), "\"r2\"".into(), vec![], e);
        assert!(c.page_etag("/x/").is_none());
        c.remember_page("/x/".into(), "\"r2\"".into(), vec![], c.epoch());
        c.clear();
        assert!(c.page_etag("/x/").is_none());
    }

    #[test]
    fn shared_fragments_cannot_see_the_user_and_per_user_ones_are_keyed_by_id() {
        let cache = Arc::new(FragmentCache::new(&CacheConfig::default()));
        let env = env(cache.clone());
        let page = |name: &str, id: i64| {
            let ctx = context! {
                user => context! { id => id, name => name, csrf => "SECRET" },
                request => context! { path => "/x/" },
            };
            track(|| {
                tags::collect(|| {
                    env.render_str(
                        r#"{{ fragment("f.html", key="k", label="L") }}{{ fragment("f.html", per_user=true, label="P") }}"#,
                        ctx,
                    )
                    .unwrap()
                })
            })
        };
        let ((a, tags_a), s1) = page("Ann", 1);
        assert_eq!(
            a, "[L|||][P|Ann||]",
            "no user, request or token in shared; no token at all"
        );
        assert_eq!(s1, RenderStats { hits: 0, misses: 2 });
        assert_eq!(tags_a.into_vec(), [CacheTag::singleton("menu")]);
        let ((b, tags_b), s2) = page("Bob", 2);
        assert_eq!(
            b, "[L|||][P|Bob||]",
            "Bob never gets Ann's personal fragment"
        );
        assert_eq!(s2, RenderStats { hits: 1, misses: 1 });
        assert_eq!(
            tags_b.into_vec(),
            [CacheTag::singleton("menu")],
            "hits re-record tags"
        );
        let ((a2, _), s3) = page("Ann", 1);
        assert_eq!(a2, a);
        assert_eq!(s3, RenderStats { hits: 2, misses: 0 });
        // A change to the tag drops every fragment that showed it.
        cache.evict(&["s:menu".into()]);
        let (_, s4) = page("Ann", 1);
        assert_eq!(s4, RenderStats { hits: 0, misses: 2 });
        cache.evict(&["s:other".into(), "garbage".into()]);
        let (_, s5) = page("Ann", 1);
        assert_eq!(s5, RenderStats { hits: 2, misses: 0 });
    }

    #[test]
    fn outside_request_renders_nothing_is_cached() {
        let cache = Arc::new(FragmentCache::new(&CacheConfig::default()));
        let env = env(cache.clone());
        let (out, stats) = track(|| {
            env.render_str(r#"{{ fragment("f.html", label="B") }}"#, context! {})
                .unwrap()
        });
        assert_eq!(out, "[B|||]");
        assert_eq!(stats, RenderStats::default());
        assert_eq!(cache.stats().entries, 0);
    }
}
