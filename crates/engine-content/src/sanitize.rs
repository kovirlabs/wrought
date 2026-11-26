//! Safe rendering of stored content.
//!
//! Content in the database is edited through the CMS (and possibly by hand
//! in SQL), so it is never trusted:
//!
//! * `markdown` fields are rendered with the same Markdown options as file
//!   content and the HTML is then cleaned with `ammonia` (an allow-list
//!   sanitiser: no `<script>`, no event handlers, no `javascript:` URLs, no
//!   `style`). Heading ids are kept but prefixed with `c-` so content cannot
//!   clobber ids the page's own markup or scripts rely on.
//! * `url` fields must be `http(s)://`, `mailto:` or site-relative.
//! * every other field is plain data, and templates auto-escape it.
//!
//! Sanitising happens when content is rendered, not when it is saved, so a
//! row changed directly in SQL is still made safe.

use std::sync::LazyLock;

static CLEANER: LazyLock<ammonia::Builder<'static>> = LazyLock::new(|| {
    let mut b = ammonia::Builder::default();
    for h in ["h1", "h2", "h3", "h4", "h5", "h6"] {
        b.add_tag_attributes(h, &["id"]);
    }
    b.id_prefix(Some("c-"));
    b.link_rel(Some("noopener noreferrer"));
    b
});

/// Render Markdown to sanitised HTML.
pub fn markdown_to_html(md: &str) -> String {
    let rendered = engine_core::content::render_markdown(md);
    clean_html(&rendered.html)
}

/// Sanitise an HTML fragment.
pub fn clean_html(html: &str) -> String {
    CLEANER.clean(html).to_string()
}

/// `Some(url)` if it is safe to put in an `href`.
pub fn safe_url(url: &str) -> Option<&str> {
    let u = url.trim();
    if u.is_empty()
        || u.chars()
            .any(|c| c.is_control() || c == '"' || c == '<' || c == '>')
    {
        return None;
    }
    let lower = u.to_ascii_lowercase();
    let ok = lower.starts_with("https://")
        || lower.starts_with("http://")
        || lower.starts_with("mailto:")
        || (u.starts_with('/') && !u.starts_with("//") && !u.starts_with("/\\"))
        || u.starts_with('#');
    ok.then_some(u)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_scripts_handlers_and_js_urls() {
        let md = "# Title\n\nHi <script>alert(1)</script> <img src=x onerror=alert(2)>\n\n\
                  [click](javascript:alert(3)) <a href=\"JaVaScRiPt:alert(4)\">x</a>\n\n\
                  <iframe src=\"https://evil\"></iframe><style>body{}</style>\
                  <div style=\"position:fixed\">ok</div>\n\n**bold** [fine](https://example.org)";
        let html = markdown_to_html(md);
        let lower = html.to_ascii_lowercase();
        for bad in [
            "<script",
            "onerror",
            "javascript:",
            "<iframe",
            "<style",
            "style=",
        ] {
            assert!(!lower.contains(bad), "{bad} in {html}");
        }
        assert!(html.contains("<strong>bold</strong>"), "{html}");
        assert!(html.contains("href=\"https://example.org\""), "{html}");
        assert!(html.contains("rel=\"noopener noreferrer\""), "{html}");
        assert!(html.contains("<h1 id=\"c-title\">Title</h1>"), "{html}");
    }

    #[test]
    fn urls() {
        for ok in ["https://a.b/c", "HTTP://x", "mailto:a@b", "/about/", "#top"] {
            assert!(safe_url(ok).is_some(), "{ok}");
        }
        for bad in [
            "javascript:alert(1)",
            " JavaScript:x",
            "data:text/html,x",
            "//evil.example",
            "/\\evil",
            "vbscript:x",
            "relative",
            "https://a\"onmouseover=\"x",
            "",
        ] {
            assert!(safe_url(bad).is_none(), "{bad}");
        }
    }
}
