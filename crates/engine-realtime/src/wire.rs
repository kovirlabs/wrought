//! Events between engine processes: `NOTIFY engine_realtime`, one JSON
//! object per notification.
//!
//! PostgreSQL delivers a notification to every session that listens on the
//! channel, in commit order, and refuses payloads of 8000 bytes or more, so
//! a long tag list is split over several notifications and a message's
//! payload is limited to [`MAX_PAYLOAD`] bytes.

use anyhow::{Result, bail};
use engine_core::tags::CacheTag;
use serde::{Deserialize, Serialize};

use crate::hub::Kind;

/// The largest JSON payload of one message ([`crate::Hub::message`]).
pub const MAX_PAYLOAD: usize = 4096;
/// Tags in one message (more: split, or a reset for a change).
pub const MAX_TAGS: usize = 256;
/// Under PostgreSQL's 8000-byte limit, with room for the channel name.
const MAX_NOTIFICATION: usize = 7900;

engine_db::statements! {
    NOTIFY = "SELECT pg_notify($1, $2)";
}

/// One notification.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Wire {
    /// The sending process ([`crate::Hub`]'s origin; empty from
    /// [`crate::notify`] outside a server).
    pub(crate) o: String,
    pub(crate) k: Kind,
    pub(crate) t: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) p: Option<serde_json::Value>,
}

/// `tags` checked and written the canonical way (`c:post`, `i:post:3`).
pub fn check_tags<S: AsRef<str>>(tags: &[S]) -> Result<Vec<String>> {
    let mut out = Vec::with_capacity(tags.len());
    for t in tags {
        let t = t.as_ref();
        if t.len() > 200 {
            bail!(
                "live tag `{}...` is longer than 200 characters",
                t.chars().take(40).collect::<String>()
            );
        }
        let parsed: CacheTag = t.parse()?;
        out.push(parsed.to_string());
    }
    Ok(out)
}

/// The notifications that carry one event.
pub(crate) fn encode(
    origin: &str,
    kind: Kind,
    tags: &[String],
    payload: Option<&serde_json::Value>,
) -> Result<Vec<String>> {
    let p = payload.map(serde_json::to_string).transpose()?;
    if p.as_ref().is_some_and(|p| p.len() > MAX_PAYLOAD) {
        bail!("a live message's payload is at most {MAX_PAYLOAD} bytes of JSON");
    }
    let empty = serde_json::to_string(&Wire {
        o: origin.into(),
        k: kind,
        t: Vec::new(),
        p: payload.cloned(),
    })?;
    let mut out = Vec::new();
    let mut chunk: Vec<String> = Vec::new();
    let mut size = empty.len();
    for t in tags {
        // A quoted string and a comma.
        let add = t.len() + 3;
        if !chunk.is_empty() && size + add > MAX_NOTIFICATION {
            out.push(std::mem::take(&mut chunk));
            size = empty.len();
        }
        size += add;
        chunk.push(t.clone());
    }
    if !chunk.is_empty() || kind == Kind::Reset {
        out.push(chunk);
    }
    out.into_iter()
        .map(|t| {
            Ok(serde_json::to_string(&Wire {
                o: origin.into(),
                k: kind,
                t,
                p: payload.cloned(),
            })?)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_tag_lists_are_split_under_the_limit() {
        let tags: Vec<String> = (0..2000).map(|i| format!("i:post:{i}")).collect();
        let n = encode("me", Kind::Change, &tags, None).unwrap();
        assert!(n.len() > 1);
        let mut back = Vec::new();
        for m in &n {
            assert!(m.len() <= MAX_NOTIFICATION, "{}", m.len());
            let w: Wire = serde_json::from_str(m).unwrap();
            back.extend(w.t);
        }
        assert_eq!(back, tags);
        let big = serde_json::json!({"x": "y".repeat(MAX_PAYLOAD)});
        assert!(encode("me", Kind::Message, &tags[..1], Some(&big)).is_err());
    }

    #[test]
    fn tags_are_checked() {
        assert_eq!(
            check_tags(&["c:post", "i:note:3"]).unwrap(),
            ["c:post", "i:note:3"]
        );
        assert!(check_tags(&["nonsense"]).is_err());
        assert!(check_tags(&[format!("c:{}", "x".repeat(300))]).is_err());
    }
}
