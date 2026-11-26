//! Reading `multipart/form-data` uploads with limits (the admin library and
//! form file fields share this).

use axum::extract::Multipart;

use crate::MediaError;

/// One uploaded file.
#[derive(Debug, Clone)]
pub struct FilePart {
    /// The form field it came in.
    pub field: String,
    /// The name the browser sent (untrusted; only used for a readable stem).
    pub filename: String,
    pub bytes: Vec<u8>,
}

/// A multipart form: its text fields (in order) and its files.
#[derive(Debug, Default)]
pub struct Parts {
    pub fields: Vec<(String, String)>,
    pub files: Vec<FilePart>,
}

/// Longest accepted text field.
const MAX_FIELD: usize = 64 * 1024;
/// Most parts in one form.
const MAX_PARTS: usize = 200;

/// Read every part, refusing files over `max_file_bytes` and more than
/// `max_files` files. Empty file inputs (no file chosen) are skipped.
pub async fn read_multipart(
    mut mp: Multipart,
    max_file_bytes: u64,
    max_files: usize,
) -> Result<Parts, MediaError> {
    let mut out = Parts::default();
    let mut n = 0;
    let too_large = || MediaError::TooLarge {
        max_mb: max_file_bytes.div_ceil(1024 * 1024),
    };
    loop {
        let field = match mp.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            // Over the body limit, or a malformed body.
            Err(e) if e.status() == axum::http::StatusCode::PAYLOAD_TOO_LARGE => {
                return Err(too_large());
            }
            Err(e) => {
                return Err(MediaError::Invalid(format!(
                    "the upload was not readable: {e}"
                )));
            }
        };
        n += 1;
        if n > MAX_PARTS {
            return Err(MediaError::Invalid("the form has too many parts".into()));
        }
        let name = field.name().unwrap_or("").to_string();
        let filename = field.file_name().map(str::to_string);
        let mut field = field;
        let mut buf = Vec::new();
        let limit = if filename.is_some() {
            max_file_bytes as usize
        } else {
            MAX_FIELD
        };
        loop {
            match field.chunk().await {
                Ok(Some(c)) => {
                    if buf.len() + c.len() > limit {
                        return Err(if filename.is_some() {
                            too_large()
                        } else {
                            MediaError::Invalid(format!("the field `{name}` is too long"))
                        });
                    }
                    buf.extend_from_slice(&c);
                }
                Ok(None) => break,
                Err(e) if e.status() == axum::http::StatusCode::PAYLOAD_TOO_LARGE => {
                    return Err(too_large());
                }
                Err(e) => {
                    return Err(MediaError::Invalid(format!(
                        "the upload was not readable: {e}"
                    )));
                }
            }
        }
        match filename {
            Some(f) => {
                if f.is_empty() && buf.is_empty() {
                    continue;
                }
                if out.files.len() >= max_files {
                    return Err(MediaError::Invalid("too many files in one form".into()));
                }
                out.files.push(FilePart {
                    field: name,
                    filename: f,
                    bytes: buf,
                });
            }
            None => {
                let v = String::from_utf8(buf)
                    .map_err(|_| MediaError::Invalid(format!("the field `{name}` is not text")))?;
                out.fields.push((name, v));
            }
        }
    }
    Ok(out)
}

/// The request body limit for a route that takes uploads: the file limit
/// plus room for the other fields.
pub fn body_limit(max_file_bytes: u64, max_files: usize) -> usize {
    (max_file_bytes as usize).saturating_mul(max_files.max(1)) + 256 * 1024
}
