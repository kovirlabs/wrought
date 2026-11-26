//! Where media files are kept.
//!
//! The media folder on disk (`[media] dir`) always holds every file: it is
//! the working copy the admin reads private files from and `engine serve`
//! answers `/media/` from. A [`PublicStorage`] (the default is none) also
//! receives every *public* file and its resized copies when they are
//! stored, and loses them when they are deleted: with the `s3` feature the
//! site's bucket under `media/`, so the CDN serves them. Private files
//! (form attachments) never leave the disk.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A second home for public media files.
pub trait PublicStorage: Send + Sync {
    /// For messages (`s3://bucket/media/`).
    fn describe(&self) -> String;

    /// Store `body` at `rel` (`<id>/<file>`), replacing what is there.
    fn put<'a>(
        &'a self,
        rel: &'a str,
        body: &'a [u8],
        mime: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<()>>;

    /// Remove `rel` (no error if it is already gone).
    fn delete<'a>(&'a self, rel: &'a str) -> BoxFuture<'a, anyhow::Result<()>>;
}

/// An optional [`PublicStorage`] with a `Debug` that names it.
#[derive(Clone, Default)]
pub struct Public(pub Option<Arc<dyn PublicStorage>>);

impl std::fmt::Debug for Public {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            Some(s) => write!(f, "Public({})", s.describe()),
            None => f.write_str("Public(disk only)"),
        }
    }
}
