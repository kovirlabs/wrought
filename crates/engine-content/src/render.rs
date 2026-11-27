//! Content in templates and routes.
//!
//! [`ContentSite`] is the `content` feature's [`SiteExtension`]: it expands
//! `[[routes]]` with `type` / `taxonomy` into one page per published item or
//! term, and adds template functions:
//!
//! | Function | Returns | Records |
//! | --- | --- | --- |
//! | `list_items(type, limit=, offset=, order=, term=)` | published items | `c:<type>` |
//! | `get_item(type, id_or_slug)` | one published item or undefined | `i:<type>:<id>` (`c:<type>` if missing) |
//! | `get_singleton(type)` | the singleton item or undefined | `s:<type>` |
//! | `list_terms(vocabulary)` | terms, by name | `c:<vocabulary>` |
//! | `render_block(b)` | `blocks/<b.type>.html` rendered with `block` | — |
//!
//! The names follow core's `get_page` / `section_pages` and avoid short
//! words (`items`, `item`) that templates and components use as variables.
//!
//! Item values are safe to print: Markdown fields hold sanitised HTML
//! marked *safe* (print with `{{ page.body }}`, no filter), URL fields hold
//! only safe links (or nothing), and everything else is plain data that
//! templates auto-escape. No template needs `| safe`.
//!
//! The extension's methods block on database queries, so they must run on a
//! thread that may block (the CLI's main thread, or `spawn_blocking`), never
//! directly on an async worker.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, RwLock};

use anyhow::{Result, bail};
use engine_core::builder::{DynamicPage, SiteExtension};
use engine_core::config::{ListConfig, SiteConfig};
use engine_core::routes::{fill_pattern, normalize_path, page_url};
use engine_core::tags::{self, CacheTag};
use minijinja::value::{Kwargs, Value};
use minijinja::{Error, ErrorKind, State, context};
use serde_json::{Map, Value as Json};

use crate::sanitize;
use crate::slug;
use crate::store::{Item, Key, ListQuery, Status, Store, Term};
use crate::types::{BLOCK_ID, FieldKind, Registry, TypeDef, valid_block_id, valid_name};

#[derive(Debug, Clone)]
struct PatternRoute {
    pattern: String,
    param: String,
}

/// A paginated list route (`[routes.list]`).
#[derive(Debug, Clone)]
struct ListRoute {
    /// Normalised path of page 1, e.g. `/articles/`.
    base: String,
    list: ListConfig,
}

struct Inner {
    store: Store,
    rt: tokio::runtime::Handle,
    type_routes: BTreeMap<String, PatternRoute>,
    term_routes: BTreeMap<String, PatternRoute>,
    /// By route index.
    list_routes: BTreeMap<usize, ListRoute>,
    /// Every item of every type, loaded once for a whole-site render (see
    /// [`ContentSite::preload`]).
    snapshot: RwLock<Option<Arc<Snapshot>>>,
    /// Off: whole-site renders query per page like the rest (tests compare
    /// both; `ENGINE_BUILD_NO_PRELOAD=1` for the CLI).
    preload: std::sync::atomic::AtomicBool,
    /// Items' template values, reused while the item is unchanged.
    values: crate::values::ValueCache,
}

/// One content type's items, read in one query (with their terms).
struct TypeSnap {
    /// All items, drafts included (related-item ranking counts them, as the
    /// SQL does), in the database's `newest` order.
    items: Vec<Item>,
    /// Term fields: (field name, vocabulary).
    term_fields: Vec<(String, String)>,
    by_id: HashMap<i64, usize>,
    by_slug: HashMap<String, usize>,
    /// Term ids of each item, and items of each term.
    terms_of: HashMap<i64, Vec<i64>>,
    items_of: HashMap<i64, Vec<i64>>,
}

/// The whole site's content at one moment, for `engine build` and full
/// rebuilds: one query per type instead of several per page (`get_item`,
/// `get_singleton`, `related_items`). Item values (rendered Markdown,
/// sanitised blocks) are made once per item and shared by every page that
/// shows it. Pages record exactly the cache tags they would without it.
struct Snapshot {
    types: HashMap<String, TypeSnap>,
    values: Mutex<HashMap<(String, i64), Value>>,
}

impl TypeSnap {
    fn new(t: &TypeDef, items: Vec<Item>) -> Self {
        let term_fields: Vec<&str> = t
            .fields
            .iter()
            .filter(|f| f.kind == FieldKind::Terms)
            .map(|f| f.name.as_str())
            .collect();
        // `items` arrive in the store's `newest` order, which in-memory
        // lists reuse (so they order exactly as the database does).
        let mut by_id = HashMap::new();
        let mut by_slug = HashMap::new();
        let mut terms_of: HashMap<i64, Vec<i64>> = HashMap::new();
        let mut items_of: HashMap<i64, Vec<i64>> = HashMap::new();
        for (n, it) in items.iter().enumerate() {
            by_id.insert(it.id, n);
            by_slug.insert(it.slug.clone(), n);
            for f in &term_fields {
                let list = it.fields.get(*f).and_then(Json::as_array);
                // Term values are `{id, slug, name}` objects.
                for id in list
                    .into_iter()
                    .flatten()
                    .filter_map(|t| t.get("id")?.as_i64())
                {
                    terms_of.entry(it.id).or_default().push(id);
                    items_of.entry(id).or_default().push(it.id);
                }
            }
        }
        TypeSnap {
            term_fields: t
                .fields
                .iter()
                .filter(|f| f.kind == FieldKind::Terms)
                .map(|f| (f.name.clone(), f.vocabulary.clone().unwrap_or_default()))
                .collect(),
            items,
            by_id,
            by_slug,
            terms_of,
            items_of,
        }
    }

