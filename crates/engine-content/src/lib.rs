//! Wrought's `content` feature: content types declared in `types/*.toml`,
//! stored in the site's database.
//!
//! * [`types`]: type and block definitions, validated strictly;
//! * [`schema`]: `engine types sync`, which turns type files into migrations;
//! * [`store`]: reading and saving items, with optimistic locking and a
//!   revision row in every save's transaction; taxonomy terms;
//! * [`value`]: input validation; [`slug`]: URL slugs;
//! * [`sanitize`]: safe rendering of stored Markdown and links;
//! * [`render`]: the [`render::ContentSite`] extension that gives templates
//!   `list_items()`, `get_item()`, `get_singleton()`, `list_terms()` and
//!   `render_block()`, and expands
//!   item and term routes;
//! * [`seed`]: `seed/*.toml` files;
//! * [`site`]: opening a site against its database.

pub mod render;
pub mod sanitize;
pub mod schema;
pub mod seed;
pub mod site;
pub mod slug;
pub mod sql;
pub mod store;
pub mod types;
pub mod value;

use engine_db::{Backend, Migration};

pub use render::ContentSite;
pub use site::ContentApp;
pub use store::{Changes, Item, Key, ListQuery, SaveError, Status, Store};
pub use types::Registry;

/// The `content` feature's own schema (revisions, terms, change log).
pub fn migrations(backend: Backend) -> Vec<Migration> {
    match backend {
        Backend::Postgres => vec![
            Migration::new(
                "content/0001_base",
                include_str!("../migrations/postgres/0001_base.sql"),
            ),
            Migration::new(
                "content/0002_term_index",
                include_str!("../migrations/postgres/0002_term_index.sql"),
            ),
        ],
        Backend::Sqlite => vec![
            Migration::new(
                "content/0001_base",
                include_str!("../migrations/sqlite/0001_base.sql"),
            ),
            Migration::new(
                "content/0002_term_index",
                include_str!("../migrations/sqlite/0002_term_index.sql"),
            ),
        ],
    }
}
