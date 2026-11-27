//! Template values of items, made once and reused while the item is
//! unchanged.
//!
//! Making an item's value renders and sanitises every Markdown field: for a
//! 2.6 KB article about 0.1 ms, which a request-rendered page paid for the
//! article itself and again for each related article it lists. The cache
//! keeps the value beside the item it was made from (as the database
//! returned it, terms included) and hands it out only for an equal item, so
//! it can never serve a stale value: an edit, a publish or a renamed term
//! changes the item the page reads, and the next read makes a new value.
//! Nothing has to be evicted on change; old entries just stop matching.
//!
//! Bounded in bytes (an estimate of each entry's size): two generations of
//! entries, the older dropped whole when the newer reaches half the budget,
//! so the most recently used items stay and the total never passes the
//! budget. A budget of 0 turns the cache off.

use std::collections::HashMap;
use std::sync::Mutex;

use minijinja::Value;
use serde_json::Value as Json;

use crate::store::Item;

struct Entry {
    item: Item,
    value: Value,
    bytes: usize,
}

#[derive(Default)]
struct Generations {
    current: HashMap<(String, i64), Entry>,
    previous: HashMap<(String, i64), Entry>,
    current_bytes: usize,
}

/// See the module documentation.
pub struct ValueCache {
    budget: usize,
    inner: Mutex<Generations>,
}

/// About how much memory an entry holds: the item's text twice (as stored
/// and as rendered) plus fixed overheads per field.
fn estimate(item: &Item) -> usize {
    fn json(v: &Json) -> usize {
        match v {
            Json::String(s) => s.len() + 24,
            Json::Array(a) => a.iter().map(json).sum::<usize>() + 24,
            Json::Object(m) => m.iter().map(|(k, v)| k.len() + json(v) + 48).sum::<usize>() + 24,
            _ => 16,
        }
    }
    let fields: usize = item
        .fields
        .iter()
        .map(|(k, v)| k.len() + json(v) + 48)
        .sum();
    256 + item.slug.len() + item.content_type.len() + 2 * fields
}

impl ValueCache {
    pub fn new(budget_bytes: usize) -> Self {
        ValueCache {
            budget: budget_bytes,
            inner: Mutex::new(Generations::default()),
        }
    }

    pub fn budget(&self) -> usize {
        self.budget
    }

    /// The value of `item`, made by `make` unless an equal item's value is
    /// kept.
    pub fn get_or_make(&self, item: &Item, make: impl FnOnce() -> Value) -> Value {
        if self.budget == 0 {
            return make();
        }
        let key = (item.content_type.clone(), item.id);
        {
            let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(e) = g.current.get(&key)
                && e.item == *item
            {
                return e.value.clone();
            }
            // Found in the older generation: move it to the newer one.
            if let Some(e) = g.previous.remove(&key)
                && e.item == *item
            {
                let v = e.value.clone();
                Self::put(&mut g, self.budget, key, e);
                return v;
            }
        }
        let value = make();
        let bytes = estimate(item);
        if bytes <= self.budget / 2 {
            let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            Self::put(
                &mut g,
                self.budget,
                key,
                Entry {
                    item: item.clone(),
                    value: value.clone(),
                    bytes,
                },
            );
        }
        value
    }

    fn put(g: &mut Generations, budget: usize, key: (String, i64), e: Entry) {
        if g.current_bytes + e.bytes > budget / 2 {
            g.previous = std::mem::take(&mut g.current);
            g.current_bytes = 0;
        }
        g.current_bytes += e.bytes;
        if let Some(old) = g.current.insert(key, e) {
            g.current_bytes -= old.bytes;
        }
    }

    /// Entries and the bytes they are estimated to hold.
    pub fn usage(&self) -> (usize, usize) {
        let g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let prev: usize = g.previous.values().map(|e| e.bytes).sum();
        (g.current.len() + g.previous.len(), g.current_bytes + prev)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Status;
    use std::cell::Cell;

    fn item(id: i64, body: &str) -> Item {
        let mut fields = serde_json::Map::new();
        fields.insert("body".into(), Json::String(body.into()));
        Item {
            content_type: "article".into(),
            id,
            slug: format!("a-{id}"),
            status: Status::Published,
            version: 1,
            created_at: String::new(),
            updated_at: String::new(),
            published_at: None,
            fields,
        }
    }

    #[test]
    fn equal_items_reuse_their_value_changed_ones_do_not() {
        let c = ValueCache::new(1 << 20);
        let made = Cell::new(0);
        let make = |s: &str| {
            made.set(made.get() + 1);
            Value::from(s.to_string())
        };
        let a = item(1, "one");
        assert_eq!(c.get_or_make(&a, || make("v1")).as_str(), Some("v1"));
        assert_eq!(c.get_or_make(&a, || make("again")).as_str(), Some("v1"));
        assert_eq!(made.get(), 1);
        // An edit (or a renamed term: any difference in the item) misses.
        let mut b = a.clone();
        b.version = 2;
        assert_eq!(c.get_or_make(&b, || make("v2")).as_str(), Some("v2"));
        let mut t = b.clone();
        t.fields.insert(
            "topics".into(),
            serde_json::json!([{"id": 1, "name": "New"}]),
        );
        assert_eq!(c.get_or_make(&t, || make("v3")).as_str(), Some("v3"));
        assert_eq!(made.get(), 3);
    }

    #[test]
    fn stays_within_its_budget() {
        let budget = 64 * 1024;
        let c = ValueCache::new(budget);
        let body = "x".repeat(1000);
        for i in 0..500 {
            c.get_or_make(&item(i, &body), || Value::from(i));
            assert!(c.usage().1 <= budget, "{:?}", c.usage());
        }
        // The most recent ones are kept.
        let made = Cell::new(false);
        c.get_or_make(&item(499, &body), || {
            made.set(true);
            Value::from(0)
        });
        assert!(!made.get());
        // Off.
        let off = ValueCache::new(0);
        off.get_or_make(&item(1, "a"), || Value::from(1));
        assert_eq!(off.usage(), (0, 0));
    }
}
