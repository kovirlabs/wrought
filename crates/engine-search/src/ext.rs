//! `search()` and the search components.

use std::collections::BTreeMap;
use std::sync::Arc;

use engine_core::builder::SiteExtension;
use include_dir::{Dir, include_dir};
use minijinja::value::{Kwargs, Value};
use minijinja::{Error, ErrorKind};

use crate::{Results, SearchIndex};

/// `search_box` and `search_results`.
pub static COMPONENTS: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/site-templates");

/// Adds `search(q, type=none, limit=10, page=1)` (results with escaped,
/// highlighted snippets) and `search_path()` (where the search page is).
#[derive(Clone)]
pub struct SearchExtension {
    index: SearchIndex,
    rt: tokio::runtime::Handle,
}

impl SearchExtension {
    pub fn new(index: SearchIndex, rt: tokio::runtime::Handle) -> Self {
        SearchExtension { index, rt }
    }

    fn search(&self, q: Value, kwargs: Kwargs) -> Result<Value, Error> {
        let ty: Option<String> = kwargs.get("type")?;
        let limit: Option<i64> = kwargs.get("limit")?;
        let page: Option<Value> = kwargs.get("page")?;
        kwargs.assert_all_used()?;
        let q = q.as_str().map(str::to_string).unwrap_or_default();
        // `page` usually comes from the query string, as text.
        let page = page
            .and_then(|p| {
                i64::try_from(p.clone())
                    .ok()
                    .or_else(|| p.as_str()?.parse().ok())
            })
            .unwrap_or(1);
        let r: Results = self
            .rt
            .block_on(
                self.index
                    .search(&q, ty.as_deref(), limit.unwrap_or(10), page),
            )
            .map_err(|e| Error::new(ErrorKind::InvalidOperation, format!("search(): {e:#}")))?;
        let hits: Vec<Value> = r
            .hits
            .iter()
            .map(|h| {
                let mut m = BTreeMap::new();
                m.insert("type", Value::from(h.content_type.clone()));
                m.insert("id", Value::from(h.id));
                m.insert("title", Value::from(h.title.clone()));
                m.insert("url", Value::from(h.url.clone()));
                m.insert("published_at", Value::from(h.published_at.clone()));
                // Escaped by `highlight`, with <mark> around the matches.
                m.insert("snippet", Value::from_safe_string(h.snippet.clone()));
                m.insert("rank", Value::from(h.rank));
                Value::from(m)
            })
            .collect();
        let mut out = BTreeMap::new();
        out.insert("query", Value::from(r.query));
        out.insert("hits", Value::from(hits));
        out.insert("total", Value::from(r.total));
        out.insert("page", Value::from(r.page));
        out.insert("pages", Value::from(r.pages));
        Ok(Value::from(out))
    }
}

impl SiteExtension for SearchExtension {
    fn extend_env(&self, env: &mut minijinja::Environment<'static>) {
        let me = Arc::new(self.clone());
        env.add_function("search", move |q: Value, kwargs: Kwargs| {
            me.search(q, kwargs)
        });
        let path = self.index.config().path.clone();
        env.add_function("search_path", move || path.clone());
    }

    fn template_dirs(&self) -> Vec<&'static Dir<'static>> {
        vec![&COMPONENTS]
    }
}