    fn get(&self, key: &Key) -> Option<&Item> {
        let n = match key {
            Key::Id(i) => self.by_id.get(i)?,
            Key::Slug(s) => self.by_slug.get(s)?,
        };
        Some(&self.items[*n])
    }

    /// The same ranking as `Store::related`: items sharing the most terms
    /// (ties: newest id first), the first `limit * 4` candidates of any
    /// status, then the published ones up to `limit`.
    fn related(&self, id: i64, limit: i64) -> Vec<&Item> {
        let limit = limit.clamp(1, 50);
        let Some(terms) = self.terms_of.get(&id) else {
            return Vec::new();
        };
        let mut shared: HashMap<i64, i64> = HashMap::new();
        for t in terms {
            for other in self.items_of.get(t).into_iter().flatten() {
                if *other != id {
                    *shared.entry(*other).or_default() += 1;
                }
            }
        }
        let mut ranked: Vec<(i64, i64)> = shared.into_iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then(b.0.cmp(&a.0)));
        ranked
            .into_iter()
            .take((limit * 4) as usize)
            .filter_map(|(i, _)| self.get(&Key::Id(i)))
            .filter(|i| i.status == Status::Published)
            .take(limit as usize)
            .collect()
    }

    /// The store's list query answered from memory, or `None` when it needs
    /// the database (another order, a search, an ambiguous term): the same
    /// filter, the same order, the same page.
    fn list(&self, q: &ListQuery) -> Option<Vec<&Item>> {
        let reverse = match q.order.as_str() {
            "" | "newest" => false,
            "oldest" => true,
            _ => return None,
        };
        if q.search.as_deref().is_some_and(|s| !s.trim().is_empty()) {
            return None;
        }
        let term = match &q.term {
            None => None,
            Some(term) => Some(match term.split_once('/') {
                Some((v, s)) => (v.to_string(), s.to_string()),
                None => match self.term_fields.as_slice() {
                    [(_, v)] => (v.clone(), term.clone()),
                    _ => return None,
                },
            }),
        };
        let keep = |i: &&Item| {
            q.status.is_none_or(|s| i.status == s)
                && term.as_ref().is_none_or(|(vocab, slug)| {
                    self.term_fields
                        .iter()
                        .filter(|(_, v)| v == vocab)
                        .filter_map(|(f, _)| i.fields.get(f)?.as_array())
                        .flatten()
                        .any(|t| t.get("slug").and_then(Json::as_str) == Some(slug.as_str()))
                })
        };
        let all: Box<dyn Iterator<Item = &Item>> = if reverse {
            Box::new(self.items.iter().rev())
        } else {
            Box::new(self.items.iter())
        };
        Some(
            all.filter(keep)
                .skip(q.offset.max(0) as usize)
                .take(q.limit.map_or(usize::MAX, |l| l.max(0) as usize))
                .collect(),
        )
    }
}

/// The `content` feature's site extension. Cheap to clone.
#[derive(Clone)]
pub struct ContentSite {
    inner: Arc<Inner>,
}

fn param_of(pattern: &str) -> String {
    pattern
        .split('/')
        .find_map(|s| s.strip_prefix('{')?.strip_suffix('}'))
        .unwrap_or("slug")
        .to_string()
}

/// The value cache's budget when none is given: 1/256 of the memory this
/// process may use, between 1 and 64 MiB.
pub fn default_value_cache_bytes() -> usize {
    let mem = engine_core::resources::get().memory;
    (mem / 256).clamp(1 << 20, 64 << 20) as usize
}

fn mj_err(e: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::InvalidOperation, e.to_string())
}

impl ContentSite {
    /// Check the site's pattern routes against the content types.
    pub fn new(store: Store, rt: tokio::runtime::Handle, cfg: &SiteConfig) -> Result<Self> {
        Self::with_value_cache(store, rt, cfg, default_value_cache_bytes())
    }

    /// [`Self::new`] with a byte budget for the cache of items' template
    /// values (0: off).
    pub fn with_value_cache(
        store: Store,
        rt: tokio::runtime::Handle,
        cfg: &SiteConfig,
        value_cache_bytes: usize,
    ) -> Result<Self> {
        let reg = store.registry();
        let mut type_routes = BTreeMap::new();
        let mut term_routes = BTreeMap::new();
        let mut list_routes = BTreeMap::new();
        for (i, rc) in cfg.routes.iter().enumerate() {
            if let Some(list) = &rc.list {
                let td = reg.require(&list.content_type)?;
                if td.singleton {
                    bail!(
                        "route `{}`: `{}` is a singleton and has nothing to list",
                        rc.path,
                        td.name
                    );
                }
                list_routes.insert(
                    i,
                    ListRoute {
                        base: normalize_path(&rc.path),
                        list: list.clone(),
                    },
                );
                continue;
            }
            let pattern = normalize_path(&rc.path);
            let pr = PatternRoute {
                param: param_of(&pattern),
                pattern,
            };
            if let Some(t) = &rc.content_type {
                let td = reg.require(t)?;
                if td.singleton {
                    bail!(
                        "route `{}`: `{t}` is a singleton; give it a plain route and call \
                         `get_singleton(\"{t}\")` in the template",
                        rc.path
                    );
                }
                if type_routes.insert(t.clone(), pr).is_some() {
                    bail!("type `{t}` has two item routes; each item needs one canonical URL");
                }
            } else if let Some(v) = &rc.taxonomy {
                if !reg.is_vocabulary(v) {
                    bail!(
                        "route `{}`: no vocabulary `{v}` (vocabularies are named by `terms` \
                         fields in types/)",
                        rc.path
                    );
                }
                if term_routes.insert(v.clone(), pr).is_some() {
                    bail!("vocabulary `{v}` has two term routes");
                }
            }
        }
        Ok(ContentSite {
            inner: Arc::new(Inner {
                store,
                rt,
                type_routes,
                term_routes,
                list_routes,
                snapshot: RwLock::new(None),
                preload: std::sync::atomic::AtomicBool::new(
                    std::env::var("ENGINE_BUILD_NO_PRELOAD").map_or(true, |v| v.trim() != "1"),
                ),
                values: crate::values::ValueCache::new(value_cache_bytes),
            }),
        })
    }

