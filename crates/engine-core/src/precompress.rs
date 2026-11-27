//! Pre-compressed copies of published text files.
//!
//! Every text file the engine writes (pages, stylesheets, scripts, SVG,
//! feeds, sitemaps, JSON, plain text) of at least [`MIN_BYTES`] gets a
//! Brotli copy (`page.html.br`) and a gzip copy (`page.html.gz`) beside it,
//! written with the file itself, so the front door (Caddy's `precompressed
//! br gzip`, the engine's own server) sends them without compressing
//! anything per request. A copy that would not be at least
//! [`MIN_SAVING_PERCENT`] smaller is not written.
//!
//! **The copies are never stale.** A writer writes the copies' bytes before
//! the file's, and then moves them into place before the file: so a
//! complete write leaves every copy no newer than its file, and every
//! rewrite or removal of a file rewrites or removes its copies first. A
//! server that checks freshness (the engine's) serves a copy only if it is
//! a regular file no newer than the original; a write cut short (a crash
//! between the renames) leaves copies newer than the file, which are then
//! ignored until the next write of that file heals them.
//!
//! Levels were chosen by measurement on the 1000-article benchmark build
//! (DECISIONS.md, 2026-11-26): Brotli [`BROTLI_QUALITY`] and gzip
//! [`GZIP_LEVEL`].

use std::io::Write;
use std::path::{Path, PathBuf};

/// Brotli quality (0-11). Measured: see the module docs.
pub const BROTLI_QUALITY: u32 = 5;
/// Brotli window (log2 bytes); 22 (4 MiB) is the format's usual default.
const BROTLI_WINDOW: u32 = 22;
/// gzip level (0-9).
pub const GZIP_LEVEL: u32 = 6;
/// Files smaller than this are not compressed: the copy would save less
/// than a packet. (Caddy's `encode` uses the same threshold.)
pub const MIN_BYTES: usize = 512;
/// A copy must be at least this much smaller than the file to be kept.
pub const MIN_SAVING_PERCENT: usize = 5;

/// One encoding: the `Content-Encoding` token and the file suffix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Br,
    Gzip,
}

impl Encoding {
    /// Preferred first.
    pub const ALL: [Encoding; 2] = [Encoding::Br, Encoding::Gzip];

    pub fn token(self) -> &'static str {
        match self {
            Encoding::Br => "br",
            Encoding::Gzip => "gzip",
        }
    }

    pub fn suffix(self) -> &'static str {
        match self {
            Encoding::Br => ".br",
            Encoding::Gzip => ".gz",
        }
    }

    /// The copy of `path` in this encoding (`a/index.html` → `a/index.html.br`).
    pub fn sidecar(self, path: &Path) -> PathBuf {
        let mut s = path.as_os_str().to_owned();
        s.push(self.suffix());
        PathBuf::from(s)
    }

    pub fn encode(self, body: &[u8]) -> Vec<u8> {
        match self {
            Encoding::Br => {
                let mut out = Vec::with_capacity(body.len() / 3 + 64);
                {
                    let mut w = brotli::CompressorWriter::new(
                        &mut out,
                        64 * 1024,
                        BROTLI_QUALITY,
                        BROTLI_WINDOW,
                    );
                    w.write_all(body).expect("writing to memory");
                }
                out
            }
            Encoding::Gzip => {
                let mut w = flate2::write::GzEncoder::new(
                    Vec::with_capacity(body.len() / 3 + 64),
                    flate2::Compression::new(GZIP_LEVEL),
                );
                w.write_all(body).expect("writing to memory");
                w.finish().expect("writing to memory")
            }
        }
    }
}

/// Whether files at `path` are text worth pre-compressing (by extension).
pub fn compressible(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some(
            "html"
                | "htm"
                | "css"
                | "js"
                | "mjs"
                | "svg"
                | "xml"
                | "rss"
                | "atom"
                | "json"
                | "map"
                | "webmanifest"
                | "txt"
                | "md"
                | "csv"
                | "ics"
        )
    )
}

