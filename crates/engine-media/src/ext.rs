//! The `media()` template function.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use engine_core::builder::SiteExtension;
use engine_core::tags::{self, CacheTag};
use minijinja::value::Value;
use minijinja::{Error, ErrorKind};
use serde::Serialize;

use crate::{Media, MediaStore};

/// What templates get from `media(id)`.
#[derive(Debug, Serialize)]
pub struct ImageData {
    pub id: i64,
    pub url: String,
    pub srcset: Option<String>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub alt: String,
    pub mime: String,
    pub is_image: bool,
    pub name: String,
    /// `[{width, url}]`, smallest first.
    pub variants: Vec<serde_json::Value>,
}

impl ImageData {
    /// `None` for private files.
    pub fn of(m: &Media) -> Option<ImageData> {
        Some(ImageData {
            id: m.id,
            url: m.url()?,
            srcset: m.srcset(),
            width: m.width,
            height: m.height,
            alt: m.alt.clone(),
            mime: m.mime.clone(),
            is_image: m.is_image(),
            name: m.file_name(),
            variants: m
                .variants
                .iter()
                .filter_map(|w| Some(serde_json::json!({ "width": w, "url": m.variant_url(*w)? })))
                .collect(),
        })
    }
}

/// Adds `media(id)` to templates: the public file with its `srcset`, or
/// none (missing or private). Pages record the tag `i:media:<id>`, so a
/// changed description or a deleted file re-renders them.
#[derive(Clone)]
pub struct MediaExtension {
    store: MediaStore,
    rt: tokio::runtime::Handle,
    /// During a whole-site render: each file looked up once.
    memo: Arc<Mutex<Option<HashMap<i64, Value>>>>,
}

impl MediaExtension {
    pub fn new(store: MediaStore, rt: tokio::runtime::Handle) -> Self {
        MediaExtension {
            store,
            rt,
            memo: Arc::default(),
        }
    }

    fn lookup(&self, id: &Value) -> Result<Value, Error> {
        if id.is_undefined() || id.is_none() {
            return Ok(Value::from(()));
        }
        let id: i64 = match i64::try_from(id.clone()) {
            Ok(i) => i,
            Err(_) => match id.as_str().and_then(|s| s.trim().parse().ok()) {
                Some(i) => i,
                None => {
                    return Err(Error::new(
                        ErrorKind::InvalidOperation,
                        "media(id): id must be a number",
                    ));
                }
            },
        };
        tags::record(CacheTag::item("media", id.to_string()));
        if let Some(v) = self
            .memo
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .and_then(|m| m.get(&id))
        {
            return Ok(v.clone());
        }
        let m = self
            .rt
            .block_on(self.store.get(id))
            .map_err(|e| engine_core::templates::fn_error_in(&format!("media({id})"), e))?;
        let v = match m.as_ref().and_then(ImageData::of) {
            Some(d) => Value::from_serialize(&d),
            None => Value::from(()),
        };
        if let Some(memo) = self.memo.lock().unwrap_or_else(|p| p.into_inner()).as_mut() {
            memo.insert(id, v.clone());
        }
        Ok(v)
    }
}

impl SiteExtension for MediaExtension {
    fn extend_env(&self, env: &mut minijinja::Environment<'static>) {
        let me = Arc::new(self.clone());
        env.add_function("media", move |id: Value| me.lookup(&id));
    }

    fn begin_bulk(&self) -> anyhow::Result<()> {
        *self.memo.lock().unwrap_or_else(|p| p.into_inner()) = Some(HashMap::new());
        Ok(())
    }

    fn end_bulk(&self) {
        *self.memo.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }
}