    /// Load every item of every type into memory for a whole-site render
    /// (`engine build`, a full rebuild), until [`Self::release_preload`].
    /// The template functions then answer from memory. Only for renders of
    /// one consistent moment: publish batches and request pages read the
    /// database as usual.
    pub fn preload(&self) -> Result<()> {
        if !self
            .inner
            .preload
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Ok(());
        }
        let mut types = HashMap::new();
        for t in self.registry().types() {
            let items = self.block_on(self.store().list(
                &t.name,
                &ListQuery {
                    status: None,
                    ..Default::default()
                },
            ))?;
            types.insert(t.name.clone(), TypeSnap::new(t, items));
        }
        *self
            .inner
            .snapshot
            .write()
            .unwrap_or_else(|p| p.into_inner()) = Some(Arc::new(Snapshot {
            types,
            values: Mutex::new(HashMap::new()),
        }));
        Ok(())
    }

    /// Turn the whole-site preload on or off (it is on unless
    /// `ENGINE_BUILD_NO_PRELOAD=1`).
    pub fn set_preload(&self, on: bool) {
        self.inner
            .preload
            .store(on, std::sync::atomic::Ordering::Relaxed);
    }

    /// Back to reading the database for every call.
    pub fn release_preload(&self) {
        *self
            .inner
            .snapshot
            .write()
            .unwrap_or_else(|p| p.into_inner()) = None;
    }

    fn snapshot(&self) -> Option<Arc<Snapshot>> {
        self.inner
            .snapshot
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// `Store::list`, from the snapshot when one is loaded and the query
    /// allows it.
    fn list_any(&self, t: &TypeDef, q: &ListQuery) -> Result<Vec<Item>> {
        if let Some(snap) = self.snapshot()
            && let Some(items) = snap.types.get(&t.name).and_then(|ts| ts.list(q))
        {
            return Ok(items.into_iter().cloned().collect());
        }
        self.block_on(self.store().list(&t.name, q))
    }

    /// `Store::count`, likewise.
    fn count_any(&self, type_name: &str, q: &ListQuery) -> Result<i64> {
        if let Some(snap) = self.snapshot()
            && let Some(items) = snap.types.get(type_name).and_then(|ts| ts.list(q))
        {
            return Ok(items.len() as i64);
        }
        self.block_on(self.store().count(type_name, q))
    }

    /// [`Self::item_value`], made once per item while a snapshot is loaded.
    fn cached_value(&self, snap: Option<&Snapshot>, t: &TypeDef, item: &Item) -> Value {
        let Some(snap) = snap else {
            return self.item_value(t, item);
        };
        let key = (t.name.clone(), item.id);
        if let Some(v) = snap
            .values
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&key)
        {
            return v.clone();
        }
        let v = self.make_value(t, item);
        snap.values
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(key, v.clone());
        v
    }

    fn list_query(list: &ListConfig) -> ListQuery {
        ListQuery {
            status: Some(Status::Published),
            order: list.order.clone().unwrap_or_else(|| "newest".into()),
            term: list.term.clone(),
            ..Default::default()
        }
    }

    /// Number of pages a list route has now (at least 1).
    fn list_pages(&self, lr: &ListRoute) -> Result<usize> {
        let n = self.count_any(&lr.list.content_type, &Self::list_query(&lr.list))?;
        Ok((n.max(0) as usize).div_ceil(lr.list.per_page).max(1))
    }

    /// Page `number` of a list route, if it exists.
    fn list_page(
        &self,
        cfg: &SiteConfig,
        index: usize,
        lr: &ListRoute,
        number: usize,
        total: usize,
    ) -> Result<Option<DynamicPage>> {
        if number == 0 || number > total {
            return Ok(None);
        }
        let rc = &cfg.routes[index];
        let t = self.registry().require(&lr.list.content_type)?.clone();
        let q = ListQuery {
            limit: Some(lr.list.per_page as i64),
            offset: ((number - 1) * lr.list.per_page) as i64,
            ..Self::list_query(&lr.list)
        };
        let snap = self.snapshot();
        let items: Vec<Value> = self
            .list_any(&t, &q)?
            .iter()
            .map(|i| self.cached_value(snap.as_deref(), &t, i))
            .collect();
        let url = |n: usize| page_url(&lr.base, n);
        let urls: Vec<Value> = (1..=total)
            .map(|n| context! { number => n, url => url(n), current => n == number })
            .collect();
        let path = url(number);
        Ok(Some(DynamicPage {
            page: context! {
                title => rc.title.clone().unwrap_or_else(|| t.label.clone()),
                description => rc.description,
                url => path,
                number => number,
            },
            path,
            tags: vec![CacheTag::collection(&t.name)],
            listing: Some(context! {
                items => items.clone(),
                pages => items,
                number => number,
                total => total,
                prev_url => (number > 1).then(|| url(number - 1)),
                next_url => (number < total).then(|| url(number + 1)),
                urls => urls,
            }),
        }))
    }

    pub fn store(&self) -> &Store {
        &self.inner.store
    }

    pub fn registry(&self) -> &Registry {
        self.inner.store.registry()
    }

    fn block_on<F: std::future::Future>(&self, f: F) -> F::Output {
        self.inner.rt.block_on(f)
    }

    /// The URL of a published item, if its type has an item route.
    pub fn item_path(&self, item: &Item) -> Option<String> {
        if item.status != Status::Published {
            return None;
        }
        let r = self.inner.type_routes.get(&item.content_type)?;
        let v = if r.param == "id" {
            item.id.to_string()
        } else {
            item.slug.clone()
        };
        Some(fill_pattern(&r.pattern, &r.param, &v))
    }

    /// The item route a draft would be shown on, and the page as that
    /// route's template would see it (for previews). `None` if its type has
    /// no item route. The path is where it would live once published.
    pub fn preview_page(&self, cfg: &SiteConfig, item: &Item) -> Option<(usize, DynamicPage)> {
        let t = self.registry().get(&item.content_type)?.clone();
        let index = cfg
            .routes
            .iter()
            .position(|r| r.content_type.as_deref() == Some(item.content_type.as_str()))?;
        let r = self.inner.type_routes.get(&item.content_type)?;
        let v = if r.param == "id" {
            item.id.to_string()
        } else {
            item.slug.clone()
        };
        Some((
            index,
            DynamicPage {
                path: fill_pattern(&r.pattern, &r.param, &v),
                page: self.item_value(&t, item),
                tags: vec![CacheTag::item(&item.content_type, item.id.to_string())],
                listing: None,
            },
        ))
    }

    pub fn term_path(&self, term: &Term) -> Option<String> {
        let r = self.inner.term_routes.get(&term.vocabulary)?;
        Some(fill_pattern(&r.pattern, &r.param, &term.slug))
    }

    /// Cache tags invalidated by a change to item `id` of `kind` (a content
    /// type or a vocabulary). Empty for unknown kinds.
    pub fn tags_for_change(&self, kind: &str, id: i64) -> Vec<CacheTag> {
        let reg = self.registry();
        match reg.get(kind) {
            Some(t) if t.singleton => vec![CacheTag::singleton(kind)],
            Some(_) => vec![
                CacheTag::item(kind, id.to_string()),
                CacheTag::collection(kind),
            ],
            None if reg.is_vocabulary(kind) => vec![
                CacheTag::item(kind, id.to_string()),
                CacheTag::collection(kind),
            ],
            // Engine-owned records that feed the change log too (`media`):
            // pages that showed one recorded its item tag.
            None => vec![CacheTag::item(kind, id.to_string())],
        }
    }

    /// Where an item or term lives now (empty if unpublished or gone).
    /// Blocks on the database.
    pub fn current_paths(&self, kind: &str, id: i64) -> Result<Vec<String>> {
        let reg = self.registry();
        if reg.get(kind).is_some() {
            let item = self.block_on(self.store().get(
                kind,
                &Key::Id(id),
                Some(Status::Published),
            ))?;
            let mut paths: Vec<String> =
                item.and_then(|i| self.item_path(&i)).into_iter().collect();
            // Every page of lists of this type: a new item can add a page
            // that no file exists for yet.
            for lr in self.inner.list_routes.values() {
                if lr.list.content_type == kind {
                    for n in 1..=self.list_pages(lr)? {
                        paths.push(page_url(&lr.base, n));
                    }
                }
            }
            return Ok(paths);
        }
        if reg.is_vocabulary(kind) {
            let term = self.block_on(self.store().term(kind, &Key::Id(id)))?;
            return Ok(term.and_then(|t| self.term_path(&t)).into_iter().collect());
        }
        Ok(Vec::new())
    }

    /// An item as templates see it (see [`Self::item_json`] for the
    /// fields). `markdown` fields are sanitised HTML marked *safe*, so a
    /// template prints them with `{{ page.body }}`; every other field is a
    /// plain value that auto-escaping protects. Templates never need
    /// `| safe`, and a field that is not `markdown` can never carry markup
    /// into a page.
    ///
    /// Values are kept (bounded in bytes) and reused for an equal item, so a
    /// page rendered per request does not render the same Markdown again.
    pub fn item_value(&self, t: &TypeDef, item: &Item) -> Value {
        self.inner
            .values
            .get_or_make(item, || self.make_value(t, item))
    }

    /// The bytes the value cache may hold and is estimated to hold now.
    pub fn value_cache_usage(&self) -> (usize, usize) {
        (self.inner.values.budget(), self.inner.values.usage().1)
    }

    fn make_value(&self, t: &TypeDef, item: &Item) -> Value {
        let json = self.item_json(t, item);
        let mut m = engine_core::templates::value_map(&Value::from_serialize(&json));
        for f in &t.fields {
            match f.kind {
                FieldKind::Markdown => {
                    if let Some(html) = json.get(&f.name).and_then(Json::as_str) {
                        m.insert(f.name.clone(), Value::from_safe_string(html.to_string()));
                    }
                }
                FieldKind::Blocks => {
                    let blocks = json.get(&f.name).and_then(Json::as_array);
                    let list: Vec<Value> = blocks
                        .into_iter()
                        .flatten()
                        .map(|b| self.block_template_value(b))
                        .collect();
                    m.insert(f.name.clone(), Value::from(list));
                }
                _ => {}
            }
        }
        Value::from(m)
    }

    /// A sanitised block (from [`Self::item_json`]) with its markdown fields
    /// marked safe.
    fn block_template_value(&self, b: &Json) -> Value {
        let mut m = engine_core::templates::value_map(&Value::from_serialize(b));
        let def = b
            .get("type")
            .and_then(Json::as_str)
            .and_then(|t| self.registry().block(t));
        for f in def.map(|d| d.fields.as_slice()).unwrap_or_default() {
            if f.kind == FieldKind::Markdown
                && let Some(html) = b.get(&f.name).and_then(Json::as_str)
            {
                m.insert(f.name.clone(), Value::from_safe_string(html.to_string()));
            }
        }
        Value::from(m)
    }

    /// An item as sanitised JSON: `markdown` fields rendered and cleaned,
    /// `url` fields only if safe (else null), blocks cleaned the same way
    /// (unknown blocks dropped), terms with their URLs, plus `type`, `id`,
    /// `slug`, `status`, `version`, dates and `url`.
    pub fn item_json(&self, t: &TypeDef, item: &Item) -> Json {
        let mut m = Map::new();
        for f in &t.fields {
            let raw = item.fields.get(&f.name).cloned().unwrap_or(Json::Null);
            let v = match f.kind {
                FieldKind::Markdown => match raw {
                    Json::String(s) => Json::String(sanitize::markdown_to_html(&s)),
                    _ => Json::Null,
                },
                FieldKind::Url => match raw.as_str().and_then(sanitize::safe_url) {
                    Some(u) => Json::String(u.to_string()),
                    None => Json::Null,
                },
                FieldKind::Blocks => self.blocks_value(&raw),
                FieldKind::Terms => Json::Array(
                    raw.as_array()
                        .into_iter()
                        .flatten()
                        .map(|term| {
                            let mut term = term.clone();
                            let path = term["slug"].as_str().and_then(|s| {
                                let r = self.inner.term_routes.get(f.vocabulary.as_deref()?)?;
                                Some(fill_pattern(&r.pattern, &r.param, s))
                            });
                            if let Json::Object(o) = &mut term {
                                o.insert(
                                    "url".into(),
                                    path.map(Json::String).unwrap_or(Json::Null),
                                );
                            }
                            term
                        })
                        .collect(),
                ),
                _ => raw,
            };
            m.insert(f.name.clone(), v);
        }
        let title = t
            .title_field
            .as_ref()
            .and_then(|f| item.fields.get(f))
            .and_then(Json::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| t.label.clone());
        m.entry("title").or_insert(Json::String(title));
        for (k, v) in [
            ("type", Json::String(item.content_type.clone())),
            ("id", Json::from(item.id)),
            ("slug", Json::String(item.slug.clone())),
            ("status", Json::String(item.status.as_str().into())),
            ("version", Json::from(item.version)),
            ("created_at", Json::String(item.created_at.clone())),
            ("updated_at", Json::String(item.updated_at.clone())),
            (
                "published_at",
                item.published_at
                    .clone()
                    .map(Json::String)
                    .unwrap_or(Json::Null),
            ),
            (
                "date",
                item.published_at
                    .as_ref()
                    .map(|d| Json::String(d.chars().take(10).collect()))
                    .unwrap_or(Json::Null),
            ),
            (
                "url",
                self.item_path(item).map(Json::String).unwrap_or(Json::Null),
            ),
        ] {
            m.insert(k.to_string(), v);
        }
        Json::Object(m)
    }

    /// Blocks with sanitised Markdown and URLs; unknown blocks dropped.
    fn blocks_value(&self, raw: &Json) -> Json {
        let reg = self.registry();
        let mut out = Vec::new();
        for b in raw.as_array().into_iter().flatten() {
            let Some(ty) = b.get("type").and_then(Json::as_str) else {
                continue;
            };
            let Some(def) = reg.block(ty) else {
                continue;
            };
            let mut m = Map::new();
            m.insert("type".into(), Json::String(ty.to_string()));
            if let Some(id) = b
                .get(BLOCK_ID)
                .and_then(Json::as_str)
                .filter(|i| valid_block_id(i))
            {
                m.insert(BLOCK_ID.into(), Json::String(id.to_string()));
            }
            for f in &def.fields {
                let raw = b.get(&f.name).cloned().unwrap_or(Json::Null);
                let v = match f.kind {
                    FieldKind::Markdown => raw
                        .as_str()
                        .map(|s| Json::String(sanitize::markdown_to_html(s)))
                        .unwrap_or(Json::Null),
                    FieldKind::Url => raw
                        .as_str()
                        .and_then(sanitize::safe_url)
                        .map(|u| Json::String(u.to_string()))
                        .unwrap_or(Json::Null),
                    _ => raw,
                };
                m.insert(f.name.clone(), v);
            }
            out.push(Json::Object(m));
        }
        Json::Array(out)
    }

    pub fn term_value(&self, term: &Term) -> Json {
        serde_json::json!({
            "type": "term",
            "id": term.id,
            "vocabulary": term.vocabulary,
            "slug": term.slug,
            "name": term.name,
            "title": term.name,
            "url": self.term_path(term),
        })
    }

    fn items_fn(&self, type_name: &str, kwargs: &Kwargs) -> Result<Value, Error> {
        let limit: Option<i64> = kwargs.get("limit")?;
        let offset: Option<i64> = kwargs.get("offset")?;
        let order: Option<String> = kwargs.get("order")?;
        let term: Option<String> = kwargs.get("term")?;
        kwargs.assert_all_used()?;
        let t = self.registry().require(type_name).map_err(mj_err)?.clone();
        tags::record(if t.singleton {
            CacheTag::singleton(&t.name)
        } else {
            CacheTag::collection(&t.name)
        });
        let q = ListQuery {
            status: Some(Status::Published),
            limit,
            offset: offset.unwrap_or(0),
            order: order.unwrap_or_else(|| "newest".into()),
            term,
            search: None,
        };
        let items = self.list_any(&t, &q).map_err(mj_err)?;
        let snap = self.snapshot();
        Ok(Value::from(
            items
                .iter()
                .map(|i| self.cached_value(snap.as_deref(), &t, i))
                .collect::<Vec<_>>(),
        ))
    }

    fn item_fn(&self, type_name: &str, key: &Value) -> Result<Value, Error> {
        let t = self.registry().require(type_name).map_err(mj_err)?.clone();
        let key = if let Some(i) = key.as_i64() {
            Key::Id(i)
        } else if let Some(s) = key.as_str() {
            Key::parse(s)
        } else if key.is_undefined() || key.is_none() {
            return Ok(Value::UNDEFINED);
        } else {
            return Err(mj_err("get_item(type, key): key must be an id or a slug"));
        };
        let snap = self.snapshot();
        let found = match snap.as_deref().and_then(|s| s.types.get(&t.name)) {
            Some(ts) => ts
                .get(&key)
                .filter(|i| i.status == Status::Published)
                .cloned(),
            None => self
                .block_on(self.store().get(&t.name, &key, Some(Status::Published)))
                .map_err(mj_err)?,
        };
        match found {
            Some(i) => {
                tags::record(CacheTag::item(&t.name, i.id.to_string()));
                Ok(self.cached_value(snap.as_deref(), &t, &i))
            }
            None => {
                // Publishing a matching item changes the collection.
                tags::record(CacheTag::collection(&t.name));
                Ok(Value::UNDEFINED)
            }
        }
    }

    fn singleton_fn(&self, type_name: &str) -> Result<Value, Error> {
        let t = self.registry().require(type_name).map_err(mj_err)?.clone();
        if !t.singleton {
            return Err(mj_err(format!(
                "get_singleton(\"{type_name}\"): `{type_name}` is not a singleton type"
            )));
        }
        tags::record(CacheTag::singleton(&t.name));
        let snap = self.snapshot();
        let found = match snap.as_deref().and_then(|s| s.types.get(&t.name)) {
            Some(ts) => ts.items.first().cloned(),
            None => self
                .block_on(self.store().singleton(&t.name))
                .map_err(mj_err)?,
        };
        Ok(match found {
            Some(i) => self.cached_value(snap.as_deref(), &t, &i),
            None => Value::UNDEFINED,
        })
    }

    /// `term_counts(vocabulary)`: terms with how many published items carry
    /// each, and a `weight` from 1 to 5 (for tag clouds).
    fn term_counts_fn(&self, vocabulary: &str) -> Result<Value, Error> {
        tags::record(CacheTag::collection(vocabulary));
        let counts = self
            .block_on(self.store().term_counts(vocabulary))
            .map_err(mj_err)?;
        // Items of every type using the vocabulary change the counts.
        for t in self.registry().types() {
            if t.fields
                .iter()
                .any(|f| f.vocabulary.as_deref() == Some(vocabulary))
            {
                tags::record(CacheTag::collection(&t.name));
            }
        }
        let max = counts.iter().map(|(_, n)| *n).max().unwrap_or(1).max(1);
        let list: Vec<Value> = counts
            .iter()
            .map(|(t, n)| {
                let mut v =
                    engine_core::templates::value_map(&Value::from_serialize(self.term_value(t)));
                v.insert("count".into(), Value::from(*n));
                // 1..=5 on a square-root scale, so one popular term does
                // not flatten the rest.
                let w = 1 + ((((*n as f64) / (max as f64)).sqrt() * 4.0).round() as i64);
                v.insert("weight".into(), Value::from(w.clamp(1, 5)));
                Value::from(v)
            })
            .collect();
        Ok(Value::from(list))
    }

    /// `related_items(type, id, limit=3)`: published items of the same type
    /// that share the most terms with item `id`.
    fn related_fn(&self, type_name: &str, id: &Value, kwargs: &Kwargs) -> Result<Value, Error> {
        let limit: Option<i64> = kwargs.get("limit")?;
        kwargs.assert_all_used()?;
        let t = self.registry().require(type_name).map_err(mj_err)?.clone();
        let Ok(id) = i64::try_from(id.clone()) else {
            return Ok(Value::from(Vec::<Value>::new()));
        };
        tags::record(CacheTag::collection(&t.name));
        let snap = self.snapshot();
        if let Some(ts) = snap.as_deref().and_then(|s| s.types.get(&t.name)) {
            return Ok(Value::from(
                ts.related(id, limit.unwrap_or(3))
                    .into_iter()
                    .map(|i| self.cached_value(snap.as_deref(), &t, i))
                    .collect::<Vec<_>>(),
            ));
        }
        let items = self
            .block_on(self.store().related(&t.name, id, limit.unwrap_or(3)))
            .map_err(mj_err)?;
        Ok(Value::from(
            items
                .iter()
                .map(|i| self.item_value(&t, i))
                .collect::<Vec<_>>(),
        ))
    }

    fn terms_fn(&self, vocabulary: &str) -> Result<Value, Error> {
        tags::record(CacheTag::collection(vocabulary));
        let terms = self
            .block_on(self.store().terms(vocabulary))
            .map_err(mj_err)?;
        Ok(Value::from_serialize(
            terms.iter().map(|t| self.term_value(t)).collect::<Vec<_>>(),
        ))
    }
}

