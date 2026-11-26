//! Stored media: the table rows and the files on disk.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use engine_db::{Db, Timestamp};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::process::{self, ProcessError};
use crate::sniff::{self, Kind};
use crate::storage::{Public, PublicStorage};
use crate::{MediaConfig, sql};

/// A stored file.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Media {
    pub id: i64,
    /// File stem, e.g. `harbour-at-dawn`.
    pub name: String,
    /// `jpg`, `png`, `gif`, `webp` or `pdf`.
    pub ext: String,
    pub mime: String,
    pub original_name: String,
    pub bytes: i64,
    pub width: Option<i32>,
    pub height: Option<i32>,
    /// Widths of the resized copies.
    pub variants: Vec<u32>,
    pub sha256: String,
    pub alt: String,
    pub private: bool,
    pub uploaded_by: Option<i64>,
    pub version: i32,
    pub created_at: String,
    pub updated_at: String,
}

impl Media {
    pub fn is_image(&self) -> bool {
        self.mime.starts_with("image/")
    }

    pub fn kind(&self) -> Option<Kind> {
        Kind::from_ext(&self.ext)
    }

    pub fn file_name(&self) -> String {
        format!("{}.{}", self.name, self.ext)
    }

    /// File name of the copy `width` pixels wide.
    pub fn variant_file(&self, width: u32) -> String {
        let ext = self
            .kind()
            .map(|k| process::variant_kind(k).ext())
            .unwrap_or("bin");
        format!("{}-{width}.{ext}", self.name)
    }

    /// Public URL (`None` for private files).
    pub fn url(&self) -> Option<String> {
        (!self.private).then(|| format!("/media/{}/{}", self.id, self.file_name()))
    }

    pub fn variant_url(&self, width: u32) -> Option<String> {
        (!self.private).then(|| format!("/media/{}/{}", self.id, self.variant_file(width)))
    }

    /// `srcset` for `<img>`: every copy plus the original.
    pub fn srcset(&self) -> Option<String> {
        if self.private || !self.is_image() {
            return None;
        }
        let mut parts: Vec<String> = self
            .variants
            .iter()
            .filter_map(|w| Some(format!("{} {w}w", self.variant_url(*w)?)))
            .collect();
        if let (Some(u), Some(w)) = (self.url(), self.width) {
            parts.push(format!("{u} {w}w"));
        }
        Some(parts.join(", "))
    }
}

/// What to store.
#[derive(Debug, Clone, Copy)]
pub struct Upload<'a> {
    pub bytes: &'a [u8],
    /// The name the sender gave (only used to make a readable file name).
    pub filename: &'a str,
    pub alt: &'a str,
    /// Private files (form attachments) are never served at `/media/`.
    pub private: bool,
    pub uploaded_by: Option<i64>,
}

/// Why an upload or change was refused.
#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("the file is larger than {max_mb} MB")]
    TooLarge { max_mb: u64 },
    #[error("{0}")]
    Refused(String),
    #[error("{0}")]
    Invalid(String),
    #[error("this file was changed by someone else (version {expected} edited, now {current})")]
    Conflict { expected: i32, current: i32 },
    #[error("no such file")]
    NotFound,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<sqlx::Error> for MediaError {
    fn from(e: sqlx::Error) -> Self {
        MediaError::Other(e.into())
    }
}

impl From<std::io::Error> for MediaError {
    fn from(e: std::io::Error) -> Self {
        MediaError::Other(e.into())
    }
}

#[derive(sqlx::FromRow)]
struct Row {
    id: i64,
    name: String,
    ext: String,
    mime: String,
    original_name: String,
    bytes: i64,
    width: Option<i32>,
    height: Option<i32>,
    variants: sqlx::types::Json<Vec<u32>>,
    sha256: String,
    alt: String,
    private: bool,
    uploaded_by: Option<i64>,
    version: i32,
    created_at: Timestamp,
    updated_at: Timestamp,
}

