//! Result snippets made in the engine from the indexed text.
//!
//! PostgreSQL's `ts_headline` parses and stems the whole document again for
//! every hit it highlights: about 0.45 ms per 2.6 KB article on one core,
//! ten times the cost of finding and ranking the matches. Here the hits'
//! text comes back with the page of results, and the snippet is cut in the
//! engine: the query's lexemes are PostgreSQL's own (the normalised
//! `tsquery`, so the query is stemmed exactly as the index was), and a
//! document word is marked when its Snowball stem (the algorithm the
//! built-in text search configurations use) equals one of them. Only words
//! that could stem to a query lexeme are stemmed (an ASCII first letter
//! that no lexeme starts with cannot), and each distinct word once per
//! search, so a page of ten hits costs tens of microseconds.
//!
//! The snippet follows `ts_headline`'s `MaxFragments=2, MaxWords=24`: up to
//! two fragments of at most 24 words, each placed to hold as many matches as
//! it can, in document order, joined by ` … `; with no match in the text (the
//! title matched), the first 24 words. Configurations without a Snowball
//! stemmer here (custom dictionaries, `hunspell`, `unaccent`) keep using
//! `ts_headline` (see [`Language::for_config`]).

use std::collections::HashMap;

use rust_stemmers::{Algorithm, Stemmer};

/// Words per fragment and fragments per snippet (`ts_headline`'s
/// `MaxWords` and `MaxFragments` as the engine used them).
pub const MAX_WORDS: usize = 24;
pub const MAX_FRAGMENTS: usize = 2;
/// Words of context before the first match of a fragment.
const LEAD: usize = 6;

/// How a text search configuration turns words into lexemes, as far as
/// snippets need to know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    /// `simple`: lower-cased words, no stemming.
    Simple,
    /// A built-in Snowball configuration.
    Snowball(Algorithm),
}

impl Language {
    /// The language of a PostgreSQL text search configuration, or `None`
    /// when snippets must come from `ts_headline` (a configuration this
    /// module cannot reproduce).
    pub fn for_config(name: &str) -> Option<Language> {
        use Algorithm::*;
        let a = match name {
            "simple" => return Some(Language::Simple),
            "arabic" => Arabic,
            "danish" => Danish,
            "dutch" => Dutch,
            "english" => English,
            "finnish" => Finnish,
            "french" => French,
            "german" => German,
            "greek" => Greek,
            "hungarian" => Hungarian,
            "italian" => Italian,
            "norwegian" => Norwegian,
            "portuguese" => Portuguese,
            "romanian" => Romanian,
            "russian" => Russian,
            "spanish" => Spanish,
            "swedish" => Swedish,
            "tamil" => Tamil,
            "turkish" => Turkish,
            _ => return None,
        };
        Some(Language::Snowball(a))
    }
}

/// The positive lexemes of a `tsquery` in PostgreSQL's text form
/// (`'iron' & !'coal' | ( 'gate' <-> 'hing' )`): every quoted lexeme not
/// under a `!`. Prefix marks (`:*`) and weights are ignored.
pub fn query_lexemes(tsquery: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // A `!` applies to the next operand: a lexeme or a parenthesised group.
    let mut negated_depths: Vec<usize> = Vec::new();
    let mut depth = 0usize;
    let mut pending_not = false;
    let mut chars = tsquery.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '!' => pending_not = true,
            '(' => {
                depth += 1;
                if pending_not {
                    negated_depths.push(depth);
                    pending_not = false;
                }
            }
            ')' => {
                if negated_depths.last() == Some(&depth) {
                    negated_depths.pop();
                }
                depth = depth.saturating_sub(1);
            }
            '\'' => {
                let mut lex = String::new();
                while let Some(c) = chars.next() {
                    if c == '\'' {
                        if chars.peek() == Some(&'\'') {
                            chars.next();
                            lex.push('\'');
                        } else {
                            break;
                        }
                    } else if c == '\\' {
                        if let Some(n) = chars.next() {
                            lex.push(n);
                        }
                    } else {
                        lex.push(c);
                    }
                }
                let negated = pending_not || !negated_depths.is_empty();
                pending_not = false;
                if !negated && !lex.is_empty() && !out.contains(&lex) {
                    out.push(lex);
                }
            }
            _ => {}
        }
    }
    out
}

/// Cuts snippets for one search: the query's lexemes, a stemmer, and the
/// words already looked at.
pub struct Snippets {
    stemmer: Option<Stemmer>,
    lexemes: Vec<String>,
    /// First letters of the lexemes, when all are ASCII (the prefilter).
    firsts: Option<Vec<u8>>,
    seen: HashMap<String, bool>,
}