/// `render_block(b)`: `blocks/<type>.html` with `block`; or, when the site
/// has no such template and the block type names a component
/// (`component = "quote"` in its type file), that component with the
/// block's fields as arguments (and `block` too). A block with `token`
/// fields set also gets the token stylesheet (`token_assets()`).
///
/// In the visual editor's preview (`block_markers` is true in the render
/// context, never on a published page) the output is wrapped in
/// `<!--wrought-block:ID-->` ... `<!--/wrought-block:ID-->` comments, so
/// the editor can tell which part of the page each block made. Ids are
/// checked ([`valid_block_id`]), so a comment cannot be closed early.
fn block_fn(
    state: &State,
    b: Value,
    components: &BTreeMap<String, String>,
    token_blocks: &BTreeMap<String, Vec<String>>,
) -> Result<Value, Error> {
    let ty = b
        .get_attr("type")
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .filter(|t| valid_name(t))
        .ok_or_else(|| mj_err("render_block(b): b needs a `type`"))?;
    let mut ctx = BTreeMap::<String, Value>::new();
    for key in ["site", "page", "ui", "engine", "route"] {
        if let Some(v) = state.lookup(key) {
            ctx.insert(key.to_string(), v);
        }
    }
    let tmpl = match (
        state.env().get_template(&format!("blocks/{ty}.html")),
        components.get(&ty),
    ) {
        (Ok(t), _) => t,
        (Err(_), Some(c)) => {
            ctx.extend(engine_core::templates::value_map(&b));
            ctx.remove("type");
            // The block's id is the editor's, not the component's `id`
            // argument (an anchor).
            ctx.remove(BLOCK_ID);
            state.env().get_template(&format!("components/{c}.html"))?
        }
        (Err(e), None) => return Err(e),
    };
    let id = b
        .get_attr(BLOCK_ID)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .filter(|i| valid_block_id(i));
    ctx.insert("block".into(), b.clone());
    let mut html = String::new();
    let uses_tokens = token_blocks.get(&ty).is_some_and(|fields| {
        fields.iter().any(|f| {
            b.get_attr(f)
                .ok()
                .is_some_and(|v| !v.is_none() && !v.is_undefined())
        })
    });
    if uses_tokens && let Some(f) = state.lookup("token_assets") {
        html.push_str(&f.call(state, &[])?.to_string());
    }
    html.push_str(&tmpl.render(Value::from(ctx))?);
    let markers = state.lookup("block_markers").is_some_and(|v| v.is_true());
    if let (true, Some(id)) = (markers, id) {
        html = format!("<!--wrought-block:{id}-->{html}<!--/wrought-block:{id}-->");
    }
    Ok(Value::from_safe_string(html))
}