impl From<Row> for Media {
    fn from(r: Row) -> Media {
        Media {
            id: r.id,
            name: r.name,
            ext: r.ext,
            mime: r.mime,
            original_name: r.original_name,
            bytes: r.bytes,
            width: r.width,
            height: r.height,
            variants: r.variants.0,
            sha256: r.sha256,
            alt: r.alt,
            private: r.private,
            uploaded_by: r.uploaded_by,
            version: r.version,
            created_at: r.created_at.iso(),
            updated_at: r.updated_at.iso(),
        }
    }
}

/// A file stem from an uploaded name: lowercase ASCII letters and digits in
/// words joined by `-`, at most 60 characters; `file` if nothing is left.
pub fn safe_stem(filename: &str) -> String {
    // Only the last path segment, without its extension.
    let base = filename.rsplit(['/', '\\']).next().unwrap_or("");
    let stem = match base.rsplit_once('.') {
        Some((s, _)) if !s.is_empty() => s,
        _ => base,
    };
    let mut out = String::new();
    let mut dash = false;
    for c in stem.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if dash && !out.is_empty() {
                out.push('-');
            }
            dash = false;
            out.push(c);
            if out.len() >= 60 {
                break;
            }
        } else {
            dash = true;
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() { "file".into() } else { out }
}

/// The media library of one site. Cheap to clone.
#[derive(Clone, Debug)]
pub struct MediaStore {
    db: Db,
    cfg: Arc<MediaConfig>,
    dir: PathBuf,
    public: Public,
}

impl MediaStore {
    /// `dir` is the media folder (`MediaConfig::dir_for(site)`).
    pub fn new(db: Db, cfg: MediaConfig, dir: PathBuf) -> Self {
        MediaStore {
            db,
            cfg: Arc::new(cfg),
            dir,
            public: Public::default(),
        }
    }

    /// Also keep public files in `storage` (the `s3` feature's bucket).
    pub fn with_public_storage(mut self, storage: Arc<dyn PublicStorage>) -> Self {
        self.public = Public(Some(storage));
        self
    }

    pub fn public_storage(&self) -> Option<&Arc<dyn PublicStorage>> {
        self.public.0.as_ref()
    }

    /// `(rel, mime)` of a file and its copies, `rel` being `<id>/<file>`.
    fn files_of(m: &Media) -> Vec<(String, &'static str)> {
        let kind = m.kind();
        let mut v = vec![(
            format!("{}/{}", m.id, m.file_name()),
            kind.map(|k| k.mime()).unwrap_or("application/octet-stream"),
        )];
        for w in &m.variants {
            v.push((
                format!("{}/{}", m.id, m.variant_file(*w)),
                kind.map(|k| process::variant_kind(k).mime())
                    .unwrap_or("application/octet-stream"),
            ));
        }
        v
    }

