//! Deciding what a file is from its bytes.
//!
//! The uploaded name and the browser's `Content-Type` are ignored: they are
//! whatever the sender says. Only the formats below are accepted; markup
//! (SVG, HTML, XML) is recognised so the refusal can say why.

/// An accepted file type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Jpeg,
    Png,
    Gif,
    Webp,
    Pdf,
}

impl Kind {
    pub fn ext(self) -> &'static str {
        match self {
            Kind::Jpeg => "jpg",
            Kind::Png => "png",
            Kind::Gif => "gif",
            Kind::Webp => "webp",
            Kind::Pdf => "pdf",
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            Kind::Jpeg => "image/jpeg",
            Kind::Png => "image/png",
            Kind::Gif => "image/gif",
            Kind::Webp => "image/webp",
            Kind::Pdf => "application/pdf",
        }
    }

    pub fn is_image(self) -> bool {
        self != Kind::Pdf
    }

    pub fn from_ext(ext: &str) -> Option<Kind> {
        Some(match ext {
            "jpg" => Kind::Jpeg,
            "png" => Kind::Png,
            "gif" => Kind::Gif,
            "webp" => Kind::Webp,
            "pdf" => Kind::Pdf,
            _ => return None,
        })
    }

    /// `image` or `pdf` (the families form fields can ask for).
    pub fn family(self) -> &'static str {
        if self.is_image() { "image" } else { "pdf" }
    }
}

/// Why a file was refused by type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    /// SVG, HTML or XML: markup that a browser may run script from.
    Markup(&'static str),
    Empty,
    Unknown,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refused::Markup(what) => write!(
                f,
                "{what} files are not accepted: they can carry scripts. Upload a JPEG, PNG, GIF, \
                 WebP or PDF instead"
            ),
            Refused::Empty => f.write_str("the file is empty"),
            Refused::Unknown => f.write_str(
                "this kind of file is not accepted (JPEG, PNG, GIF, WebP and PDF are; the file \
                 name does not matter, its content does)",
            ),
        }
    }
}

/// What `bytes` are.
pub fn sniff(bytes: &[u8]) -> Result<Kind, Refused> {
    if bytes.is_empty() {
        return Err(Refused::Empty);
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Ok(Kind::Jpeg);
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Ok(Kind::Png);
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Ok(Kind::Gif);
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Ok(Kind::Webp);
    }
    if bytes.starts_with(b"%PDF-") {
        return Ok(Kind::Pdf);
    }
    if let Some(what) = markup(bytes) {
        return Err(Refused::Markup(what));
    }
    Err(Refused::Unknown)
}

/// Whether the start of `bytes` looks like SVG, HTML or XML (with any BOM,
/// whitespace or comments before it, in UTF-8 or UTF-16).
fn markup(bytes: &[u8]) -> Option<&'static str> {
    let head = &bytes[..bytes.len().min(4096)];
    // UTF-16 text: drop the zero bytes to look at it as ASCII.
    let ascii: Vec<u8> = if head.starts_with(&[0xFF, 0xFE]) || head.starts_with(&[0xFE, 0xFF]) {
        head.iter().copied().filter(|b| *b != 0).collect()
    } else {
        head.to_vec()
    };
    let lower = String::from_utf8_lossy(&ascii).to_ascii_lowercase();
    if lower.contains("<svg") {
        return Some("SVG");
    }
    if lower.contains("<html")
        || lower.contains("<!doctype html")
        || lower.contains("<script")
        || lower.contains("<body")
        || lower.contains("<iframe")
    {
        return Some("HTML");
    }
    let t = lower.trim_start_matches(['\u{feff}', ' ', '\t', '\r', '\n']);
    if t.starts_with("<?xml") || t.starts_with('<') {
        return Some("XML or HTML");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_bytes_decide() {
        assert_eq!(sniff(b"\xFF\xD8\xFF\xE0rest").unwrap(), Kind::Jpeg);
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\nrest").unwrap(), Kind::Png);
        assert_eq!(sniff(b"GIF89a....").unwrap(), Kind::Gif);
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 ").unwrap(), Kind::Webp);
        assert_eq!(sniff(b"%PDF-1.7\n").unwrap(), Kind::Pdf);
        assert_eq!(sniff(b"").unwrap_err(), Refused::Empty);
        assert_eq!(sniff(b"MZ\x90\0").unwrap_err(), Refused::Unknown);
        assert_eq!(sniff(b"just text").unwrap_err(), Refused::Unknown);
    }

    #[test]
    fn markup_is_refused_with_a_reason() {
        for (bytes, what) in [
            (
                &b"<svg xmlns='http://www.w3.org/2000/svg'><script>x</script></svg>"[..],
                "SVG",
            ),
            (b"\xEF\xBB\xBF  <?xml version='1.0'?>\n<svg/>", "SVG"),
            (b"<!-- hi --><SVG onload=alert(1)>", "SVG"),
            (b"<!DOCTYPE html><p>x", "HTML"),
            (b"\n\n<script>alert(1)</script>", "HTML"),
            (b"<?xml version='1.0'?><x/>", "XML or HTML"),
        ] {
            assert_eq!(
                sniff(bytes).unwrap_err(),
                Refused::Markup(what),
                "{bytes:?}"
            );
        }
        // UTF-16 SVG.
        let utf16: Vec<u8> = "\u{feff}<svg>"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        assert_eq!(sniff(&utf16).unwrap_err(), Refused::Markup("SVG"));
        // A JPEG name does not make HTML a JPEG.
        assert!(sniff(b"<html><body>").is_err());
    }
}
