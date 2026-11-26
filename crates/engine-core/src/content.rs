//! File-based Markdown content.
//!
//! Convention (documented in `docs/content/docs/content.md`):
//!
//! * Every `.md` file under `content/` is a page. `content/about.md` is served
//!   at `/about/`, `content/blog/hello.md` at `/blog/hello/`.
//! * `_index.md` (or `index.md`) is the index of its folder: `content/_index.md`
//!   is the home page `/`, `content/blog/_index.md` is the section page
//!   `/blog/`, which lists the folder's pages and can paginate them.
//! * Front matter is TOML between `+++` lines at the top of the file.
//! * Cache-tag type of a page is its top-level folder (`blog`), or `page` for
//!   files directly in `content/`; the id is its path without `.md`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use pulldown_cmark::{CowStr, Event, HeadingLevel, Options, Parser, Tag, TagEnd, html};
use serde::{Deserialize, Serialize};

use crate::config::RenderMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SortBy {
    #[default]
    Date,
    Weight,
    Title,
}

/// Front matter keys. Unknown keys are errors (typos are caught); put
/// free-form values under `[extra]`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FrontMatter {
    pub title: Option<String>,
    pub description: Option<String>,
    pub date: Option<toml::Value>,
    pub updated: Option<toml::Value>,
    pub template: Option<String>,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub weight: i64,
    pub slug: Option<String>,
    pub mode: Option<RenderMode>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub author: Option<String>,
    // Section (`_index.md`) settings.
    /// Pages per listing page; 0 or absent means no pagination.
    pub paginate: Option<usize>,
    pub sort_by: Option<SortBy>,
    /// Template for the section's pages (default `page.html`).
    pub page_template: Option<String>,
    #[serde(default)]
    pub extra: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PageKind {
    Page,
    Section,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Heading {
    pub level: u8,
    pub id: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Page {
    /// Path under `content/` without `.md`, e.g. `blog/hello`, `blog/_index`.
    pub id: String,
    pub kind: PageKind,
    /// Folder the page lives in (`blog`, or `""` for the root).
    pub section: String,
    /// Cache-tag type: the top-level folder, or `page`.
    pub tag_type: String,
    /// Source path relative to the site folder.
    pub source: PathBuf,
    /// Root-relative URL with a trailing slash, e.g. `/blog/hello/`.
    pub url: String,
    pub title: String,
    pub description: Option<String>,
    /// ISO date (`2026-11-20` or a full RFC 3339 time).
    pub date: Option<String>,
    pub updated: Option<String>,
    pub draft: bool,
    pub weight: i64,
    pub tags: Vec<String>,
    pub author: Option<String>,
    pub mode: Option<RenderMode>,
    pub template: Option<String>,
    /// Rendered HTML of the body. Templates insert it with `| safe`.
    pub content: String,
    /// HTML of the first paragraph (or everything before `<!-- more -->`).
    pub summary: String,
    pub toc: Vec<Heading>,
    pub word_count: usize,
    pub reading_minutes: usize,
    pub extra: BTreeMap<String, toml::Value>,
    #[serde(skip)]
    pub front: FrontMatter,
}

impl Page {
    /// The tag id of the page itself (`i:<type>:<id>`).
    pub fn tag(&self) -> crate::tags::CacheTag {
        crate::tags::CacheTag::item(&self.tag_type, &self.id)
    }
}

/// All content of a site.
#[derive(Debug, Default)]
pub struct Content {
    pub pages: Vec<Page>,
    by_id: HashMap<String, usize>,
}

impl Content {
    /// Load every `.md` file under `dir` (a missing folder is empty content).
    pub fn load(site_dir: &Path, include_drafts: bool) -> Result<Self> {
        let dir = site_dir.join("content");
        let mut files = Vec::new();
        if dir.is_dir() {
            walk(&dir, &mut files)?;
        }
        files.sort();
        let mut pages = Vec::new();
        for file in files {
            let rel = file.strip_prefix(&dir).expect("walked under dir");
            let text = std::fs::read_to_string(&file)
                .with_context(|| format!("reading {}", file.display()))?;
            let page =
                parse_page(rel, &text).with_context(|| format!("in content/{}", rel.display()))?;
            if page.draft && !include_drafts {
                continue;
            }
            pages.push(page);
        }
        let mut c = Content {
            pages,
            by_id: HashMap::new(),
        };
        let mut urls: HashMap<&str, &Path> = HashMap::new();
        for p in &c.pages {
            if let Some(other) = urls.insert(&p.url, &p.source) {
                bail!(
                    "{} and {} both map to {}",
                    other.display(),
                    p.source.display(),
                    p.url
                );
            }
        }
        c.reindex();
        Ok(c)
    }

    fn reindex(&mut self) {
        self.by_id = self
            .pages
            .iter()
            .enumerate()
            .map(|(i, p)| (p.id.clone(), i))
            .collect();
    }

    pub fn get(&self, id: &str) -> Option<&Page> {
        let id = id.trim_matches('/');
        let idx = self
            .by_id
            .get(id)
            .or_else(|| self.by_id.get(&format!("{id}/_index")))
            .or_else(|| {
                if id.is_empty() {
                    self.by_id.get("_index")
                } else {
                    None
                }
            })?;
        Some(&self.pages[*idx])
    }

    pub fn by_url(&self, url: &str) -> Option<&Page> {
        self.pages.iter().find(|p| p.url == url)
    }

    /// The section index page of folder `section` (`""` is the home page).
    pub fn section_index(&self, section: &str) -> Option<&Page> {
        self.pages
            .iter()
            .find(|p| p.kind == PageKind::Section && p.id_folder() == section)
    }

    /// Regular pages directly inside folder `section`, in listing order.
    pub fn section_pages(&self, section: &str) -> Vec<&Page> {
        let sort =
            self.section_index(section)
                .and_then(|s| s.front.sort_by)
                .unwrap_or_else(|| {
                    let any_dated = self.pages.iter().any(|p| {
                        p.kind == PageKind::Page && p.section == section && p.date.is_some()
                    });
                    if any_dated {
                        SortBy::Date
                    } else {
                        SortBy::Weight
                    }
                });
        let mut v: Vec<&Page> = self
            .pages
            .iter()
            .filter(|p| p.kind == PageKind::Page && p.section == section)
            .collect();
        match sort {
            SortBy::Date => v.sort_by(|a, b| b.date.cmp(&a.date).then(a.title.cmp(&b.title))),
            SortBy::Weight => v.sort_by(|a, b| a.weight.cmp(&b.weight).then(a.title.cmp(&b.title))),
            SortBy::Title => v.sort_by(|a, b| a.title.cmp(&b.title)),
        }
        v
    }

    /// Ancestor section pages of `page`, from the home page down (excluding
    /// the page itself). Only sections that have an index page appear.
    pub fn ancestors(&self, page: &Page) -> Vec<&Page> {
        let mut folders = vec![String::new()];
        let folder = page.id_folder();
        let mut acc = String::new();
        for seg in folder.split('/').filter(|s| !s.is_empty()) {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(seg);
            folders.push(acc.clone());
        }
        if page.kind == PageKind::Section {
            folders.pop();
        }
        folders
            .iter()
            .filter_map(|f| self.section_index(f))
            .filter(|p| p.id != page.id)
            .collect()
    }
}

impl Page {
    /// For a section index: the folder it indexes. For a page: its folder.
    pub fn id_folder(&self) -> &str {
        match self.kind {
            PageKind::Section => self
                .id
                .strip_suffix("/_index")
                .or_else(|| self.id.strip_suffix("_index"))
                .unwrap_or(&self.id),
            PageKind::Page => &self.section,
        }
    }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        let ft = entry.file_type()?;
        if ft.is_dir() {
            walk(&entry.path(), out)?;
        } else if ft.is_file() && name.ends_with(".md") {
            out.push(entry.path());
        }
    }
    Ok(())
}

/// Turn a file or folder name into a URL segment.
pub fn slug_segment(seg: &str) -> Result<String> {
    let s: String = seg
        .trim()
        .chars()
        .map(|c| {
            if c.is_whitespace() {
                '-'
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect();
    let ok = !s.is_empty()
        && !s.starts_with('.')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !ok {
        bail!(
            "`{seg}` can't be used in a URL: use letters, digits, `-`, `_` and `.` (or set `slug`)"
        );
    }
    Ok(s)
}

/// Split `+++` TOML front matter from the body.
pub fn split_front_matter(text: &str) -> Result<(FrontMatter, &str)> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let first_line_end = text.find('\n').map(|i| i + 1).unwrap_or(text.len());
    let first = text[..first_line_end].trim_end();
    if first == "---" {
        bail!("front matter must be TOML between `+++` lines (YAML `---` is not supported)");
    }
    if first != "+++" {
        return Ok((FrontMatter::default(), text));
    }
    let rest = &text[first_line_end..];
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "+++" {
            let fm_text = &rest[..offset];
            let body = &rest[offset + line.len()..];
            let fm: FrontMatter =
                toml::from_str(fm_text).map_err(|e| anyhow!("front matter: {e}"))?;
            return Ok((fm, body));
        }
        offset += line.len();
    }
    bail!("front matter starts with `+++` but has no closing `+++` line")
}

fn date_string(v: &Option<toml::Value>, key: &str) -> Result<Option<String>> {
    let s = match v {
        None => return Ok(None),
        Some(toml::Value::Datetime(d)) => d.to_string(),
        Some(toml::Value::String(s)) => s.clone(),
        Some(other) => bail!("`{key}` must be a date like 2026-11-20, got {other}"),
    };
    let b = s.as_bytes();
    let ok = b.len() >= 10
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit);
    if !ok {
        bail!("`{key}` must be a date like 2026-11-20, got `{s}`");
    }
    Ok(Some(s))
}

/// Parse one content file. `rel` is its path under `content/`.
pub fn parse_page(rel: &Path, text: &str) -> Result<Page> {
    let (front, body) = split_front_matter(text)?;

    let mut segs: Vec<String> = Vec::new();
    for c in rel.components() {
        let s = c
            .as_os_str()
            .to_str()
            .ok_or_else(|| anyhow!("file name is not UTF-8"))?;
        segs.push(s.to_string());
    }
    let file = segs.pop().ok_or_else(|| anyhow!("empty path"))?;
    let stem = file.strip_suffix(".md").unwrap_or(&file).to_string();
    let is_index = stem == "_index" || stem == "index";
    let kind = if is_index {
        PageKind::Section
    } else {
        PageKind::Page
    };

    let folder_slugs: Vec<String> = segs
        .iter()
        .map(|s| slug_segment(s))
        .collect::<Result<_>>()?;
    let section = folder_slugs.join("/");
    let id = if is_index {
        if section.is_empty() {
            "_index".to_string()
        } else {
            format!("{section}/_index")
        }
    } else {
        let s = slug_segment(&stem)?;
        if section.is_empty() {
            s
        } else {
            format!("{section}/{s}")
        }
    };
    let tag_type = match (kind, folder_slugs.first()) {
        (_, Some(top)) => top.clone(),
        (PageKind::Section, None) => "page".to_string(),
        (PageKind::Page, None) => "page".to_string(),
    };

    let mut url_segs = folder_slugs.clone();
    if !is_index {
        let last = match &front.slug {
            Some(s) => slug_segment(s).context("in `slug`")?,
            None => slug_segment(&stem)?,
        };
        url_segs.push(last);
    } else if front.slug.is_some() {
        bail!("`slug` is not allowed on a section index; rename the folder instead");
    }
    let url = if url_segs.is_empty() {
        "/".to_string()
    } else {
        format!("/{}/", url_segs.join("/"))
    };

    for t in [&front.template, &front.page_template]
        .into_iter()
        .flatten()
    {
        crate::templates::validate_template_name(t).map_err(|e| anyhow!(e))?;
    }

    let rendered = render_markdown(body);
    let title = front.title.clone().unwrap_or_else(|| {
        rendered
            .toc
            .iter()
            .find(|h| h.level == 1)
            .map(|h| h.text.clone())
            .unwrap_or_else(|| title_from_stem(if is_index { &section } else { &stem }))
    });
    let date = date_string(&front.date, "date")?;
    let updated = date_string(&front.updated, "updated")?;

    Ok(Page {
        id,
        kind,
        section: if is_index {
            // A section index belongs to its parent folder.
            match section.rsplit_once('/') {
                Some((parent, _)) => parent.to_string(),
                None => String::new(),
            }
        } else {
            section
        },
        tag_type,
        source: Path::new("content").join(rel),
        url,
        title,
        description: front.description.clone(),
        date,
        updated,
        draft: front.draft,
        weight: front.weight,
        tags: front.tags.clone(),
        author: front.author.clone(),
        mode: front.mode,
        template: front.template.clone(),
        content: rendered.html,
        summary: rendered.summary,
        toc: rendered.toc,
        word_count: rendered.words,
        reading_minutes: rendered.words.div_ceil(220).max(1),
        extra: front.extra.clone(),
        front,
    })
}

fn title_from_stem(stem: &str) -> String {
    let last = stem.rsplit('/').next().unwrap_or(stem);
    let words: Vec<String> = last
        .split(['-', '_'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect();
    if words.is_empty() {
        "Home".to_string()
    } else {
        words.join(" ")
    }
}

pub struct Rendered {
    pub html: String,
    pub summary: String,
    pub toc: Vec<Heading>,
    pub words: usize,
}

/// Make a heading id from its text: lowercase words joined by `-`.
pub fn heading_id(text: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in text.chars() {
        if c.is_alphanumeric() {
            if dash && !out.is_empty() {
                out.push('-');
            }
            dash = false;
            out.extend(c.to_lowercase());
        } else {
            dash = true;
        }
    }
    if out.is_empty() {
        "section".to_string()
    } else {
        out
    }
}

fn level_num(l: HeadingLevel) -> u8 {
    match l {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// Render Markdown (CommonMark plus tables, footnotes, strikethrough, task
/// lists and heading attributes). Headings get ids so TagAction can add
/// anchor links and the `toc` component can link to them.
///
/// Raw HTML in Markdown is passed through: in build mode the content author
/// is trusted. CMS-edited content (v0.2+) must be sanitised before this.
pub fn render_markdown(md: &str) -> Rendered {
    let opts = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_HEADING_ATTRIBUTES
        | Options::ENABLE_SMART_PUNCTUATION;
    let mut events: Vec<Event> = Parser::new_ext(md, opts).collect();

    // Assign ids to headings and collect the table of contents.
    let mut used: HashMap<String, usize> = HashMap::new();
    let mut toc = Vec::new();
    let mut words = 0;
    let mut i = 0;
    while i < events.len() {
        match &events[i] {
            Event::Text(t) | Event::Code(t) => words += t.split_whitespace().count(),
            _ => {}
        }
        if let Event::Start(Tag::Heading { level, id, .. }) = &events[i] {
            let level = *level;
            let existing = id.clone();
            let mut text = String::new();
            let mut j = i + 1;
            while j < events.len() {
                match &events[j] {
                    Event::End(TagEnd::Heading(_)) => break,
                    Event::Text(t) | Event::Code(t) => text.push_str(t),
                    _ => {}
                }
                j += 1;
            }
            let base = existing
                .map(|s| s.to_string())
                .unwrap_or_else(|| heading_id(&text));
            let n = used.entry(base.clone()).or_insert(0);
            let unique = if *n == 0 {
                base.clone()
            } else {
                format!("{base}-{n}")
            };
            *n += 1;
            if let Event::Start(Tag::Heading { id, .. }) = &mut events[i] {
                *id = Some(CowStr::from(unique.clone()));
            }
            toc.push(Heading {
                level: level_num(level),
                id: unique,
                text: text.trim().to_string(),
            });
        }
        i += 1;
    }

    // Summary: everything before `<!-- more -->`, else the first paragraph.
    let more = events.iter().position(|e| match e {
        Event::Html(h) | Event::InlineHtml(h) => h.trim() == "<!-- more -->",
        _ => false,
    });
    let summary_events: Vec<Event> = match more {
        Some(pos) => events[..pos].to_vec(),
        None => {
            let start = events
                .iter()
                .position(|e| matches!(e, Event::Start(Tag::Paragraph)));
            match start {
                Some(s) => {
                    let end = events[s..]
                        .iter()
                        .position(|e| matches!(e, Event::End(TagEnd::Paragraph)))
                        .map(|e| s + e + 1)
                        .unwrap_or(events.len());
                    events[s..end].to_vec()
                }
                None => Vec::new(),
            }
        }
    };
    let mut summary = String::new();
    html::push_html(&mut summary, summary_events.into_iter());

    let mut out = String::with_capacity(md.len() * 2);
    html::push_html(&mut out, events.into_iter());
    Rendered {
        html: out,
        summary,
        toc,
        words,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn front_matter_and_urls() {
        let p = parse_page(
            Path::new("blog/Hello World.md"),
            "+++\ntitle = \"Hi\"\ndate = 2026-11-20\ntags = [\"a\"]\n+++\nFirst para.\n\nSecond.\n",
        )
        .unwrap();
        assert_eq!(p.id, "blog/hello-world");
        assert_eq!(p.url, "/blog/hello-world/");
        assert_eq!(p.section, "blog");
        assert_eq!(p.tag_type, "blog");
        assert_eq!(p.title, "Hi");
        assert_eq!(p.date.as_deref(), Some("2026-11-20"));
        assert_eq!(p.summary.trim(), "<p>First para.</p>");
        assert_eq!(p.tag().to_string(), "i:blog:blog/hello-world");
    }

    #[test]
    fn index_files_are_sections() {
        let home = parse_page(Path::new("_index.md"), "# Welcome\n").unwrap();
        assert_eq!(home.url, "/");
        assert_eq!(home.kind, PageKind::Section);
        assert_eq!(home.title, "Welcome");
        assert_eq!(home.id_folder(), "");
        let blog = parse_page(Path::new("blog/_index.md"), "").unwrap();
        assert_eq!(blog.url, "/blog/");
        assert_eq!(blog.id_folder(), "blog");
        assert_eq!(blog.section, "");
        assert_eq!(blog.title, "Blog");
        let nested = parse_page(Path::new("docs/guide/index.md"), "").unwrap();
        assert_eq!(nested.url, "/docs/guide/");
        assert_eq!(nested.section, "docs");
    }

    #[test]
    fn slug_overrides_last_segment() {
        let p = parse_page(Path::new("about.md"), "+++\nslug = \"about-us\"\n+++\n").unwrap();
        assert_eq!(p.url, "/about-us/");
        assert!(parse_page(Path::new("a.md"), "+++\nslug = \"../x\"\n+++\n").is_err());
    }

    #[test]
    fn bad_front_matter_is_reported() {
        assert!(parse_page(Path::new("a.md"), "---\ntitle: x\n---\n").is_err());
        assert!(parse_page(Path::new("a.md"), "+++\ntitle = 'x'\n").is_err());
        assert!(parse_page(Path::new("a.md"), "+++\ntitel = 'x'\n+++\n").is_err());
        assert!(parse_page(Path::new("a.md"), "+++\ndate = 'yesterday'\n+++\n").is_err());
        assert!(parse_page(Path::new("a.md"), "+++\ntemplate = '../../x'\n+++\n").is_err());
        assert!(parse_page(Path::new("bad name!.md"), "").is_err());
    }

    #[test]
    fn headings_get_unique_ids_and_toc() {
        let r = render_markdown(
            "# Title\n\n## Getting started\n\n## Getting started\n\n### `engine new`\n",
        );
        assert!(
            r.html.contains(r#"<h2 id="getting-started">"#),
            "{}",
            r.html
        );
        assert!(
            r.html.contains(r#"<h2 id="getting-started-1">"#),
            "{}",
            r.html
        );
        assert!(r.html.contains(r#"<h3 id="engine-new">"#), "{}", r.html);
        assert_eq!(r.toc.len(), 4);
        assert_eq!(r.toc[3].text, "engine new");
    }

    #[test]
    fn more_marker_sets_summary() {
        let r = render_markdown("One.\n\nTwo.\n\n<!-- more -->\n\nThree.\n");
        assert!(r.summary.contains("One.") && r.summary.contains("Two."));
        assert!(!r.summary.contains("Three."));
    }

    #[test]
    fn content_tree_queries() {
        let dir = tempfile::tempdir().unwrap();
        let c = dir.path().join("content");
        std::fs::create_dir_all(c.join("blog")).unwrap();
        std::fs::write(c.join("_index.md"), "# Home\n").unwrap();
        std::fs::write(c.join("blog/_index.md"), "+++\ntitle='Blog'\n+++\n").unwrap();
        std::fs::write(c.join("blog/a.md"), "+++\ndate=2026-01-01\n+++\nA").unwrap();
        std::fs::write(c.join("blog/b.md"), "+++\ndate=2026-02-01\n+++\nB").unwrap();
        std::fs::write(c.join("blog/d.md"), "+++\ndraft=true\n+++\nD").unwrap();
        let content = Content::load(dir.path(), false).unwrap();
        assert_eq!(content.pages.len(), 4);
        let order: Vec<_> = content
            .section_pages("blog")
            .iter()
            .map(|p| p.id.clone())
            .collect();
        assert_eq!(order, ["blog/b", "blog/a"]);
        let b = content.get("blog/b").unwrap();
        let anc: Vec<_> = content.ancestors(b).iter().map(|p| p.url.clone()).collect();
        assert_eq!(anc, ["/", "/blog/"]);
        assert_eq!(content.get("blog").unwrap().title, "Blog");
        assert!(
            Content::load(dir.path(), true)
                .unwrap()
                .get("blog/d")
                .is_some()
        );
    }
}
