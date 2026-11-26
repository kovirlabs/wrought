//! Where products and their prices come from. Prices are always read
//! from here on the server; a price a browser sends is never used.
//!
//! Two sources:
//!
//! * [`FileCatalogue`]: `products.toml` in the site folder (or the file
//!   `[payments] products_file` names), for shops without the `content`
//!   feature;
//! * a content type (`[payments] product_type`), when `content` is compiled
//!   in: products are then edited in the admin like any content, and only
//!   published items are for sale. The CLI provides that adapter, so this
//!   crate does not depend on `engine-content`.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::provider::BoxFuture;

/// A product as the shop sees it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Product {
    /// What the cart stores: the slug (content) or `id` (file).
    pub id: String,
    pub name: String,
    pub summary: Option<String>,
    /// Price of one, in the shop currency's minor unit.
    pub price: i64,
    pub currency: String,
    /// The product's page, if it has one.
    pub url: Option<String>,
    /// An image path or URL, and its text alternative.
    pub image: Option<String>,
    pub image_alt: Option<String>,
    /// `false`: shown but not for sale.
    pub available: bool,
}

/// Product ids: `[a-z0-9][a-z0-9_-]{0,79}` (slugs fit).
pub fn valid_id(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 80
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'-' | b'_'))
}

/// A source of products.
pub trait Catalogue: Send + Sync {
    /// Everything on sale or shown, in display order.
    fn products(&self) -> BoxFuture<'_, Result<Vec<Product>>>;

    /// One product by id.
    fn product<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<Product>>>;
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileProduct {
    id: String,
    name: String,
    price: i64,
    summary: Option<String>,
    url: Option<String>,
    image: Option<String>,
    image_alt: Option<String>,
    #[serde(default = "yes")]
    available: bool,
}

fn yes() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileDoc {
    #[serde(default)]
    products: Vec<FileProduct>,
}

/// Products from a TOML file (`[[products]]` with `id`, `name`, `price`
/// in minor units, and optional `summary`, `url`, `image`, `image_alt`,
/// `available`). Read once when the server starts.
#[derive(Debug, Clone)]
pub struct FileCatalogue {
    list: Vec<Product>,
}

impl FileCatalogue {
    pub fn parse(text: &str, currency: &str, what: &str) -> Result<Self> {
        let doc: FileDoc = toml::from_str(text).with_context(|| format!("reading {what}"))?;
        let mut list = Vec::new();
        for p in doc.products {
            if !valid_id(&p.id) {
                bail!(
                    "{what}: product id `{}` must be lower-case letters, digits, - or _",
                    p.id
                );
            }
            if p.price < 0 {
                bail!("{what}: product `{}` has a negative price", p.id);
            }
            if list.iter().any(|q: &Product| q.id == p.id) {
                bail!("{what}: product `{}` is listed twice", p.id);
            }
            if p.name.trim().is_empty() {
                bail!("{what}: product `{}` has no name", p.id);
            }
            list.push(Product {
                id: p.id,
                name: p.name,
                summary: p.summary,
                price: p.price,
                currency: currency.to_string(),
                url: p.url,
                image: p.image,
                image_alt: p.image_alt,
                available: p.available,
            });
        }
        Ok(FileCatalogue { list })
    }

    /// Read `file` (relative to the site). A missing file is an empty shop.
    pub fn load(site: &Path, file: &Path, currency: &str) -> Result<Self> {
        let p = site.join(file);
        if !p.exists() {
            return Ok(FileCatalogue { list: Vec::new() });
        }
        let text =
            std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
        Self::parse(&text, currency, &file.display().to_string())
    }
}

impl Catalogue for FileCatalogue {
    fn products(&self) -> BoxFuture<'_, Result<Vec<Product>>> {
        Box::pin(async move { Ok(self.list.clone()) })
    }

    fn product<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<Product>>> {
        Box::pin(async move { Ok(self.list.iter().find(|p| p.id == id).cloned()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_catalogue() {
        let c = FileCatalogue::parse(
            r#"
[[products]]
id = "mug"
name = "Mug"
price = 1250

[[products]]
id = "poster-a2"
name = "Poster"
price = 900
available = false
"#,
            "GBP",
            "products.toml",
        )
        .unwrap();
        assert_eq!(c.list.len(), 2);
        assert_eq!(c.list[0].currency, "GBP");
        assert!(!c.list[1].available);
        for bad in [
            "[[products]]\nid = \"Mug\"\nname = \"x\"\nprice = 1",
            "[[products]]\nid = \"m\"\nname = \"x\"\nprice = -1",
            "[[products]]\nid = \"m\"\nname = \"x\"\nprice = 1\n[[products]]\nid = \"m\"\nname = \"y\"\nprice = 1",
            "[[products]]\nid = \"m\"\nname = \"x\"\nprice = 1\ncost = 2",
        ] {
            assert!(FileCatalogue::parse(bad, "GBP", "t").is_err(), "{bad}");
        }
    }
}
