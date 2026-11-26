//! Slugs: the URL segment of an item (`/articles/{slug}/`).
//!
//! A slug is lowercase ASCII letters and digits in words joined by single
//! hyphens, at most [`MAX_LEN`] characters. The database enforces the same
//! rule with a CHECK constraint, so a slug written by hand in SQL cannot
//! smuggle `/`, `..` or markup into a published path.

pub const MAX_LEN: usize = 80;

/// The SQL regular expression equivalent of [`is_valid`].
pub const SQL_PATTERN: &str = "^[a-z0-9]+(-[a-z0-9]+)*$";

pub fn is_valid(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_LEN
        && s.split('-').all(|w| {
            !w.is_empty()
                && w.bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
}

/// Make a slug from free text: common Latin accents are folded
/// (`Crème brûlée` → `creme-brulee`), anything else that is not a letter or
/// digit separates words. Returns `None` if nothing usable is left.
pub fn slugify(text: &str) -> Option<String> {
    let mut out = String::new();
    let mut gap = false;
    for c in text.chars() {
        let folded = fold(c);
        let mut any = false;
        for f in folded.chars() {
            if f.is_ascii_alphanumeric() {
                if gap && !out.is_empty() {
                    out.push('-');
                }
                gap = false;
                out.push(f.to_ascii_lowercase());
                any = true;
            }
        }
        if !any && c != '\'' && c != '’' {
            gap = true;
        }
        if out.len() >= MAX_LEN {
            break;
        }
    }
    out.truncate(MAX_LEN);
    let out = out.trim_end_matches('-').to_string();
    (!out.is_empty()).then_some(out)
}

/// `base`, or `base-2`, `base-3`, ... : the first not in `taken`.
pub fn unique(base: &str, taken: &[String]) -> String {
    if !taken.iter().any(|t| t == base) {
        return base.to_string();
    }
    for n in 2.. {
        let suffix = format!("-{n}");
        let mut stem = base.to_string();
        stem.truncate(MAX_LEN - suffix.len());
        let stem = stem.trim_end_matches('-');
        let cand = format!("{stem}{suffix}");
        if !taken.iter().any(|t| t == &cand) {
            return cand;
        }
    }
    unreachable!()
}

fn fold(c: char) -> String {
    let s = match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' => {
            "a"
        }
        'æ' | 'Æ' => "ae",
        'ç' | 'č' | 'ć' | 'Ç' | 'Č' | 'Ć' => "c",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ę' | 'ě' | 'È' | 'É' | 'Ê' | 'Ë' => "e",
        'ì' | 'í' | 'î' | 'ï' | 'Ì' | 'Í' | 'Î' | 'Ï' => "i",
        'ñ' | 'ń' | 'Ñ' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' | 'Ø' => "o",
        'œ' | 'Œ' => "oe",
        'ù' | 'ú' | 'û' | 'ü' | 'Ù' | 'Ú' | 'Û' | 'Ü' => "u",
        'ý' | 'ÿ' | 'Ý' => "y",
        'ß' => "ss",
        'š' | 'ś' | 'Š' => "s",
        'ž' | 'ź' | 'ż' | 'Ž' => "z",
        'ł' | 'Ł' => "l",
        'đ' | 'ð' | 'Đ' => "d",
        'þ' | 'Þ' => "th",
        '&' => " and ",
        _ => return c.to_string(),
    };
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_cases() {
        assert_eq!(slugify("Hello, World!").as_deref(), Some("hello-world"));
        assert_eq!(slugify("  Crème brûlée  ").as_deref(), Some("creme-brulee"));
        assert_eq!(slugify("Don't panic").as_deref(), Some("dont-panic"));
        assert_eq!(slugify("Fish & Chips").as_deref(), Some("fish-and-chips"));
        assert_eq!(slugify("../../etc/passwd").as_deref(), Some("etc-passwd"));
        assert_eq!(slugify("<script>").as_deref(), Some("script"));
        assert_eq!(slugify("日本語"), None);
        assert_eq!(slugify("---"), None);
        let long = slugify(&"word ".repeat(40)).unwrap();
        assert!(long.len() <= MAX_LEN && is_valid(&long), "{long}");
    }

    #[test]
    fn validity_and_uniqueness() {
        for ok in ["a", "a-b", "2026-notes", "x1"] {
            assert!(is_valid(ok), "{ok}");
        }
        for bad in ["", "A", "a--b", "-a", "a-", "a/b", "a.b", "a b", "é"] {
            assert!(!is_valid(bad), "{bad}");
        }
        let taken = vec!["post".to_string(), "post-2".to_string()];
        assert_eq!(unique("post", &taken), "post-3");
        assert_eq!(unique("fresh", &taken), "fresh");
        let base = "a".repeat(MAX_LEN);
        let u = unique(&base, std::slice::from_ref(&base));
        assert!(u.len() <= MAX_LEN && u.ends_with("-2"));
    }
}
