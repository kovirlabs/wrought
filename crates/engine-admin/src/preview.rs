//! Drafts rendered by the site's own templates, for the `workflow`
//! feature's preview page and the visual editor's live preview.

use std::future::Future;
use std::pin::Pin;

use axum::http::HeaderMap;
use engine_content::store::Item;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Renders a draft with the site's templates (implemented by the CLI,
/// which owns the renderer).
pub trait Preview: Send + Sync {
    /// The page HTML, or `None` if the item's type has no page. With
    /// `block_markers`, blocks drawn by `render_block` are wrapped in
    /// `<!--wrought-block:ID-->` comments (the visual editor's preview).
    /// Never cached, never written to disk.
    fn render<'a>(
        &'a self,
        item: &'a Item,
        headers: &'a HeaderMap,
        block_markers: bool,
    ) -> BoxFuture<'a, anyhow::Result<Option<String>>>;
}
