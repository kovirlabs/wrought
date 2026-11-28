//! The `payments` feature in the engine: `[payments]`, the product
//! catalogue (a file, or a content type when `content` is compiled in),
//! the Commerce template extension, and the shop's routes in `serve`.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Result, bail};
use engine_core::config::SiteConfig;
use engine_db::Db;
use engine_payments::{Catalogue, FileCatalogue, PaymentsConfig};

/// The shop's settings and catalogue, made once per command.
#[derive(Clone)]
pub struct Shop {
    pub config: PaymentsConfig,
    pub catalogue: Arc<dyn Catalogue>,
}

/// `[payments]` of a site, with its catalogue. `None` without the section.
pub fn shop(site: &Path, cfg: &SiteConfig, db: Option<&Db>) -> Result<Option<Shop>> {
    let Some(pc) = PaymentsConfig::from_site(cfg.payments.as_ref())? else {
        return Ok(None);
    };
    let Some(db) = db else {
        bail!("[payments] needs the site's database: set ENGINE_DATABASE_URL");
    };
    let catalogue: Arc<dyn Catalogue> = match (&pc.product_type, pc.products_file()) {
        (Some(t), _) => content_catalogue(site, cfg, db, t, &pc.currency)?,
        (None, Some(f)) => Arc::new(FileCatalogue::load(site, &f, &pc.currency)?),
        (None, None) => unreachable!("one of the two is always set"),
    };
    Ok(Some(Shop {
        config: pc,
        catalogue,
    }))
}

#[cfg(not(feature = "content"))]
fn content_catalogue(
    _site: &Path,
    _cfg: &SiteConfig,
    _db: &Db,
    t: &str,
    _currency: &str,
) -> Result<Arc<dyn Catalogue>> {
    bail!(
        "[payments] product_type = \"{t}\" needs the `content` feature, which is not in this \
         build; use products_file (products.toml) instead"
    )
}

#[cfg(feature = "content")]
fn content_catalogue(
    site: &Path,
    cfg: &SiteConfig,
    db: &Db,
    t: &str,
    currency: &str,
) -> Result<Arc<dyn Catalogue>> {
    let reg = engine_content::Registry::load(site)?;
    let ty = reg
        .get(t)
        .ok_or_else(|| anyhow::anyhow!("[payments] product_type: no content type `{t}`"))?;
    match ty.field("price") {
        Some(f) if f.kind == engine_content::types::FieldKind::Integer => {}
        _ => bail!(
            "[payments] product_type `{t}` needs an integer field `price` (the price in minor \
             units, e.g. pence)"
        ),
    }
    let url = cfg
        .routes
        .iter()
        .find(|r| r.content_type.as_deref() == Some(t))
        .map(|r| r.path.clone());
    Ok(Arc::new(content::ContentCatalogue {
        store: engine_content::Store::new(db.clone(), Arc::new(reg.clone())),
        type_name: t.to_string(),
        title_field: ty.title_field.clone(),
        currency: currency.to_string(),
        url,
    }))
}

#[cfg(feature = "content")]
mod content {
    use anyhow::Result;
    use engine_content::store::{Item, Key, ListQuery, Status};
    use engine_core::tags::{self, CacheTag};
    use engine_payments::Product;
    use engine_payments::catalogue::{Catalogue, valid_id};
    use engine_payments::provider::BoxFuture;

    /// Products from a content type: published items only; `price`
    /// (integer, minor units), the type's title field as the name, and
    /// optional `summary`, `image`, `image_alt` and `available` fields.
    pub struct ContentCatalogue {
        pub store: engine_content::Store,
        pub type_name: String,
        pub title_field: Option<String>,
        pub currency: String,
        /// The type's route pattern (`/shop/{slug}/`), for product links.
        pub url: Option<String>,
    }

    impl ContentCatalogue {
        fn product(&self, i: &Item) -> Option<Product> {
            if !valid_id(&i.slug) {
                return None;
            }
            let text = |k: &str| {
                i.fields
                    .get(k)
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            };
            let price = i.fields.get("price").and_then(|v| v.as_i64());
            let name = self
                .title_field
                .as_deref()
                .and_then(text)
                .unwrap_or_else(|| i.slug.clone());
            Some(Product {
                id: i.slug.clone(),
                name,
                summary: text("summary"),
                price: price.unwrap_or(0),
                currency: self.currency.clone(),
                url: self.url.as_ref().map(|u| {
                    u.replace("{slug}", &i.slug)
                        .replace("{id}", &i.id.to_string())
                }),
                image: text("image"),
                image_alt: text("image_alt"),
                // No price, a negative one, or switched off: not for sale.
                available: price.is_some_and(|p| p >= 0)
                    && i.fields.get("available").and_then(|v| v.as_bool()) != Some(false),
            })
        }
    }

    impl Catalogue for ContentCatalogue {
        fn products(&self) -> BoxFuture<'_, Result<Vec<Product>>> {
            Box::pin(async move {
                // Pages listing products follow product edits.
                tags::record(CacheTag::collection(self.type_name.clone()));
                let items = self
                    .store
                    .list(
                        &self.type_name,
                        &ListQuery {
                            status: Some(Status::Published),
                            order: "title".into(),
                            limit: Some(500),
                            ..Default::default()
                        },
                    )
                    .await?;
                Ok(items.iter().filter_map(|i| self.product(i)).collect())
            })
        }

        fn product<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<Product>>> {
            Box::pin(async move {
                if !valid_id(id) || id.bytes().all(|b| b.is_ascii_digit()) {
                    return Ok(None);
                }
                let item = self
                    .store
                    .get(
                        &self.type_name,
                        &Key::Slug(id.to_string()),
                        Some(Status::Published),
                    )
                    .await?;
                if let Some(i) = &item {
                    tags::record(CacheTag::item(self.type_name.clone(), i.id.to_string()));
                }
                Ok(item.as_ref().and_then(|i| self.product(i)))
            })
        }
    }
}
