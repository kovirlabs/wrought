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
//! (insert with `| safe`), URL fields hold only safe links (or nothing), and
//! everything else is plain data that templates auto-escape.
//!
//! The extension's methods block on database queries, so they must run on a
//! thread that may block (the CLI's main thread, or `spawn_blocking`), never
//! directly on an async worker.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{Result, bail};
use engine_core::builder::{DynamicPage, SiteExtension};
use engine_core::config::SiteConfig;
use engine_core::routes::{fill_pattern, normalize_path};
use engine_core::tags::{self, CacheTag};
use minijinja::value::{Kwargs, Value};
use minijinja::{Error, ErrorKind, State};
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

struct Inner {
    store: Store,
    rt: tokio::runtime::Handle,
    type_routes: BTreeMap<String, PatternRoute>,
    term_routes: BTreeMap<String, PatternRoute>,
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
        for rc in &cfg.routes {
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
            }),
        })
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
            return Ok(item.and_then(|i| self.item_path(&i)).into_iter().collect());
        }
        if reg.is_vocabulary(kind) {
            let term = self.block_on(self.store().term(kind, &Key::Id(id)))?;
            return Ok(term.and_then(|t| self.term_path(&t)).into_iter().collect());
        }
        Ok(Vec::new())
    }

    /// An item as templates see it.
    pub fn item_value(&self, t: &TypeDef, item: &Item) -> Json {
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
        };
        let items = self
            .block_on(self.store().list(&t.name, &q))
            .map_err(mj_err)?;
        Ok(Value::from_serialize(
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
                Ok(Value::from_serialize(self.item_value(&t, &i)))
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
            Some(i) => Value::from_serialize(self.item_value(&t, &i)),
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

    fn expand(&self, cfg: &SiteConfig, index: usize) -> Result<Vec<DynamicPage>> {
        let rc = &cfg.routes[index];
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
                        page: self.term_value(t),
                        tags: vec![CacheTag::item(vocab, t.id.to_string())],
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
                })
            }));
        }
        if let Some(vocab) = &rc.taxonomy {
            let term = self.block_on(self.store().term(vocab, &key))?;
            return Ok(term.and_then(|t| {
                Some(DynamicPage {
                    path: self.term_path(&t)?,
                    page: self.term_value(&t),
                    tags: vec![CacheTag::item(vocab, t.id.to_string())],
                })
            }));
        }
        Ok(None)
    }
}