/// The Listings components (`content_list`, `filtered_list`, `tag_cloud`,
/// `related_items`).
pub static LISTINGS: include_dir::Dir<'static> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/site-templates");

impl SiteExtension for ContentSite {
    fn extend_env(&self, env: &mut minijinja::Environment<'static>) {
        let s = self.clone();
        env.add_function("list_items", move |type_name: String, kwargs: Kwargs| {
            s.items_fn(&type_name, &kwargs)
        });
        let s = self.clone();
        env.add_function("get_item", move |type_name: String, key: Value| {
            s.item_fn(&type_name, &key)
        });
        let s = self.clone();
        env.add_function("get_singleton", move |type_name: String| {
            s.singleton_fn(&type_name)
        });
        let s = self.clone();
        env.add_function("list_terms", move |vocabulary: String| {
            s.terms_fn(&vocabulary)
        });
        let s = self.clone();
        env.add_function("term_counts", move |vocabulary: String| {
            s.term_counts_fn(&vocabulary)
        });
        let s = self.clone();
        env.add_function(
            "related_items",
            move |type_name: String, id: Value, kwargs: Kwargs| {
                s.related_fn(&type_name, &id, &kwargs)
            },
        );
        let components: BTreeMap<String, String> = self
            .registry()
            .blocks()
            .filter_map(|b| Some((b.name.clone(), b.component.clone()?)))
            .collect();
        let token_blocks: BTreeMap<String, Vec<String>> = self
            .registry()
            .blocks()
            .map(|b| {
                let fields = b
                    .fields
                    .iter()
                    .filter(|f| f.kind == FieldKind::Token)
                    .map(|f| f.name.clone())
                    .collect::<Vec<_>>();
                (b.name.clone(), fields)
            })
            .filter(|(_, f)| !f.is_empty())
            .collect();
        env.add_function("render_block", move |state: &State, b: Value| {
            block_fn(state, b, &components, &token_blocks)
        });
    }

