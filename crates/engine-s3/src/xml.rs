//! Just enough XML for the S3 and CloudFront answers Hammerwright reads
//! (`ListObjectsV2`, error documents): elements without attributes that
//! matter, text with the predefined and numeric entities. Not a general
//! XML parser.

/// Text of every `<name>...</name>` element in `doc`, in order.
pub fn all<'a>(doc: &'a str, name: &str) -> Vec<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let mut out = Vec::new();
    let mut rest = doc;
    while let Some(i) = rest.find(&open) {
        let after = &rest[i + open.len()..];
        let Some(j) = after.find(&close) else { break };
        out.push(&after[..j]);
        rest = &after[j + close.len()..];
    }
    out
}

/// Text of the first `<name>` element, unescaped.
pub fn first(doc: &str, name: &str) -> Option<String> {
    all(doc, name).first().map(|s| unescape(s))
}

/// Resolve `&lt; &gt; &amp; &quot; &apos;` and `&#N;` / `&#xN;`.
pub fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let after = &rest[i..];
        let Some(end) = after.find(';') else {
            out.push_str(after);
            return out;
        };
        let ent = &after[1..end];
        let ch = match ent {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => ent
                .strip_prefix("#x")
                .or_else(|| ent.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &after[end + 1..];
            }
            None => {
                out.push('&');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Escape text for an element body.
pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_and_unescapes() {
        let doc = "<R><Contents><Key>a&amp;b</Key><ETag>&quot;abc&quot;</ETag></Contents>\
                   <Contents><Key>c&#47;d&#x2F;e</Key></Contents><IsTruncated>false</IsTruncated></R>";
        let c = all(doc, "Contents");
        assert_eq!(c.len(), 2);
        assert_eq!(first(c[0], "Key").unwrap(), "a&b");
        assert_eq!(first(c[0], "ETag").unwrap(), "\"abc\"");
        assert_eq!(first(c[1], "Key").unwrap(), "c/d/e");
        assert_eq!(first(doc, "IsTruncated").unwrap(), "false");
        assert_eq!(unescape("x & y &bogus; z"), "x & y &bogus; z");
        assert_eq!(escape("<a&'\">"), "&lt;a&amp;&apos;&quot;&gt;");
    }
}
