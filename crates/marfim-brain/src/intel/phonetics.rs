//! Port of oracle `src/intel/phonetics.js` — deterministic soundex-style
//! keys + char-n-gram sets: the "sealed-gap" phonetic matching channel that
//! lets mishearings ("kwin") and typo'd tool atoms ("transparancy") still
//! reach the right tool without widening the lexical channel for everything
//! else. Pure functions, builtins only.
//!
//! NOTE (do not merge with lexicon): this module's `char_ngrams` pads with
//! TWO SPACES each side and returns a SET; `lexicon.char_ngrams` pads with
//! "|" and returns a LIST.
//!
//! NARROWING: `charNgrams` indexes by JS `.length` (UTF-16 code units) and
//! slices UTF-16 code units, so astral-plane input (surrogate pairs) can be
//! split mid-character by the oracle. The port slices the UTF-16 unit array
//! exactly like the oracle and decodes each window with
//! `String::from_utf16_lossy` — BMP text (including astral chars as
//! whole-window contents) is byte-exact; a window that splits a surrogate
//! pair yields U+FFFD where the oracle would yield a lone surrogate. All
//! committed fixtures are BMP-only.

use std::collections::BTreeSet;

/// Soundex consonant classes (oracle LETTER_CODE).
fn letter_code(c: char) -> Option<&'static str> {
    match c {
        'B' | 'F' | 'P' | 'V' => Some("1"),
        'C' | 'G' | 'J' | 'K' | 'Q' | 'S' | 'X' | 'Z' => Some("2"),
        'D' | 'T' => Some("3"),
        'L' => Some("4"),
        'M' | 'N' => Some("5"),
        'R' => Some("6"),
        _ => None,
    }
}

/// Classic soundex, bounded to 4 chars. Deterministic; "" for strings with
/// no letters (digits/punct never produce keys). H/W skip WITHOUT resetting
/// the previous code; vowels DO reset it.
pub fn soundex(word: &str) -> String {
    // oracle: String(word).replace(/[^A-Za-z]/g, '').toUpperCase()
    let cleaned: String = word
        .chars()
        .filter(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_ascii_uppercase();
    let Some(first) = cleaned.chars().next() else {
        return String::new();
    };
    let mut coded = String::new();
    let mut prev = letter_code(first).unwrap_or("");
    for ch in cleaned.chars().skip(1) {
        let code = letter_code(ch).unwrap_or("");
        if ch == 'H' || ch == 'W' {
            continue; // do not reset prev
        }
        if !code.is_empty() && code != prev {
            coded.push_str(code);
        }
        prev = code;
    }
    let mut out = String::new();
    out.push(first);
    out.push_str(&coded);
    out.push_str("000");
    out.chars().take(4).collect()
}

/// JS `\s` (RegExp whitespace): Unicode White_Space plus U+FEFF.
fn is_js_space(c: char) -> bool {
    c.is_whitespace() || c == '\u{feff}'
}

/// Lowercased padded character n-grams as a SET (two-space pads). Mirrors
/// `String.toLowerCase()`, `replace(/\s+/g, ' ')`, `trim()` and the
/// UTF-16-unit slicing of the oracle.
pub fn char_ngrams(text: &str, n: usize) -> BTreeSet<String> {
    let lower = text.to_lowercase();
    // collapse runs of JS whitespace to a single space
    let mut collapsed = String::new();
    let mut last_was_space = false;
    for ch in lower.chars() {
        if is_js_space(ch) {
            if !last_was_space {
                collapsed.push(' ');
            }
            last_was_space = true;
        } else {
            collapsed.push(ch);
            last_was_space = false;
        }
    }
    // trim (both ends) with the same whitespace class
    let trimmed: &str = collapsed.trim_matches(is_js_space);
    let normalized: String = trimmed.to_string();

    let units: Vec<u16> = normalized.encode_utf16().collect();
    let mut out: BTreeSet<String> = BTreeSet::new();
    if units.len() < n {
        if !normalized.is_empty() {
            out.insert(normalized);
        }
        return out;
    }
    let padded_units: Vec<u16> = format!("  {normalized}  ").encode_utf16().collect();
    let mut i = 0usize;
    while i + n <= padded_units.len() {
        out.insert(String::from_utf16_lossy(&padded_units[i..i + n]));
        i += 1;
    }
    out
}

/// JS `.length` — the number of UTF-16 code units.
fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Fraction of the query's content tokens whose soundex key matches ANY
/// atom token's key. 0.0 when the query has no phonetic content — digits
/// and one/two-letter tokens never match, so pure-value requests
/// ("bar 34") keep their honest 0.0.
pub fn phonetic_hit_rate(query_tokens: &[String], atom_tokens: &[String]) -> f64 {
    let q_keys: Vec<String> = query_tokens
        .iter()
        .filter(|t| utf16_len(t) > 2 && t.chars().any(|c| c.is_ascii_alphabetic()))
        .map(|t| soundex(t))
        .filter(|k| !k.is_empty())
        .collect();
    let a_keys: BTreeSet<String> = atom_tokens
        .iter()
        .map(|a| soundex(a))
        .filter(|k| !k.is_empty())
        .collect();
    if q_keys.is_empty() || a_keys.is_empty() {
        return 0.0;
    }
    let mut hits = 0usize;
    for k in &q_keys {
        if a_keys.contains(k) {
            hits += 1;
        }
    }
    hits as f64 / q_keys.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soundex_classic_keys() {
        assert_eq!(soundex("Robert"), "R163");
        assert_eq!(soundex("Rupert"), "R163");
        assert_eq!(soundex("Ashcraft"), "A261");
        assert_eq!(soundex("Ashcroft"), "A261");
        assert_eq!(soundex("Tymczak"), "T522");
        assert_eq!(soundex("Pfister"), "P236");
        assert_eq!(soundex("Honeyman"), "H555");
    }

    #[test]
    fn soundex_degenerate_inputs() {
        assert_eq!(soundex(""), "");
        assert_eq!(soundex("123"), "");
        assert_eq!(soundex("!!!"), "");
        assert_eq!(soundex(""), "");
        assert_eq!(soundex("x"), "X000");
        // H/W skip without resetting; vowels reset
        assert_eq!(soundex("WHY"), "W000");
    }

    #[test]
    fn ngrams_pad_and_short_inputs() {
        let s = char_ngrams("abc", 3);
        let v: Vec<String> = s.into_iter().collect();
        assert_eq!(v, vec!["  a", " ab", "abc", "bc ", "c  "]);
        // shorter than n: the whole normalized string is the one element
        assert_eq!(
            char_ngrams("ab", 3).into_iter().collect::<Vec<_>>(),
            vec!["ab"]
        );
        assert!(char_ngrams("", 3).is_empty());
        assert!(char_ngrams("   ", 3).is_empty());
    }

    #[test]
    fn hit_rate_honest_zero_for_non_phonetic() {
        let q: Vec<String> = ["bar", "34"].iter().map(|s| s.to_string()).collect();
        let a: Vec<String> = ["bar"].iter().map(|s| s.to_string()).collect();
        assert_eq!(phonetic_hit_rate(&q, &a), 1.0);
        // two-letter tokens never count as phonetic content
        let q2: Vec<String> = ["ab", "cd"].iter().map(|s| s.to_string()).collect();
        assert_eq!(phonetic_hit_rate(&q2, &a), 0.0);
        assert_eq!(phonetic_hit_rate(&[], &a), 0.0);
        assert_eq!(phonetic_hit_rate(&q, &[]), 0.0);
    }
}