    fn template_dirs(&self) -> Vec<&'static include_dir::Dir<'static>> {
        vec![&LISTINGS]
    }

    fn begin_bulk(&self) -> Result<()> {
        self.preload()
    }

    fn end_bulk(&self) {
        self.release_preload();
    }

    fn handles_dynamic_routes(&self) -> bool {
        true
    }

    fn expand(&self, cfg: &SiteConfig, index: usize) -> Result<Vec<DynamicPage>> {
        let rc = &cfg.routes[index];
        if let Some(lr) = self.inner.list_routes.get(&index) {
            let total = self.list_pages(lr)?;
            let mut out = Vec::new();
            for n in 1..=total {
                out.extend(self.list_page(cfg, index, lr, n, total)?);
            }
            return Ok(out);
        }
        if let Some(type_name) = &rc.content_type {
            let t = self.registry().require(type_name)?.clone();
            let items = self.block_on(self.store().list(
                type_name,
                &ListQuery {
                    status: Some(Status::Published),
                    ..Default::default()
                },
            ))?;
            return Ok(items
                .iter()
                .filter_map(|i| {
                    Some(DynamicPage {
                        path: self.item_path(i)?,
                        page: self.item_value(&t, i),
                        tags: vec![CacheTag::item(type_name, i.id.to_string())],
                        listing: None,
                    })
                })
                .collect());
        }
        if let Some(vocab) = &rc.taxonomy {
            let terms = self.block_on(self.store().terms(vocab))?;
            return Ok(terms
                .iter()
                .filter_map(|t| {
                    Some(DynamicPage {
                        path: self.term_path(t)?,
                        page: Value::from_serialize(self.term_value(t)),
                        tags: vec![CacheTag::item(vocab, t.id.to_string())],
                        listing: None,
                    })
                })
                .collect());
        }
        Ok(Vec::new())
    }

