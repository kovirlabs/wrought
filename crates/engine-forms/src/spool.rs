//! The form spool: where a public form post goes when the database cannot
//! take it (asleep, restarting, down), so the visitor still gets the normal
//! answer and nothing is lost (DECISIONS.md, "Survive a sleeping
//! database").
//!
//! * A post is written to the spool only when the database is unavailable
//!   (or the spool still holds older posts, to keep the order): the normal
//!   path writes straight to the database with no extra disk write.
//! * Each post is one file, `<seq>-<key>.json`, mode 0600 in a 0700
//!   folder: written to a temporary name, `fsync`ed, renamed, and the
//!   folder `fsync`ed, all before the visitor is answered. A crash leaves
//!   either no file or a whole one (temporary files are removed on start).
//! * `seq` orders posts as they arrived; `key` (random, per post) makes the
//!   replay exactly once: the replay claims the key in the database in the
//!   same transaction as the submission and its effects (queued mail), and
//!   deletes the file only after that commits. A crash between commit and
//!   delete replays the file again, finds the key claimed and only deletes
//!   it.
//! * The spool is bounded in bytes ([`engine_core::limits`], `[limits]
//!   form_spool_mb`); a post that does not fit is refused with a clear
//!   message. One client may hold at most [`client_share`] of it, so a
//!   flood from one address cannot keep everyone else's posts out.
//! * A post the database refuses (not "unavailable": a constraint, bad
//!   data) is tried [`MAX_TRIES`] times, then moved to `quarantine/` (still
//!   0600, still counted against the limit, reported by `engine doctor`
//!   and the health endpoint, logged) so the posts after it go on. Order
//!   holds for the posts that are not set aside.

use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// The environment variable that moves the spool (default:
/// `<site>/.engine/spool/forms`).
pub const SPOOL_DIR_ENV: &str = "ENGINE_FORM_SPOOL_DIR";

/// One spooled post: everything the replay needs to store it and run its
/// effects as if it had just arrived.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Spooled {
    /// Idempotency key (random, 32 hex digits).
    pub key: String,
    pub form: String,
    pub data: serde_json::Value,
    pub unique: Option<String>,
    pub user_id: Option<i64>,
    pub visitor_name: Option<String>,
    pub visitor_email: Option<String>,
    pub ip: Option<String>,
    pub user_agent: String,
    /// When the visitor sent it (UTC, ISO 8601).
    pub created_at: String,
    /// Whether the form stores submissions (`store = true`).
    pub store: bool,
}

/// Replays of one post the database refused before it is set aside.
pub const MAX_TRIES: u32 = 3;

/// The most one client may hold: 1/16 of the spool, at least 64 KiB.
pub fn client_share(limit: u64) -> u64 {
    (limit / 16).max(64 * 1024).min(limit)
}