    /// Copy a public file and its copies to the public storage; on failure
    /// remove what was copied.
    async fn copy_public(&self, m: &Media, p: &process::Processed) -> anyhow::Result<()> {
        let Some(st) = self.public.0.as_ref().filter(|_| !m.private) else {
            return Ok(());
        };
        let files = Self::files_of(m);
        let mut bodies: Vec<&[u8]> = vec![&p.original];
        bodies.extend(p.variants.iter().map(|(_, d)| d.as_slice()));
        let mut done: Vec<String> = Vec::new();
        for ((rel, mime), body) in files.iter().zip(bodies) {
            if let Err(e) = st.put(rel, body, mime).await {
                for r in &done {
                    let _ = st.delete(r).await;
                }
                return Err(e.context(format!("copying media to {}", st.describe())));
            }
            done.push(rel.clone());
        }
        Ok(())
    }
    pub fn config(&self) -> &MediaConfig {
        &self.cfg
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    /// Folder of public files (what `/media/` serves).
    pub fn public_dir(&self) -> PathBuf {
        self.dir.join("public")
    }

    /// Folder holding a file and its copies.
    pub fn folder(&self, m: &Media) -> PathBuf {
        self.dir
            .join(if m.private { "private" } else { "public" })
            .join(m.id.to_string())
    }

    /// Path of the original on disk.
    pub fn path(&self, m: &Media) -> PathBuf {
        self.folder(m).join(m.file_name())
    }

    /// Check, clean and store an upload.
    pub async fn upload(&self, up: Upload<'_>) -> Result<Media, MediaError> {
        if up.bytes.len() as u64 > self.cfg.max_bytes() {
            return Err(MediaError::TooLarge {
                max_mb: self.cfg.max_mb,
            });
        }
        let kind = sniff::sniff(up.bytes).map_err(|r| MediaError::Refused(r.to_string()))?;
        if up.alt.chars().count() > 500 {
            return Err(MediaError::Invalid(
                "the description is longer than 500 characters".into(),
            ));
        }
        let bytes = up.bytes.to_vec();
        let cfg = self.cfg.clone();
        let p = tokio::task::spawn_blocking(move || process::process(&bytes, kind, &cfg))
            .await
            .map_err(|e| MediaError::Other(e.into()))?
            .map_err(|e: ProcessError| MediaError::Refused(e.to_string()))?;
        let name = safe_stem(up.filename);
        let original_name: String = up
            .filename
            .chars()
            .filter(|c| !c.is_control())
            .take(255)
            .collect();
        let sha = format!("{:x}", Sha256::digest(&p.original));
        let widths: Vec<u32> = p.variants.iter().map(|v| v.0).collect();
        let mut tx = self.db.begin().await?;
        let id: i64 = sql::INSERT
            .bind(name.as_str())
            .bind(kind.ext())
            .bind(kind.mime())
            .bind(original_name.as_str())
            .bind(p.original.len() as i64)
            .bind(p.width.map(|w| w as i32))
            .bind(p.height.map(|h| h as i32))
            .bind(serde_json::json!(widths))
            .bind(sha.as_str())
            .bind(up.alt.trim())
            .bind(up.private)
            .bind(up.uploaded_by)
            .bind(Timestamp::now())
            .scalar(&mut tx)
            .await?;
        let probe = Media {
            id,
            name: name.clone(),
            ext: kind.ext().into(),
            mime: kind.mime().into(),
            original_name: String::new(),
            bytes: 0,
            width: None,
            height: None,
            variants: widths.clone(),
            sha256: String::new(),
            alt: String::new(),
            private: up.private,
            uploaded_by: None,
            version: 1,
            created_at: String::new(),
            updated_at: String::new(),
        };
        let folder = self.folder(&probe);
        let written = write_files(&folder, &probe, &p).await;
        if let Err(e) = written {
            let _ = tokio::fs::remove_dir_all(&folder).await;
            return Err(e.into());
        }
        if let Err(e) = self.copy_public(&probe, &p).await {
            let _ = tokio::fs::remove_dir_all(&folder).await;
            return Err(e.into());
        }
        if let Err(e) = tx.commit().await {
            let _ = tokio::fs::remove_dir_all(&folder).await;
            if let Some(st) = self.public.0.as_ref().filter(|_| !up.private) {
                for (rel, _) in Self::files_of(&probe) {
                    let _ = st.delete(&rel).await;
                }
            }
            return Err(e.into());
        }
        self.db.notify_changed();
        self.get(id).await?.ok_or(MediaError::NotFound)
    }

    pub async fn get(&self, id: i64) -> anyhow::Result<Option<Media>> {
        let row: Option<Row> = sql::GET.bind(id).fetch_optional(&self.db).await?;
        Ok(row.map(Into::into))
    }

    /// Newest first. `family` filters (`image` or `pdf`).
    pub async fn list(
        &self,
        private: bool,
        family: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> anyhow::Result<Vec<Media>> {
        let rows: Vec<Row> = sql::LIST
            .bind(private)
            .bind(family_pattern(family))
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.db)
            .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn count(&self, private: bool, family: Option<&str>) -> anyhow::Result<i64> {
        Ok(sql::COUNT
            .bind(private)
            .bind(family_pattern(family))
            .scalar(&self.db)
            .await?)
    }

    /// Change the description of a file read at `expected` version.
    pub async fn set_alt(&self, id: i64, expected: i32, alt: &str) -> Result<Media, MediaError> {
        let alt = alt.trim();
        if alt.chars().count() > 500 {
            return Err(MediaError::Invalid(
                "the description is longer than 500 characters".into(),
            ));
        }
        let v: Option<i32> = sql::SET_ALT
            .bind(id)
            .bind(expected)
            .bind(alt)
            .bind(Timestamp::now())
            .scalar_opt(&self.db)
            .await?;
        if v.is_none() {
            return Err(self.problem(id, expected).await?);
        }
        self.db.notify_changed();
        self.get(id).await?.ok_or(MediaError::NotFound)
    }

    /// Delete a file read at `expected` version, with its copies.
    pub async fn delete(&self, id: i64, expected: i32) -> Result<(), MediaError> {
        let m = self.get(id).await?.ok_or(MediaError::NotFound)?;
        let n = sql::DELETE
            .bind(id)
            .bind(expected)
            .execute(&self.db)
            .await?;
        if n != 1 {
            return Err(self.problem(id, expected).await?);
        }
        if let Err(e) = tokio::fs::remove_dir_all(self.folder(&m)).await
            && e.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("media: could not remove the files of #{id}: {e}");
        }
        if let Some(st) = self.public.0.as_ref().filter(|_| !m.private) {
            for (rel, _) in Self::files_of(&m) {
                if let Err(e) = st.delete(&rel).await {
                    eprintln!(
                        "media: could not remove {rel} from {}: {e:#}",
                        st.describe()
                    );
                }
            }
        }
        self.db.notify_changed();
        Ok(())
    }

    async fn problem(&self, id: i64, expected: i32) -> anyhow::Result<MediaError> {
        let current: Option<i32> = sql::VERSION.bind(id).scalar_opt(&self.db).await?;
        Ok(match current {
            Some(current) => MediaError::Conflict { expected, current },
            None => MediaError::NotFound,
        })
    }

    /// Read a private file (for the admin).
    pub async fn read(&self, m: &Media) -> std::io::Result<Vec<u8>> {
        tokio::fs::read(self.path(m)).await
    }
}

fn family_pattern(family: Option<&str>) -> Option<String> {
    match family {
        Some("image") => Some("image/%".into()),
        Some("pdf") => Some("application/pdf".into()),
        _ => None,
    }
}

async fn write_one(folder: &Path, file: &str, data: &[u8]) -> std::io::Result<()> {
    let tmp = folder.join(format!(".{file}.tmp"));
    tokio::fs::write(&tmp, data).await?;
    tokio::fs::rename(&tmp, folder.join(file)).await
}

async fn write_files(folder: &Path, m: &Media, p: &process::Processed) -> std::io::Result<()> {
    tokio::fs::create_dir_all(folder).await?;
    write_one(folder, &m.file_name(), &p.original).await?;
    for (w, data) in &p.variants {
        write_one(folder, &m.variant_file(*w), data).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stems_are_safe() {
        assert_eq!(safe_stem("Harbour at Dawn.JPG"), "harbour-at-dawn");
        assert_eq!(safe_stem("../../etc/passwd"), "passwd");
        assert_eq!(safe_stem("..\\..\\boot.ini"), "boot");
        assert_eq!(safe_stem("photo.php.jpg"), "photo-php");
        assert_eq!(safe_stem("<script>.png"), "script");
        assert_eq!(safe_stem(".htaccess"), "htaccess");
        assert_eq!(safe_stem("日本.png"), "file");
        assert_eq!(safe_stem(""), "file");
        assert!(safe_stem(&"a".repeat(500)).len() <= 60);
    }
}
