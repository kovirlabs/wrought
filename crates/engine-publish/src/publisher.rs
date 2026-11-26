//! The publisher: re-renders pages when content changes.
//!
//! Flow for a batch of changes (from the change log, after a NOTIFY):
//!
//! 1. each changed item or term gives cache tags (`i:article:3`,
//!    `c:article`; `s:<type>` for singletons; `i:topics:7`, `c:topics` for
//!    terms) and the path where it lives now, if it is published;
//! 2. the tag index turns the tags into the paths whose last render read
//!    that data;
//! 3. each path is resolved against the route table: a `publish` route is
//!    rendered and written atomically, and its new tags are recorded; a path
//!    that no longer resolves (unpublished, deleted, renamed slug) is
//!    deleted and dropped from the index. `build` routes are never touched.
//!
//! Every other file is left alone, so an edit rewrites only the pages that
//! carry the edited item's tags.
//!
//! The publisher's methods are synchronous and block on the database; call
//! them from `spawn_blocking` (or a plain thread), not an async worker.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use anyhow::{Context, Result};
use engine_content::ContentSite;
use engine_core::builder::{BuildReport, Renderer};
use engine_core::config::RenderMode;
use engine_core::routes::{Route, output_file};
use engine_core::tags::CacheTag;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::sync::broadcast;

use crate::index::{PageEntry, TagIndex};
use crate::target::PublishTarget;

/// What one publish batch did.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct PublishReport {
    /// Why it ran: `changes 12..14`, `rebuild`, `miss /x/`.
    pub reason: String,
    /// Tags that triggered it.
    pub tags: Vec<String>,
    /// URL paths re-rendered and written.
    pub rendered: Vec<String>,
    /// URL paths re-rendered to the same bytes as their last published
    /// version and therefore not written again (only for targets that
    /// skip unchanged pages, such as a bucket: no upload, no invalidation).
    pub unchanged: Vec<String>,
    /// URL paths whose files were removed.
    pub deleted: Vec<String>,
    /// Paths that failed to render (their old file is kept).
    pub errors: Vec<String>,
    pub elapsed_ms: u128,
}

impl PublishReport {
    pub fn is_empty(&self) -> bool {
        self.rendered.is_empty() && self.deleted.is_empty() && self.errors.is_empty()
    }

    /// One line for logs.
    pub fn summary(&self) -> String {
        let mut s = format!(
            "publish ({}): {} rendered, {} deleted",
            self.reason,
            self.rendered.len(),
            self.deleted.len()
        );
        if !self.unchanged.is_empty() {
            s.push_str(&format!(", {} unchanged", self.unchanged.len()));
        }
        if !self.errors.is_empty() {
            s.push_str(&format!(", {} FAILED", self.errors.len()));
        }
        s.push_str(&format!(" in {} ms", self.elapsed_ms));
        if !self.tags.is_empty() {
            s.push_str(&format!(" [{}]", self.tags.join(" ")));
        }
        for p in &self.rendered {
            s.push_str(&format!("\n  wrote   {p}"));
        }
        for p in &self.deleted {
            s.push_str(&format!("\n  deleted {p}"));
        }
        for e in &self.errors {
            s.push_str(&format!("\n  error   {e}"));
        }
        s
    }
}

pub fn sha256_hex(body: &[u8]) -> String {
    format!("{:x}", Sha256::digest(body))
}

pub struct Publisher {
    /// Swapped by [`Publisher::reload`] when templates change.
    renderer: RwLock<Arc<Renderer>>,
    content: ContentSite,
    index: TagIndex,
    target: Box<dyn PublishTarget>,
    rt: tokio::runtime::Handle,
    lock: Mutex<()>,
    events: broadcast::Sender<PublishReport>,
}

impl Publisher {
    pub fn new(
        renderer: Renderer,
        content: ContentSite,
        index: TagIndex,
        target: Box<dyn PublishTarget>,
        rt: tokio::runtime::Handle,
    ) -> Self {
        let (events, _) = broadcast::channel(64);
        Publisher {
            renderer: RwLock::new(Arc::new(renderer)),
            content,
            index,
            target,
            rt,
            lock: Mutex::new(()),
            events,
        }
    }