#[derive(Debug, thiserror::Error)]
pub enum SpoolError {
    #[error("the form spool is full ({used} of {limit} bytes)")]
    Full { used: u64, limit: u64 },
    #[error("this client's share of the form spool is used up ({used} of {share} bytes)")]
    ClientFull { used: u64, share: u64 },
    #[error(transparent)]
    Io(#[from] anyhow::Error),
}

/// A spooled file, in order.
#[derive(Debug, Clone)]
pub struct Entry {
    pub path: PathBuf,
    pub bytes: u64,
}

#[derive(Debug)]
pub struct Spool {
    dir: PathBuf,
    limit: u64,
    used: AtomicU64,
    files: AtomicU64,
    quarantined: AtomicU64,
    next: AtomicU64,
    /// Bytes waiting per client (address), for [`client_share`]. Starts
    /// empty after a restart (the total limit still holds).
    clients: std::sync::Mutex<std::collections::HashMap<String, u64>>,
    /// Failed replays per waiting file.
    tries: std::sync::Mutex<std::collections::HashMap<PathBuf, u32>>,
    /// One replay at a time.
    pub(crate) replaying: tokio::sync::Mutex<()>,
    /// Wakes the replay when a post is added.
    added: tokio::sync::Notify,
}

/// A new idempotency key.
pub fn new_key() -> String {
    engine_core::web::secret::random_bytes(16)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

impl Spool {
    /// Open (or create) the spool in `dir`, holding at most `limit` bytes.
    /// Removes half-written files and counts what is waiting.
    pub fn open(dir: &Path, limit: u64) -> Result<Spool> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .with_context(|| format!("creating the form spool {}", dir.display()))?;
        let mut used = 0;
        let mut files = 0;
        let mut next = 0;
        let mut quarantined = 0;
        let qdir = dir.join("quarantine");
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&qdir)
            .with_context(|| format!("creating {}", qdir.display()))?;
        for e in fs::read_dir(&qdir)? {
            let e = e?;
            if e.file_type()?.is_file() {
                used += e.metadata()?.len();
                quarantined += 1;
            }
        }
        for e in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
            let e = e?;
            let name = e.file_name().to_string_lossy().into_owned();
            if name.ends_with(".tmp") {
                let _ = fs::remove_file(e.path());
                continue;
            }
            if let Some(seq) = seq_of(&name) {
                used += e.metadata()?.len();
                files += 1;
                next = next.max(seq + 1);
            }
        }
        Ok(Spool {
            dir: dir.to_path_buf(),
            limit,
            used: AtomicU64::new(used),
            files: AtomicU64::new(files),
            quarantined: AtomicU64::new(quarantined),
            next: AtomicU64::new(next),
            clients: Default::default(),
            tries: Default::default(),
            replaying: tokio::sync::Mutex::new(()),
            added: tokio::sync::Notify::new(),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn limit(&self) -> u64 {
        self.limit
    }

    /// Bytes held.
    pub fn used(&self) -> u64 {
        self.used.load(Ordering::Relaxed)
    }

    /// Posts waiting.
    pub fn len(&self) -> u64 {
        self.files.load(Ordering::Relaxed)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Posts set aside because the database refused them.
    pub fn quarantined(&self) -> u64 {
        self.quarantined.load(Ordering::Relaxed)
    }

    /// Where refused posts are set aside.
    pub fn quarantine_dir(&self) -> PathBuf {
        self.dir.join("quarantine")
    }

    /// Write `post` from `client` durably (blocking: file and folder
    /// `fsync`). Returns once it is on disk.
    pub fn push(&self, post: &Spooled, client: &str) -> Result<(), SpoolError> {
        let bytes = serde_json::to_vec(post).context("encoding a spooled post")?;
        let len = bytes.len() as u64;
        let share = client_share(self.limit);
        {
            let mut c = self.clients.lock().unwrap_or_else(|e| e.into_inner());
            let mine = c.get(client).copied().unwrap_or(0);
            if mine + len > share {
                return Err(SpoolError::ClientFull { used: mine, share });
            }
            *c.entry(client.to_string()).or_insert(0) += len;
        }
        let r = self.push_bytes(post, &bytes, len);
        if r.is_err() {
            self.release_client(client, len);
        }
        r
    }

    fn release_client(&self, client: &str, len: u64) {
        let mut c = self.clients.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = c.get_mut(client) {
            *n = n.saturating_sub(len);
            if *n == 0 {
                c.remove(client);
            }
        }
    }

    fn push_bytes(&self, post: &Spooled, bytes: &[u8], len: u64) -> Result<(), SpoolError> {
        // Reserve the space first, so posts at once cannot overshoot.
        let mut used = self.used.load(Ordering::Relaxed);
        loop {
            if used + len > self.limit {
                return Err(SpoolError::Full {
                    used,
                    limit: self.limit,
                });
            }
            match self.used.compare_exchange_weak(
                used,
                used + len,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(now) => used = now,
            }
        }
        let r = self.write(post, bytes);
        if r.is_err() {
            self.used.fetch_sub(len, Ordering::AcqRel);
        }
        r.map_err(SpoolError::Io)
    }

    fn write(&self, post: &Spooled, bytes: &[u8]) -> Result<()> {
        let seq = self.next.fetch_add(1, Ordering::AcqRel);
        let name = format!("{seq:020}-{}.json", post.key);
        let tmp = self.dir.join(format!("{name}.tmp"));
        let path = self.dir.join(&name);
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
            .with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(bytes)
            .and_then(|_| f.sync_all())
            .with_context(|| format!("writing {}", tmp.display()))?;
        drop(f);
        fs::rename(&tmp, &path).with_context(|| format!("naming {}", path.display()))?;
        sync_dir(&self.dir)?;
        self.files.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    /// Wake the replay (a post was added).
    pub fn kick(&self) {
        self.added.notify_one();
    }

    /// Wait for [`Spool::kick`].
    pub async fn kicked(&self) {
        self.added.notified().await
    }

    /// The waiting posts, oldest first.
    pub fn entries(&self) -> Result<Vec<Entry>> {
        let mut v: Vec<(u64, Entry)> = Vec::new();
        for e in fs::read_dir(&self.dir)? {
            let e = e?;
            let name = e.file_name().to_string_lossy().into_owned();
            if let Some(seq) = seq_of(&name) {
                v.push((
                    seq,
                    Entry {
                        path: e.path(),
                        bytes: e.metadata()?.len(),
                    },
                ));
            }
        }
        v.sort_by_key(|(s, _)| *s);
        Ok(v.into_iter().map(|(_, e)| e).collect())
    }

    pub fn read(&self, e: &Entry) -> Result<Spooled> {
        let b = fs::read(&e.path).with_context(|| format!("reading {}", e.path.display()))?;
        serde_json::from_slice(&b).with_context(|| format!("decoding {}", e.path.display()))
    }

    /// Remove a replayed post of `client`.
    pub fn remove(&self, e: &Entry, client: &str) -> Result<()> {
        fs::remove_file(&e.path).with_context(|| format!("removing {}", e.path.display()))?;
        self.used
            .fetch_sub(e.bytes.min(self.used()), Ordering::AcqRel);
        self.files.fetch_sub(1, Ordering::AcqRel);
        self.release_client(client, e.bytes);
        self.tries
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&e.path);
        // A lost unlink only means the post is found claimed next time.
        let _ = sync_dir(&self.dir);
        Ok(())
    }

    /// Count a replay of `e` the database refused; returns the tries so
    /// far.
    pub fn failed(&self, e: &Entry) -> u32 {
        let mut t = self.tries.lock().unwrap_or_else(|p| p.into_inner());
        let n = t.entry(e.path.clone()).or_insert(0);
        *n += 1;
        *n
    }

    /// Set a post aside in `quarantine/` (refused by the database, or not
    /// decodable): kept for a person to look at, still counted against the
    /// limit, no longer in the way of the posts after it.
    pub fn quarantine(&self, e: &Entry, client: Option<&str>) -> Result<PathBuf> {
        let to = self
            .quarantine_dir()
            .join(e.path.file_name().unwrap_or_default());
        fs::rename(&e.path, &to).with_context(|| format!("moving {}", e.path.display()))?;
        let _ = sync_dir(&self.dir);
        self.files.fetch_sub(1, Ordering::AcqRel);
        self.quarantined.fetch_add(1, Ordering::AcqRel);
        if let Some(c) = client {
            self.release_client(c, e.bytes);
        }
        self.tries
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&e.path);
        Ok(to)
    }
}

fn seq_of(name: &str) -> Option<u64> {
    let stem = name.strip_suffix(".json")?;
    let (seq, key) = stem.split_once('-')?;
    (seq.len() == 20 && key.len() == 32).then_some(())?;
    seq.parse().ok()
}

fn sync_dir(dir: &Path) -> Result<()> {
    File::open(dir)
        .and_then(|d| d.sync_all())
        .with_context(|| format!("syncing {}", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn post(n: u32) -> Spooled {
        Spooled {
            key: new_key(),
            form: "contact".into(),
            data: serde_json::json!({"message": format!("hello {n}")}),
            unique: None,
            user_id: None,
            visitor_name: None,
            visitor_email: None,
            ip: Some("192.0.2.1".into()),
            user_agent: "test".into(),
            created_at: "2026-11-27T12:00:00Z".into(),
            store: true,
        }
    }

    #[test]
    fn order_privacy_and_bound() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("spool");
        let s = Spool::open(&dir, 2_000).unwrap();
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let posts: Vec<Spooled> = (0..5).map(post).collect();
        for (i, p) in posts.iter().enumerate() {
            s.push(p, &format!("client {i}")).unwrap();
        }
        assert_eq!(s.len(), 5);
        let es = s.entries().unwrap();
        for (e, p) in es.iter().zip(&posts) {
            assert_eq!(&s.read(e).unwrap(), p);
            assert_eq!(
                fs::metadata(&e.path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        // Full: refused, nothing written, the count unchanged.
        let mut big = post(9);
        big.data = serde_json::json!({"message": "x".repeat(2_000)});
        assert!(matches!(
            s.push(&big, "client 9"),
            Err(SpoolError::Full { .. }) | Err(SpoolError::ClientFull { .. })
        ));
        assert_eq!(s.entries().unwrap().len(), 5);
        // Reopened (a restart): same posts, same order, a half-written
        // file dropped, new posts after the old ones.
        fs::write(dir.join("00000000000000000099-x.json.tmp"), "{").unwrap();
        let used = s.used();
        drop(s);
        let s = Spool::open(&dir, 2_000).unwrap();
        assert_eq!((s.len(), s.used()), (5, used));
        assert!(!dir.join("00000000000000000099-x.json.tmp").exists());
        let late = post(6);
        s.push(&late, "client 6").unwrap();
        let es = s.entries().unwrap();
        assert_eq!(s.read(es.last().unwrap()).unwrap(), late);
        // One refused post set aside: still counted, out of the way.
        assert_eq!(s.failed(&es[0]), 1);
        let q = s.quarantine(&es[0], Some("client 0")).unwrap();
        assert_eq!(
            fs::metadata(&q).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!((s.len(), s.quarantined()), (5, 1));
        for e in &es[1..] {
            s.remove(e, "x").unwrap();
        }
        assert!(s.is_empty());
        assert_eq!(s.used(), es[0].bytes, "the quarantined post still counts");
        drop(s);
        let s = Spool::open(&dir, 2_000).unwrap();
        assert_eq!((s.len(), s.quarantined(), s.used()), (0, 1, es[0].bytes));
    }

    #[test]
    fn one_client_gets_a_share() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Spool::open(tmp.path(), 1 << 20).unwrap();
        assert_eq!(client_share(1 << 20), 64 * 1024);
        let mut p = post(0);
        p.data = serde_json::json!({"message": "x".repeat(4000)});
        let mut n = 0;
        while s.push(&p, "203.0.113.7").is_ok() {
            n += 1;
        }
        assert!((14..=16).contains(&n), "{n} posts in 64 KiB");
        assert!(matches!(
            s.push(&p, "203.0.113.7"),
            Err(SpoolError::ClientFull { .. })
        ));
        s.push(&p, "198.51.100.2").unwrap();
    }
}