    fn resolve(
        &self,
        cfg: &SiteConfig,
        index: usize,
        params: &BTreeMap<String, String>,
    ) -> Result<Option<DynamicPage>> {
        let rc = &cfg.routes[index];
        if let Some(lr) = self.inner.list_routes.get(&index) {
            let Some(n) = params.get("page").and_then(|p| p.parse::<usize>().ok()) else {
                return Ok(None);
            };
            let total = self.list_pages(lr)?;
            return self.list_page(cfg, index, lr, n, total);
        }
        let key = match (params.get("slug"), params.get("id"), params.get("term")) {
            (Some(s), _, _) | (_, _, Some(s)) if slug::is_valid(s) => Key::Slug(s.clone()),
            (_, Some(i), _) => match i.parse::<i64>() {
                Ok(i) if i > 0 => Key::Id(i),
                _ => return Ok(None),
            },
            _ => return Ok(None),
        };
        if let Some(type_name) = &rc.content_type {
            let t = self.registry().require(type_name)?.clone();
            // A whole-site render (a build, a large publish batch) answers
            // from the snapshot: no round trip per page.
            if let Some(snap) = self.snapshot()
                && let Some(ts) = snap.types.get(type_name)
            {
                let item = ts.get(&key).filter(|i| i.status == Status::Published);
                return Ok(item.and_then(|i| {
                    Some(DynamicPage {
                        path: self.item_path(i)?,
                        page: self.cached_value(Some(&snap), &t, i),
                        tags: vec![CacheTag::item(type_name, i.id.to_string())],
                        listing: None,
                    })
                }));
            }
            let item = self.block_on(self.store().get(type_name, &key, Some(Status::Published)))?;
            return Ok(item.and_then(|i| {
                Some(DynamicPage {
                    path: self.item_path(&i)?,
                    page: self.item_value(&t, &i),
                    tags: vec![CacheTag::item(type_name, i.id.to_string())],
                    listing: None,
                })
            }));
        }
        if let Some(vocab) = &rc.taxonomy {
            let term = self.block_on(self.store().term(vocab, &key))?;
            return Ok(term.and_then(|t| {
                Some(DynamicPage {
                    path: self.term_path(&t)?,
                    page: Value::from_serialize(self.term_value(&t)),
                    tags: vec![CacheTag::item(vocab, t.id.to_string())],
                    listing: None,
                })
            }));
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(site: Option<&std::path::Path>) -> minijinja::Environment<'static> {
        let mut env = engine_core::templates::environment(
            site.map(std::path::Path::to_path_buf),
            Arc::default(),
            String::new(),
            vec![&LISTINGS],
            Arc::new(engine_core::components::Catalogue::load(&[&LISTINGS], site).unwrap()),
        );
        let components: BTreeMap<String, String> = [
            ("pull".to_string(), "quote".to_string()),
            ("txt".to_string(), "rich_text".to_string()),
        ]
        .into();
        let token_blocks: BTreeMap<String, Vec<String>> =
            [("pull".to_string(), vec!["background".to_string()])].into();
        env.add_function("render_block", move |state: &State, b: Value| {
            block_fn(state, b, &components, &token_blocks)
        });
        env
    }

    #[test]
    fn blocks_render_through_their_component_unless_the_site_has_a_template() {
        let b = context! { type => "pull", text => "<b>hi</b>", cite => "me" };
        let out = env(None)
            .render_str("{{ render_block(b) }}", context! { b => b.clone() })
            .unwrap();
        assert!(
            out.contains("<blockquote><p>&lt;b&gt;hi&lt;/b&gt;</p></blockquote>"),
            "{out}"
        );
        assert!(out.contains("<figcaption>me</figcaption>"), "{out}");
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("blocks")).unwrap();
        std::fs::write(
            d.path().join("blocks/pull.html"),
            "<p>own {{ block.text }}</p>",
        )
        .unwrap();
        let out = env(Some(d.path()))
            .render_str("{{ render_block(b) }}", context! { b => b })
            .unwrap();
        assert_eq!(out, "<p>own &lt;b&gt;hi&lt;/b&gt;</p>");
        let missing = env(None).render_str(
            "{{ render_block(b) }}",
            context! { b => context! { type => "other" } },
        );
        assert!(missing.is_err());
    }