impl Snippets {
    pub fn new(lang: Language, lexemes: Vec<String>) -> Snippets {
        let stemmer = match lang {
            Language::Simple => None,
            Language::Snowball(a) => Some(Stemmer::create(a)),
        };
        // Snowball stemmers strip suffixes and normalise letters (accents,
        // `ё`, `ß`), and none here rewrites an ASCII first letter into
        // another one; Arabic's prefix stripping only meets non-ASCII words,
        // which are always stemmed.
        let firsts = lexemes
            .iter()
            .map(|l| l.as_bytes().first().copied().filter(u8::is_ascii))
            .collect::<Option<Vec<u8>>>();
        Snippets {
            stemmer,
            lexemes,
            firsts,
            seen: HashMap::new(),
        }
    }

    fn matches(&mut self, word: &str) -> bool {
        // The prefilter needs no lower-cased copy: most words stop here.
        if let (Some(firsts), Some(&b)) = (&self.firsts, word.as_bytes().first())
            && b.is_ascii()
            && !firsts.contains(&b.to_ascii_lowercase())
        {
            return false;
        }
        let lower = if word.is_ascii() {
            word.to_ascii_lowercase()
        } else {
            word.to_lowercase()
        };
        if let Some(&m) = self.seen.get(&lower) {
            return m;
        }
        let m = match &self.stemmer {
            Some(s) => {
                let stem = s.stem(&lower);
                self.lexemes.iter().any(|l| *l == stem)
            }
            None => self.lexemes.contains(&lower),
        };
        self.seen.insert(lower, m);
        m
    }

    /// Every word of `text` that matches, lower-cased (for comparing with
    /// `ts_headline`'s marks in tests).
    pub fn marked_words(&mut self, text: &str) -> Vec<String> {
        words(text)
            .into_iter()
            .map(|(s, e)| &text[s..e])
            .filter(|w| self.matches(w))
            .map(str::to_lowercase)
            .collect()
    }

    /// The snippet of `text`, matches between `\x01` and `\x02` (as
    /// `ts_headline` was asked to mark them; see [`crate::highlight`]).
    pub fn cut(&mut self, text: &str) -> String {
        let words = words(text);
        if words.is_empty() {
            return String::new();
        }
        let hits: Vec<bool> = words
            .iter()
            .map(|&(s, e)| self.matches(&text[s..e]))
            .collect();
        let at: Vec<usize> = (0..words.len()).filter(|&i| hits[i]).collect();
        if at.is_empty() {
            let end = words.len().min(MAX_WORDS);
            return fragment(text, &words, &hits, 0, end);
        }
        // Matches before each word, for counting a window's matches.
        let mut before = vec![0usize; words.len() + 1];
        for i in 0..words.len() {
            before[i + 1] = before[i] + usize::from(hits[i]);
        }
        let window = |start: usize| {
            let end = (start + MAX_WORDS).min(words.len());
            // Use the whole width near the end of the text.
            let start = end.saturating_sub(MAX_WORDS).min(start);
            (start, end, before[end] - before[start])
        };
        let mut chosen: Vec<(usize, usize)> = Vec::new();
        for _ in 0..MAX_FRAGMENTS {
            let best = at
                .iter()
                .map(|&m| window(m.saturating_sub(LEAD)))
                .filter(|&(s, e, _)| chosen.iter().all(|&(cs, ce)| e <= cs || s >= ce))
                // Most matches, then the earliest.
                .max_by(|a, b| a.2.cmp(&b.2).then(b.0.cmp(&a.0)));
            match best {
                Some((s, e, n)) if n > 0 => chosen.push((s, e)),
                _ => break,
            }
        }
        chosen.sort_unstable();
        chosen
            .iter()
            .map(|&(s, e)| fragment(text, &words, &hits, s, e))
            .collect::<Vec<_>>()
            .join(" … ")
    }
}

/// Byte ranges of the words of `text`: runs of letters and digits (the
/// default parser's word tokens; punctuation and spaces separate them).
fn words(text: &str) -> Vec<(usize, usize)> {
    let b = text.as_bytes();
    let mut out = Vec::with_capacity(b.len() / 6);
    let mut start = None;
    let mut i = 0;
    while i < b.len() {
        // ASCII without decoding; anything else as a char.
        let (word_char, len) = if b[i].is_ascii() {
            (b[i].is_ascii_alphanumeric(), 1)
        } else {
            let c = text[i..].chars().next().unwrap_or(' ');
            (c.is_alphanumeric(), c.len_utf8())
        };
        if word_char {
            start.get_or_insert(i);
        } else if let Some(s) = start.take() {
            out.push((s, i));
        }
        i += len;
    }
    if let Some(s) = start {
        out.push((s, text.len()));
    }
    out
}

