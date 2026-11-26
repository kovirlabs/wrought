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

use std::collections::BTreeMap;
use std::sync::Arc;

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
use crate::types::{FieldKind, Registry, TypeDef, valid_name};

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

fn mj_err(e: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::InvalidOperation, e.to_string())
}

impl ContentSite {
    /// Check the site's pattern routes against the content types.
    pub fn new(store: Store, rt: tokio::runtime::Handle, cfg: &SiteConfig) -> Result<Self> {
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
            }),
        })
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
        let n = self.block_on(
            self.store()
                .count(&lr.list.content_type, &Self::list_query(&lr.list)),
        )?;
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
        let items: Vec<Value> = self
            .block_on(self.store().list(&t.name, &q))?
            .iter()
            .map(|i| self.item_value(&t, i))
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
            None => Vec::new(),
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
    pub fn item_value(&self, t: &TypeDef, item: &Item) -> Value {
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
        let items = self
            .block_on(self.store().list(&t.name, &q))
            .map_err(mj_err)?;
        Ok(Value::from(
            items
                .iter()
                .map(|i| self.item_value(&t, i))
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
        let found = self
            .block_on(self.store().get(&t.name, &key, Some(Status::Published)))
            .map_err(mj_err)?;
        match found {
            Some(i) => {
                tags::record(CacheTag::item(&t.name, i.id.to_string()));
                Ok(self.item_value(&t, &i))
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
        let found = self
            .block_on(self.store().singleton(&t.name))
            .map_err(mj_err)?;
        Ok(match found {
            Some(i) => self.item_value(&t, &i),
            None => Value::UNDEFINED,
        })
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

fn block_fn(state: &State, b: Value) -> Result<Value, Error> {
    let ty = b
        .get_attr("type")
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .filter(|t| valid_name(t))
        .ok_or_else(|| mj_err("render_block(b): b needs a `type`"))?;
    let tmpl = state.env().get_template(&format!("blocks/{ty}.html"))?;
    let mut ctx = BTreeMap::<String, Value>::new();
    for key in ["site", "page", "ui", "engine", "route"] {
        if let Some(v) = state.lookup(key) {
            ctx.insert(key.to_string(), v);
        }
    }
    ctx.insert("block".into(), b);
    Ok(Value::from_safe_string(tmpl.render(Value::from(ctx))?))
}

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
        env.add_function("render_block", block_fn);
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