    #[test]
    fn block_ids_markers_and_token_styles() {
        // The id is not passed to the component as its `id` argument.
        let b = context! { type => "pull", id => "k1", text => "hi" };
        let plain = env(None)
            .render_str("{{ render_block(b) }}", context! { b => b.clone() })
            .unwrap();
        assert!(
            !plain.contains("k1") && !plain.contains("wrought-block"),
            "{plain}"
        );
        assert!(!plain.contains("tokens-"), "no token set: {plain}");
        // Markers only in the editor's preview.
        let marked = env(None)
            .render_str(
                "{{ render_block(b) }}",
                context! { b => b.clone(), block_markers => true },
            )
            .unwrap();
        assert!(
            marked.starts_with("<!--wrought-block:k1--><figure>"),
            "{marked}"
        );
        assert!(marked.ends_with("<!--/wrought-block:k1-->"), "{marked}");
        // A bad id gets no marker (it could close the comment).
        let bad = context! { type => "pull", id => "x-->", text => "hi" };
        let out = env(None)
            .render_str(
                "{{ render_block(b) }}",
                context! { b => bad, block_markers => true },
            )
            .unwrap();
        assert!(!out.contains("wrought-block"), "{out}");
        // A markdown field reaches a component's `markdown` argument as the
        // engine's sanitised HTML, which the component shows as HTML.
        let b = context! { type => "txt", markdown => Value::from_safe_string("<p>a <strong>b</strong></p>".into()) };
        let out = env(None)
            .render_str("{{ render_block(b) }}", context! { b => b })
            .unwrap();
        assert!(out.contains("<p>a <strong>b</strong></p>"), "{out}");
        // A token field set: the token stylesheet comes with the block.
        let b = context! { type => "pull", text => "hi", background => "surface" };
        let out = env(None)
            .render_str("{{ render_block(b) }}", context! { b => b })
            .unwrap();
        assert!(
            out.starts_with("<link rel=\"stylesheet\" href=\"/_engine/components/tokens-"),
            "{out}"
        );
    }
}
