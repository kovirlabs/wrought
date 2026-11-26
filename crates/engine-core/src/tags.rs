//! Cache tags.
//!
//! Every render records which data it read, as tags:
//!
//! * `i:<type>:<id>` — one item (a page, a product, ...)
//! * `c:<type>` — a collection/listing of a type
//! * `s:<singleton>` — a singleton such as site settings or the main menu
//!
//! In v0.1 nothing consumes them yet; they are reported by `engine build -v`.
//! `publish` mode (v0.2) will use them to decide which files to rewrite and
//! `request` mode (v0.4) to evict fragments.
//!
//! Recording goes through a scoped, thread-local [`TagCollector`]: code that
//! loads data (Rust loaders or template functions) calls [`record`] without
//! threading a collector through every signature. [`collect`] installs a
//! fresh collector for the duration of one render and returns what it saw.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use serde::{Serialize, Serializer};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CacheTag {
    Item { kind: String, id: String },
    Collection { kind: String },
    Singleton { name: String },
}

impl CacheTag {
    pub fn item(kind: impl Into<String>, id: impl Into<String>) -> Self {
        CacheTag::Item {
            kind: kind.into(),
            id: id.into(),
        }
    }
    pub fn collection(kind: impl Into<String>) -> Self {
        CacheTag::Collection { kind: kind.into() }
    }
    pub fn singleton(name: impl Into<String>) -> Self {
        CacheTag::Singleton { name: name.into() }
    }
}

impl fmt::Display for CacheTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CacheTag::Item { kind, id } => write!(f, "i:{kind}:{id}"),
            CacheTag::Collection { kind } => write!(f, "c:{kind}"),
            CacheTag::Singleton { name } => write!(f, "s:{name}"),
        }
    }
}

impl Serialize for CacheTag {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
#[error("invalid cache tag `{0}` (expected i:<type>:<id>, c:<type> or s:<name>)")]
pub struct ParseTagError(String);

impl FromStr for CacheTag {
    type Err = ParseTagError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || ParseTagError(s.to_string());
        let (prefix, rest) = s.split_once(':').ok_or_else(err)?;
        if rest.is_empty() {
            return Err(err());
        }
        match prefix {
            "i" => {
                let (kind, id) = rest.split_once(':').ok_or_else(err)?;
                if kind.is_empty() || id.is_empty() {
                    return Err(err());
                }
                Ok(CacheTag::item(kind, id))
            }
            "c" if !rest.contains(':') => Ok(CacheTag::collection(rest)),
            "s" if !rest.contains(':') => Ok(CacheTag::singleton(rest)),
            _ => Err(err()),
        }
    }
}

/// The set of tags one render touched.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct TagCollector {
    tags: BTreeSet<CacheTag>,
}

impl TagCollector {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn insert(&mut self, tag: CacheTag) {
        self.tags.insert(tag);
    }
    pub fn contains(&self, tag: &CacheTag) -> bool {
        self.tags.contains(tag)
    }
    pub fn iter(&self) -> impl Iterator<Item = &CacheTag> {
        self.tags.iter()
    }
    pub fn len(&self) -> usize {
        self.tags.len()
    }
    pub fn is_empty(&self) -> bool {
        self.tags.is_empty()
    }
    pub fn into_vec(self) -> Vec<CacheTag> {
        self.tags.into_iter().collect()
    }
}

thread_local! {
    static STACK: RefCell<Vec<TagCollector>> = const { RefCell::new(Vec::new()) };
}

/// Record a tag into the innermost active collector. A no-op outside
/// [`collect`], so loaders can always call it.
pub fn record(tag: CacheTag) {
    STACK.with(|s| {
        if let Some(top) = s.borrow_mut().last_mut() {
            top.insert(tag);
        }
    });
}

/// Run `f` with a fresh collector and return its result plus the tags it
/// recorded. Nested calls get their own collector, and their tags are also
/// added to the enclosing one (a fragment's dependencies are the page's too).
///
/// Collection is synchronous and per thread: wrap it around a synchronous
/// render (minijinja renders synchronously; async callers should render in
/// `spawn_blocking`), never across an `.await`.
pub fn collect<T>(f: impl FnOnce() -> T) -> (T, TagCollector) {
    /// Pops this level even if `f` panics, so the stack stays balanced.
    struct Level {
        depth: usize,
    }
    impl Level {
        fn pop(&self) -> TagCollector {
            STACK.with(|s| {
                let mut s = s.borrow_mut();
                if s.len() != self.depth {
                    return TagCollector::new();
                }
                let popped = s.pop().unwrap_or_default();
                if let Some(parent) = s.last_mut() {
                    parent.tags.extend(popped.tags.iter().cloned());
                }
                popped
            })
        }
    }
    impl Drop for Level {
        fn drop(&mut self) {
            self.pop();
        }
    }

    let depth = STACK.with(|s| {
        let mut s = s.borrow_mut();
        s.push(TagCollector::new());
        s.len()
    });
    let level = Level { depth };
    let out = f();
    let tags = level.pop();
    // `level` drops here; the stack is already shorter, so its drop is a no-op.
    (out, tags)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_and_parse_roundtrip() {
        for (tag, text) in [
            (CacheTag::item("blog", "hello"), "i:blog:hello"),
            (CacheTag::collection("blog"), "c:blog"),
            (CacheTag::singleton("site"), "s:site"),
        ] {
            assert_eq!(tag.to_string(), text);
            assert_eq!(text.parse::<CacheTag>().unwrap(), tag);
        }
        assert_eq!(
            "i:docs:a/b".parse::<CacheTag>().unwrap(),
            CacheTag::item("docs", "a/b")
        );
        for bad in ["", "x", "i:", "i:blog", "c:", "c:a:b", "q:x", "i::x"] {
            assert!(bad.parse::<CacheTag>().is_err(), "{bad}");
        }
    }

    #[test]
    fn collect_records_and_dedups() {
        let (v, tags) = collect(|| {
            record(CacheTag::singleton("site"));
            record(CacheTag::item("page", "about"));
            record(CacheTag::singleton("site"));
            42
        });
        assert_eq!(v, 42);
        assert_eq!(tags.len(), 2);
        assert!(tags.contains(&CacheTag::item("page", "about")));
    }

    #[test]
    fn record_outside_collect_is_noop() {
        record(CacheTag::collection("x"));
        let ((), tags) = collect(|| {});
        assert!(tags.is_empty());
    }

    #[test]
    fn nested_collectors_propagate_to_parent() {
        let (((), inner), outer) = collect(|| {
            record(CacheTag::singleton("site"));
            collect(|| record(CacheTag::collection("blog")))
        });
        assert_eq!(inner.len(), 1);
        assert!(outer.contains(&CacheTag::collection("blog")));
        assert!(outer.contains(&CacheTag::singleton("site")));
    }

    #[test]
    fn panic_inside_collect_keeps_stack_balanced() {
        let r = std::panic::catch_unwind(|| {
            collect(|| {
                record(CacheTag::singleton("x"));
                panic!("render failed");
            })
        });
        assert!(r.is_err());
        STACK.with(|s| assert!(s.borrow().is_empty()));
        let ((), tags) = collect(|| record(CacheTag::singleton("y")));
        assert_eq!(tags.into_vec(), vec![CacheTag::singleton("y")]);
    }

    #[test]
    fn serializes_as_strings() {
        let mut c = TagCollector::new();
        c.insert(CacheTag::collection("blog"));
        assert_eq!(
            serde_json::to_string(&c.into_vec()).unwrap(),
            r#"["c:blog"]"#
        );
    }
}