    /// The current renderer (also used for `request`-mode pages).
    pub fn renderer(&self) -> Arc<Renderer> {
        self.renderer
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Replace the renderer (templates or Markdown changed) and re-render
    /// every page with it.
    pub fn reload(&self, renderer: Renderer) -> Result<PublishReport> {
        *self.renderer.write().unwrap_or_else(|p| p.into_inner()) = Arc::new(renderer);
        let mut r = self.rebuild_all()?;
        r.reason = "templates changed".into();
        Ok(r)
    }

    /// Receive a report after every batch that did something.
    pub fn subscribe(&self) -> broadcast::Receiver<PublishReport> {
        self.events.subscribe()
    }

    /// Whether missing pages may be rendered on request (not with a
    /// bucket target).
    pub fn renders_on_miss(&self) -> bool {
        self.target.render_on_miss()
    }

    /// The target's name (`disk`, `s3`).
    pub fn target_name(&self) -> &str {
        self.target.name()
    }

    pub fn index(&self) -> &TagIndex {
        &self.index
    }

    fn block_on<F: std::future::Future>(&self, f: F) -> F::Output {
        self.rt.block_on(f)
    }

    fn guard(&self) -> std::sync::MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Tell subscribers: the log printer, and the `request` mode's
    /// fragment cache, which evicts by the changed tags even when no
    /// published page was affected.
    fn emit(&self, report: &PublishReport) {
        if !report.is_empty() || !report.tags.is_empty() {
            let _ = self.events.send(report.clone());
        }
    }

    /// Render one route, write it, and record its tags if it is a publish
    /// route. Returns `false` if the target skips unchanged pages and the
    /// bytes equal the last published version (nothing was written).
    fn publish_route(&self, route: &Route) -> Result<bool> {
        let (html, tags) = self.renderer().render(route)?;
        let body = html.as_bytes();
        let sha256 = sha256_hex(body);
        let publish = route.mode == RenderMode::Publish;
        let unchanged = self.target.skip_unchanged()
            && if publish {
                // The tag index knows the hash of the last published bytes.
                self.block_on(self.index.sha256_of(&route.path))?.as_deref()
                    == Some(sha256.as_str())
            } else {
                // `build` routes are not indexed; ask the target.
                self.target.is_current(&route.output, body)
            };
        if !unchanged {
            self.target.put(&route.output, body)?;
        }
        if publish {
            self.block_on(self.index.record(&PageEntry {
                path: route.path.clone(),
                output: route.output.to_string_lossy().to_string(),
                sha256,
                tags: tags.iter().map(ToString::to_string).collect(),
            }))?;
        }
        Ok(!unchanged)
    }

    /// Re-render `paths` (publish routes only), delete the ones that no
    /// longer exist.
    fn publish_paths(&self, paths: BTreeSet<String>, report: &mut PublishReport) -> Result<()> {
        for path in paths {
            let route = match self.renderer().resolve_path(&path) {
                Ok(r) => r,
                Err(e) => {
                    report.errors.push(format!("{path}: {e:#}"));
                    continue;
                }
            };
            match route {
                Some(route) if route.mode == RenderMode::Publish => {
                    match self.publish_route(&route) {
                        Ok(true) => report.rendered.push(route.path),
                        Ok(false) => report.unchanged.push(route.path),
                        Err(e) => report.errors.push(format!("{path}: {e:#}")),
                    }
                }
                Some(_) => {} // a `build` route: only `engine build` writes it
                None => {
                    self.target.delete(&output_file(&path))?;
                    self.block_on(self.index.remove(&path))?;
                    report.deleted.push(path);
                }
            }
        }
        let mut changed = report.rendered.clone();
        changed.extend(report.deleted.iter().cloned());
        self.target.finish_batch(&changed)?;
        Ok(())
    }

    /// Publish everything the change log holds past the cursor.
    pub fn process_pending(&self) -> Result<PublishReport> {
        let _g = self.guard();
        let started = Instant::now();
        let cursor = self.block_on(self.index.cursor())?;
        let changes = self.block_on(self.index.pending(cursor, 10_000))?;
        let Some(last) = changes.last().map(|c| c.id) else {
            return Ok(PublishReport::default());
        };
        let mut report = PublishReport {
            reason: format!("changes {}..{}", changes[0].id, last),
            ..Default::default()
        };
        let mut tags = BTreeSet::new();
        let mut paths = BTreeSet::new();
        let mut seen = BTreeSet::new();
        for c in &changes {
            if !seen.insert((c.content_type.clone(), c.item_id)) {
                continue;
            }
            tags.extend(self.content.tags_for_change(&c.content_type, c.item_id));
            paths.extend(self.content.current_paths(&c.content_type, c.item_id)?);
        }
        let tag_strings: Vec<String> = tags.iter().map(ToString::to_string).collect();
        paths.extend(self.block_on(self.index.lookup(&tag_strings))?);
        report.tags = tag_strings;
        self.publish_paths(paths, &mut report)?;
        // Advance even if some pages failed: their old files stay, the
        // errors are reported, and a later `engine rebuild` retries them.
        // Not advancing would retry a broken template on every change.
        self.block_on(self.index.advance(last))?;
        report.elapsed_ms = started.elapsed().as_millis();
        self.emit(&report);
        Ok(report)
    }

    /// Re-render the pages carrying any of `tags`.
    pub fn rebuild_tags(&self, tags: &[CacheTag]) -> Result<PublishReport> {
        let _g = self.guard();
        let started = Instant::now();
        let tag_strings: Vec<String> = tags.iter().map(ToString::to_string).collect();
        let mut report = PublishReport {
            reason: "rebuild".into(),
            tags: tag_strings.clone(),
            ..Default::default()
        };
        let paths = self.block_on(self.index.lookup(&tag_strings))?;
        self.publish_paths(paths.into_iter().collect(), &mut report)?;
        report.elapsed_ms = started.elapsed().as_millis();
        self.emit(&report);
        Ok(report)
    }

    /// Re-render every route in place (build and publish), refresh the tag
    /// index, and delete published files whose routes are gone.
    pub fn rebuild_all(&self) -> Result<PublishReport> {
        let _g = self.guard();
        let started = Instant::now();
        let mut report = PublishReport {
            reason: "rebuild".into(),
            ..Default::default()
        };
        let cursor = self.block_on(self.index.latest_change())?;
        let routes: Vec<Route> = self
            .renderer()
            .routes()?
            .into_iter()
            .filter(|r| r.mode != RenderMode::Request)
            .collect();
        let mut live = BTreeSet::new();
        for route in &routes {
            live.insert(route.path.clone());
            match self.publish_route(route) {
                Ok(true) => report.rendered.push(route.path.clone()),
                Ok(false) => report.unchanged.push(route.path.clone()),
                Err(e) => report.errors.push(format!("{}: {e:#}", route.path)),
            }
        }
        for (path, output) in self.block_on(self.index.all())? {
            let still_publish = routes
                .iter()
                .any(|r| r.path == path && r.mode == RenderMode::Publish);
            if !still_publish {
                if !live.contains(&path) {
                    self.target.delete(Path::new(&output))?;
                    report.deleted.push(path.clone());
                }
                self.block_on(self.index.remove(&path))?;
            }
        }
        let mut changed = report.rendered.clone();
        changed.extend(report.deleted.iter().cloned());
        self.target.finish_batch(&changed)?;
        self.block_on(self.index.advance(cursor))?;
        report.elapsed_ms = started.elapsed().as_millis();
        self.emit(&report);
        Ok(report)
    }

    /// Render-on-miss: if `url_path` is a publish route whose file is
    /// missing, render and write it. Returns whether the file now exists.
    pub fn render_missing(&self, url_path: &str) -> Result<bool> {
        let Some(route) = self.renderer().resolve_path(url_path)? else {
            return Ok(false);
        };
        if route.mode != RenderMode::Publish || !self.target.render_on_miss() {
            return Ok(false);
        }
        let _g = self.guard();
        if self.target.exists(&route.output) {
            return Ok(true);
        }
        let started = Instant::now();
        self.publish_route(&route)
            .with_context(|| format!("rendering {} on a miss", route.path))?;
        let report = PublishReport {
            reason: format!("miss {}", route.path),
            rendered: vec![route.path.clone()],
            elapsed_ms: started.elapsed().as_millis(),
            ..Default::default()
        };
        self.emit(&report);
        Ok(true)
    }
}

/// After `engine build`: index every publish route the build wrote, and set
/// the cursor to `cursor` (the newest change before the build started), all
/// in one transaction.
pub async fn index_build(index: &TagIndex, report: &BuildReport, cursor: i64) -> Result<usize> {
    let mut entries = Vec::new();
    for r in report
        .routes
        .iter()
        .filter(|r| r.mode == RenderMode::Publish)
    {
        let body = std::fs::read(report.output.join(&r.output))
            .with_context(|| format!("reading {}", r.output.display()))?;
        entries.push(PageEntry {
            path: r.path.clone(),
            output: r.output.to_string_lossy().to_string(),
            sha256: sha256_hex(&body),
            tags: r.tags.clone(),
        });
    }
    index.replace_all(&entries, cursor).await?;
    Ok(entries.len())
}
