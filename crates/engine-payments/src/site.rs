//! The Commerce components (`components/product_card`, `product_grid`,
//! `cart`, `checkout_summary`, `order_history`), the template functions
//! behind them and the `shop` global.
//!
//! Functions:
//!
//! * `shop_products(limit=None)`: the catalogue, each product with
//!   `price_display`;
//! * `shop_product(id)`: one product, or none;
//! * `shop_cart()`: the signed-in visitor's cart priced from the catalogue
//!   (`lines`, `count`, `total`, `total_display`, `unavailable`, `empty`,
//!   and `checkout_key`, a fresh idempotency key for the checkout form);
//!   empty for visitors who are not signed in and inside shared fragments;
//! * `shop_orders(limit=20)` and `shop_order(id)`: the visitor's own orders
//!   (with `lines`);
//! * `money(amount, currency=None)`, also a filter: `1250 | money` ->
//!   `£12.50` (the shop's currency unless another is given).

use std::sync::Arc;

use include_dir::{Dir, include_dir};
use minijinja::value::{Kwargs, Value};
use minijinja::{Error, ErrorKind, State, context};

use engine_core::builder::SiteExtension;

use crate::catalogue::{Catalogue, Product};
use crate::orders::{self, OrderStore};
use crate::{PaymentsConfig, money};

/// Templates of the Commerce components.
pub static COMPONENTS: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/site-templates");

/// The template extension of the `payments` feature.
#[derive(Clone)]
pub struct ShopExtension {
    store: OrderStore,
    catalogue: Arc<dyn Catalogue>,
    config: PaymentsConfig,
    rt: tokio::runtime::Handle,
}

fn mj(e: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::InvalidOperation, e.to_string())
}

fn user_id(state: &State) -> Option<i64> {
    state
        .lookup("user")
        .filter(|v| !v.is_undefined() && !v.is_none())
        .and_then(|u| u.get_attr("id").ok())
        .and_then(|v| i64::try_from(v).ok())
}

fn product_value(p: &Product) -> Value {
    context! {
        id => p.id, name => p.name, summary => p.summary, price => p.price,
        currency => p.currency, price_display => money::format(p.price, &p.currency),
        url => p.url, image => p.image, image_alt => p.image_alt, available => p.available,
    }
}

impl ShopExtension {
    pub fn new(
        store: OrderStore,
        catalogue: Arc<dyn Catalogue>,
        config: PaymentsConfig,
        rt: tokio::runtime::Handle,
    ) -> Self {
        ShopExtension {
            store,
            catalogue,
            config,
            rt,
        }
    }

    fn products(&self, kwargs: Kwargs) -> Result<Value, Error> {
        let limit: Option<usize> = kwargs.get("limit")?;
        kwargs.assert_all_used()?;
        let mut list = self.rt.block_on(self.catalogue.products()).map_err(mj)?;
        if let Some(l) = limit {
            list.truncate(l);
        }
        Ok(Value::from(
            list.iter().map(product_value).collect::<Vec<_>>(),
        ))
    }

    fn product(&self, id: String) -> Result<Value, Error> {
        if !crate::catalogue::valid_id(&id) {
            return Ok(Value::from(()));
        }
        let p = self.rt.block_on(self.catalogue.product(&id)).map_err(mj)?;
        Ok(p.as_ref().map(product_value).unwrap_or(Value::from(())))
    }

    fn cart(&self, state: &State) -> Result<Value, Error> {
        let currency = &self.config.currency;
        let Some(uid) = user_id(state) else {
            return Ok(context! {
                signed_in => false, empty => true, lines => Vec::<Value>::new(), count => 0,
                total => 0, total_display => money::format(0, currency), currency => currency,
                unavailable => Vec::<String>::new(),
            });
        };
        let cart = self
            .rt
            .block_on(async {
                let lines = self.store.cart_lines(uid).await?;
                orders::price_cart(&lines, self.catalogue.as_ref(), currency).await
            })
            .map_err(mj)?;
        Ok(context! {
            signed_in => true, empty => cart.is_empty(), lines => cart.lines, count => cart.count,
            total => cart.total, total_display => cart.total_display, currency => cart.currency,
            unavailable => cart.unavailable,
            checkout_key => engine_core::web::secret::token(),
        })
    }

    fn orders(&self, state: &State, kwargs: Kwargs) -> Result<Value, Error> {
        let limit: Option<i64> = kwargs.get("limit")?;
        kwargs.assert_all_used()?;
        let Some(uid) = user_id(state) else {
            return Ok(Value::from(Vec::<Value>::new()));
        };
        let list = self
            .rt
            .block_on(self.store.orders_of(uid, limit.unwrap_or(20).clamp(1, 200)))
            .map_err(mj)?;
        Ok(Value::from_serialize(&list))
    }

    fn order(&self, state: &State, id: String) -> Result<Value, Error> {
        let Some(uid) = user_id(state) else {
            return Ok(Value::from(()));
        };
        if !id.starts_with("ord_") || id.len() > 44 {
            return Ok(Value::from(()));
        }
        let o = self
            .rt
            .block_on(self.store.order_by_public(&id))
            .map_err(mj)?
            .filter(|o| o.user_id == Some(uid));
        Ok(o.map(|o| Value::from_serialize(&o))
            .unwrap_or(Value::from(())))
    }
}

impl SiteExtension for ShopExtension {
    fn extend_env(&self, env: &mut minijinja::Environment<'static>) {
        let me = Arc::new(self.clone());
        let m = me.clone();
        env.add_function("shop_products", move |kwargs: Kwargs| m.products(kwargs));
        let m = me.clone();
        env.add_function("shop_product", move |id: String| m.product(id));
        let m = me.clone();
        env.add_function("shop_cart", move |state: &State| m.cart(state));
        let m = me.clone();
        env.add_function("shop_orders", move |state: &State, kwargs: Kwargs| {
            m.orders(state, kwargs)
        });
        let m = me.clone();
        env.add_function("shop_order", move |state: &State, id: String| {
            m.order(state, id)
        });
        let cur = self.config.currency.clone();
        let fmt = move |amount: i64, currency: Option<String>| -> Result<String, Error> {
            let c = currency.unwrap_or_else(|| cur.clone());
            if !money::valid_currency(&c) {
                return Err(mj(format!("money: `{c}` is not a currency code")));
            }
            Ok(money::format(amount, &c))
        };
        env.add_function("money", fmt.clone());
        env.add_filter("money", fmt);
        env.add_global(
            "shop",
            context! {
                currency => self.config.currency,
                cart_action => "/_engine/shop/cart/",
                checkout_action => "/_engine/shop/checkout/",
                cart_page => self.config.cancel_path,
                orders_page => self.config.success_path,
            },
        );
    }

    fn template_dirs(&self) -> Vec<&'static Dir<'static>> {
        vec![&COMPONENTS]
    }
}