/// Whether `rel` is itself a pre-compressed copy (a `.br` or `.gz` file
/// whose stem is a compressible file).
pub fn is_sidecar(path: &Path) -> bool {
    let s = path.as_os_str().to_string_lossy();
    Encoding::ALL.iter().any(|e| {
        s.strip_suffix(e.suffix())
            .is_some_and(|stem| compressible(Path::new(stem)))
    })
}

/// The copies to write for `body` at `path`: each encoding's bytes, or
/// `None` where no copy should exist (not text, too small, no gain).
pub fn encode_all(path: &Path, body: &[u8]) -> [(Encoding, Option<Vec<u8>>); 2] {
    let wanted = compressible(path) && body.len() >= MIN_BYTES;
    Encoding::ALL.map(|e| {
        let copy = wanted
            .then(|| e.encode(body))
            .filter(|c| c.len() * 100 <= body.len() * (100 - MIN_SAVING_PERCENT));
        (e, copy)
    })
}

/// Write `body` to `path` and its copies beside it, for a folder that no
/// server reads yet (a build's staging folder): copies first, then the
/// file, so copies are never newer than the file. `write` writes one file
/// (and may refuse to overwrite).
pub fn write_new_with_copies(
    path: &Path,
    body: &[u8],
    mut write: impl FnMut(&Path, &[u8]) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    for (e, copy) in encode_all(path, body) {
        if let Some(c) = copy {
            write(&e.sidecar(path), &c)?;
        }
    }
    write(path, body)
}

/// Whether the copy at `sidecar` may be served for the file whose metadata
/// is `original`: a regular file (not a symlink), no newer than the file.
pub fn fresh(original: &std::fs::Metadata, sidecar: &Path) -> Option<std::fs::Metadata> {
    let m = std::fs::symlink_metadata(sidecar).ok()?;
    if !m.file_type().is_file() {
        return None;
    }
    match (m.modified(), original.modified()) {
        (Ok(copy), Ok(orig)) if copy <= orig => Some(m),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn copies_round_trip_and_small_files_are_skipped() {
        let page = "<p>Wrought iron, worked by hand.</p>\n".repeat(40);
        let [(_, br), (_, gz)] = encode_all(Path::new("a/index.html"), page.as_bytes());
        let (br, gz) = (br.unwrap(), gz.unwrap());
        assert!(br.len() < page.len() / 5 && gz.len() < page.len() / 5);
        let mut out = String::new();
        brotli::Decompressor::new(&br[..], 4096)
            .read_to_string(&mut out)
            .unwrap();
        assert_eq!(out, page);
        out.clear();
        flate2::read::GzDecoder::new(&gz[..])
            .read_to_string(&mut out)
            .unwrap();
        assert_eq!(out, page);

        let small = encode_all(Path::new("a/index.html"), b"<p>hi</p>");
        assert!(small.iter().all(|(_, c)| c.is_none()));
        let png = encode_all(Path::new("a.png"), page.as_bytes());
        assert!(png.iter().all(|(_, c)| c.is_none()), "not text");
        // Random bytes do not compress: no copy.
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let noise: Vec<u8> = (0..4096)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect();
        assert!(
            encode_all(Path::new("x.txt"), &noise)
                .iter()
                .all(|(_, c)| c.is_none())
        );
    }

    #[test]
    fn names() {
        let p = Path::new("articles/x/index.html");
        assert_eq!(
            Encoding::Br.sidecar(p),
            PathBuf::from("articles/x/index.html.br")
        );
        assert!(is_sidecar(Path::new("a/index.html.gz")));
        assert!(is_sidecar(Path::new("feed.xml.br")));
        assert!(!is_sidecar(Path::new("archive.tar.gz")));
        assert!(!is_sidecar(Path::new("index.html")));
        assert!(compressible(Path::new("STYLE.CSS")));
        assert!(!compressible(Path::new("photo.jpg")));
    }
}
