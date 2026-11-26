//! Draft previews for the admin (`workflow` feature): a draft rendered by
//! the site's own template for its type, as a `request` render that is
//! never cached or written.

use axum::http::HeaderMap;
use engine_admin::workflow::BoxFuture;
use engine_content::ContentSite;
use engine_content::store::Item;
use minijinja::context;

use crate::request::RequestMode;

pub struct Preview {
    pub rm: RequestMode,
    pub content: ContentSite,
    pub public_url: String,
}

impl engine_admin::workflow::Preview for Preview {
    fn render<'a>(
        &'a self,
        item: &'a Item,
        headers: &'a HeaderMap,
    ) -> BoxFuture<'a, anyhow::Result<Option<String>>> {
        Box::pin(async move {
            let r = (self.rm.current)();
            let Some((index, page)) = self.content.preview_page(r.config(), item) else {
                return Ok(None);
            };
            let session = self.rm.session(headers).await;
            let path = page.path.clone();
            let ctx = context! {
                user => RequestMode::user_value(session.as_ref()),
                request => context! { path => path, query => (), args => context! {} },
                // Fragments are not cached in previews: a draft must
                // never end up in a fragment the public sees.
                preview => true,
            };
            let html = tokio::task::spawn_blocking(move || r.render_dynamic(index, page, &ctx))
                .await??
                .0;
            let _ = &self.public_url;
            Ok(Some(html))
        })
    }
}