/// Words `start..end` of `text` with what lies between them, matches
/// marked.
fn fragment(
    text: &str,
    words: &[(usize, usize)],
    hits: &[bool],
    start: usize,
    end: usize,
) -> String {
    let mut out = String::with_capacity(words[end - 1].1 - words[start].0 + 8);
    let mut pos = words[start].0;
    for i in start..end {
        let (s, e) = words[i];
        out.push_str(&text[pos..s]);
        if hits[i] {
            out.push('\u{1}');
            out.push_str(&text[s..e]);
            out.push('\u{2}');
        } else {
            out.push_str(&text[s..e]);
        }
        pos = e;
    }
    // Punctuation right after the last word (`forged.`), up to a space.
    let tail = text[pos..]
        .find(|c: char| c.is_whitespace() || c.is_alphanumeric())
        .unwrap_or(text.len() - pos);
    out.push_str(&text[pos..pos + tail]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn english(q: &[&str]) -> Snippets {
        Snippets::new(
            Language::for_config("english").unwrap(),
            q.iter().map(|s| s.to_string()).collect(),
        )
    }

    #[test]
    fn lexemes_of_a_tsquery() {
        assert_eq!(query_lexemes("'iron'"), ["iron"]);
        assert_eq!(query_lexemes("'iron' & 'forg'"), ["iron", "forg"]);
        assert_eq!(query_lexemes("'iron' & !'coal'"), ["iron"]);
        assert_eq!(
            query_lexemes("'gate' <-> 'hing' | 'rivet' & !( 'weld' <-> 'seam' ) & 'x'"),
            ["gate", "hing", "rivet", "x"]
        );
        assert_eq!(query_lexemes("'o''brien' & 'a\\\\b'"), ["o'brien", "a\\b"]);
        assert_eq!(query_lexemes("'forg':*"), ["forg"]);
        assert!(query_lexemes("").is_empty());
    }

    #[test]
    fn stemmed_words_are_marked() {
        let mut s = english(&["forg", "iron"]);
        assert_eq!(
            s.cut("Forging the irons, then forged."),
            "\u{1}Forging\u{2} the \u{1}irons\u{2}, then \u{1}forged\u{2}."
        );
        // Not every word starting with the same letter.
        let mut s = english(&["iron"]);
        assert_eq!(s.cut("In it: Irish iron"), "In it: Irish \u{1}iron\u{2}");
    }

    #[test]
    fn long_texts_get_fragments() {
        let filler = |n: usize| vec!["word"; n].join(" ");
        let text = format!(
            "{} anvil {} anvil {}",
            filler(100),
            filler(100),
            filler(100)
        );
        let mut s = english(&["anvil"]);
        let out = s.cut(&text);
        let parts: Vec<&str> = out.split(" … ").collect();
        assert_eq!(parts.len(), 2, "{out}");
        for p in &parts {
            assert!(p.contains("\u{1}anvil\u{2}"));
            assert_eq!(words(p).len(), MAX_WORDS);
            // Context before the match.
            assert!(p.starts_with("word word"));
        }
        // Both matches close together: one fragment holds them.
        let text = format!("{} anvil word anvil {}", filler(50), filler(50));
        let out = s.cut(&text);
        assert!(!out.contains(" … "), "{out}");
        assert_eq!(out.matches('\u{1}').count(), 2);
    }

    #[test]
    fn no_match_in_the_text_gives_its_start() {
        let mut s = english(&["anvil"]);
        let text = (1..=40)
            .map(|i| format!("w{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let out = s.cut(&text);
        assert!(out.starts_with("w1, w2") && out.ends_with("w24,"), "{out}");
        assert_eq!(s.cut(""), "");
        assert_eq!(s.cut(" … !"), "");
    }

    #[test]
    fn short_texts_and_edges() {
        let mut s = english(&["anvil"]);
        assert_eq!(s.cut("anvil"), "\u{1}anvil\u{2}");
        let text = format!("{} anvil", vec!["w"; 60].join(" "));
        let out = s.cut(&text);
        assert!(out.ends_with("\u{1}anvil\u{2}"));
        assert_eq!(words(&out).len(), MAX_WORDS);
    }

    #[test]
    fn simple_and_other_languages() {
        let mut s = Snippets::new(Language::Simple, vec!["forge".into()]);
        assert_eq!(s.cut("Forge forging"), "\u{1}Forge\u{2} forging");
        // German stems drop the umlaut; non-ASCII first letters are always
        // stemmed.
        let mut s = Snippets::new(
            Language::for_config("german").unwrap(),
            vec!["apfel".into()],
        );
        assert_eq!(
            s.cut("Äpfel und Apfel"),
            "\u{1}Äpfel\u{2} und \u{1}Apfel\u{2}"
        );
        let mut s = Snippets::new(
            Language::for_config("russian").unwrap(),
            vec!["кузнец".into()],
        );
        assert!(s.cut("Кузнецы и кузнец").starts_with("\u{1}Кузнецы\u{2}"));
        assert_eq!(Language::for_config("english_hunspell"), None);
        assert_eq!(Language::for_config("simple"), Some(Language::Simple));
    }
}
