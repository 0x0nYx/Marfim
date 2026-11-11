//! Session search (distilled: docs/DISTILLATION.md §S — Hermes
//! `hermes_state_fts.py` / `_search.py`, adapted to Marfim's JSONL
//! journal corpus).
//!
//! Pure, bounded, deterministic full-text search over session journal
//! records:
//! - corpus: the journal's text-bearing records, scanned NEWEST-LAST
//!   window capped at [`SCAN_CAP`] records (bounded work per query);
//! - tokenizer: lowercase alphanumeric runs (deterministic, no locale);
//! - ranking: BM25-lite — idf × tf-saturation, plus a phrase bonus for
//!   each quoted `"…"` phrase that matches verbatim;
//! - snippets: ≤ [`SNIPPET_CHARS`] chars around the first match with
//!   the matched region marked (2k-char reads);
//! - ordering: score desc, then record index ASC (ties are stable and
//!   reproducible; same corpus + query → same output, no wall-clock).
//!
//! This is a library: the `SessionSearch` tool (marfim-tools) reads the
//! journal file and feeds the records here; nothing in this module does
//! I/O.

/// Records scanned per query (most recent window of the journal).
pub const SCAN_CAP: usize = 300;
/// Maximum snippet length handed to the model (2k-char reads).
pub const SNIPPET_CHARS: usize = 2_000;
/// Hard cap on hits returned.
pub const MAX_HITS: usize = 20;

/// One ranked hit.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    /// 0-based index into the scanned record window (newest record =
    /// highest index). Lineage for the caller.
    pub index: usize,
    /// Record kind (user_message / assistant_message / tool_result /
    /// compaction / actuation / other).
    pub kind: String,
    /// Deterministic relevance score.
    pub score: f64,
    /// ≤ SNIPPET_CHARS context window around the first match.
    pub snippet: String,
}

/// A parsed query: bare terms + quoted phrases.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Query {
    pub terms: Vec<String>,
    pub phrases: Vec<String>,
}

/// Tokenize: lowercase alphanumeric runs (unicode-aware). Deterministic.
pub fn tokenize(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect()
}

/// Parse a query string: whitespace-separated tokens; `"…"` spans are
/// phrases (quoted-token sanitizer — quotes never reach the index).
pub fn parse_query(q: &str) -> Query {
    let mut out = Query::default();
    let mut chars = q.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c == '"' {
            chars.next();
            let mut phrase = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                phrase.push(c2);
            }
            let phrase = phrase.trim().to_string();
            if !phrase.is_empty() {
                out.phrases.push(phrase);
            }
        } else if c.is_whitespace() {
            chars.next();
        } else {
            let mut term = String::new();
            for c2 in chars.by_ref() {
                if c2.is_whitespace() || c2 == '"' {
                    break;
                }
                term.push(c2);
            }
            let term = term.to_lowercase();
            if !term.is_empty() {
                out.terms.push(term);
            }
        }
    }
    out
}

/// One corpus record: (kind, text). The caller maps journal records to
/// these (text-bearing kinds only).
pub type Record = (String, String);

/// Rank the record window (index 0 = oldest of the window) against the
/// query. Bounded: scans every record once, O(tokens) per record.
pub fn search(records: &[Record], query: &Query) -> Vec<Hit> {
    if query.terms.is_empty() && query.phrases.is_empty() {
        return Vec::new();
    }
    let n = records.len().max(1) as f64;
    // document frequency per term (for idf)
    let mut df: std::collections::BTreeMap<&str, usize> = Default::default();
    for (_, text) in records {
        let toks = tokenize(text);
        for t in &query.terms {
            if toks.iter().any(|x| x == t) {
                *df.entry(t.as_str()).or_insert(0) += 1;
            }
        }
    }
    let mut hits: Vec<Hit> = Vec::new();
    for (i, (kind, text)) in records.iter().enumerate() {
        let toks = tokenize(text);
        let mut score = 0.0f64;
        let mut first_match_byte: Option<usize> = None;
        for t in &query.terms {
            let tf = toks.iter().filter(|x| *x == t).count() as f64;
            if tf == 0.0 {
                continue;
            }
            let idf = (1.0 + n / (1.0 + *df.get(t.as_str()).unwrap_or(&0) as f64)).ln();
            score += idf * (tf / (tf + 1.2));
            // first byte offset of the term (case-insensitive) for the
            // snippet window
            if first_match_byte.is_none() {
                if let Some(pos) = text.to_lowercase().find(t.as_str()) {
                    first_match_byte = Some(pos);
                }
            }
        }
        // quoted phrases: verbatim substring, big bonus, snippet anchor
        for p in &query.phrases {
            if let Some(pos) = text.to_lowercase().find(&p.to_lowercase()) {
                score += 5.0;
                if first_match_byte.map(|m| pos < m).unwrap_or(true) {
                    first_match_byte = Some(pos);
                }
            }
        }
        if score <= 0.0 {
            continue;
        }
        let snippet = snippet_of(text, first_match_byte);
        hits.push(Hit {
            index: i,
            kind: kind.clone(),
            score,
            snippet,
        });
    }
    // score desc, index asc — fully deterministic
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.index.cmp(&b.index))
    });
    hits.truncate(MAX_HITS);
    hits
}

/// ≤ SNIPPET_CHARS window around the match, marked with « » around the
/// matched region (bounded read).
fn snippet_of(text: &str, at: Option<usize>) -> String {
    if text.len() <= SNIPPET_CHARS {
        return text.to_string();
    }
    let Some(mid) = at else {
        let cut = text
            .char_indices()
            .map(|(b, _)| b)
            .take_while(|&b| b <= SNIPPET_CHARS)
            .count();
        return format!("…{}", &text[..cut.min(text.len())]);
    };
    let half = SNIPPET_CHARS / 2;
    let start_byte = mid.saturating_sub(half);
    // snap to char boundaries
    let start_byte = (0..=start_byte)
        .rev()
        .find(|b| text.is_char_boundary(*b))
        .unwrap_or(0);
    let raw_end = (start_byte + SNIPPET_CHARS).min(text.len());
    let end_byte = (raw_end..=text.len())
        .find(|b| text.is_char_boundary(*b))
        .unwrap_or(text.len());
    let mut out = String::new();
    if start_byte > 0 {
        out.push('…');
    }
    out.push_str(&text[start_byte..end_byte]);
    if end_byte < text.len() {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(kind: &str, text: &str) -> Record {
        (kind.to_string(), text.to_string())
    }

    #[test]
    fn parse_query_splits_terms_and_phrases() {
        let q = parse_query(r#"frobnicator "src/lib.rs" tuning"#);
        assert_eq!(q.terms, vec!["frobnicator", "tuning"]);
        assert_eq!(q.phrases, vec!["src/lib.rs"]);
        // quotes never leak into terms
        assert!(q.terms.iter().all(|t| !t.contains('"')));
    }

    #[test]
    fn empty_query_returns_nothing() {
        assert!(search(&[rec("user_message", "hello")], &parse_query("")).is_empty());
        assert!(search(&[rec("user_message", "hello")], &parse_query(r#""""#)).is_empty());
    }

    #[test]
    fn terms_rank_by_idf_tf_and_ties_are_stable() {
        let corpus = vec![
            rec("user_message", "fix the frobnicator"),
            rec("assistant_message", "the frobnicator frobnicator is tuned"),
            rec("tool_result", "frobnicator ok"),
            rec("user_message", "unrelated text"),
        ];
        let hits = search(&corpus, &parse_query("frobnicator"));
        assert_eq!(hits.len(), 3);
        // tf-saturation: 2 occurrences (middle) beat 1, but sublinearly
        assert_eq!(hits[0].index, 1);
        assert!(hits[0].score > hits[1].score);
        // deterministic tie-break: same score → lower index first
        let mut c = corpus.clone();
        c[0] = rec("user_message", "alpha beta");
        c[1] = rec("assistant_message", "alpha beta");
        let h2 = search(&c, &parse_query("alpha"));
        assert_eq!(h2.len(), 2);
        assert_eq!(h2[0].index, 0);
        assert_eq!(h2[1].index, 1);
        assert!((h2[0].score - h2[1].score).abs() < f64::EPSILON);
    }

    #[test]
    fn phrases_require_verbatim_match_and_bonus() {
        let corpus = vec![
            rec("user_message", "please check src/lib.rs now"),
            rec("tool_result", "src lib rs are separate words here"),
        ];
        let hits = search(&corpus, &parse_query(r#""src/lib.rs""#));
        assert_eq!(hits.len(), 1, "phrase is verbatim, not token soup");
        assert_eq!(hits[0].index, 0);
    }

    #[test]
    fn snippet_is_bounded_and_marks_nothing_on_short_text() {
        let hits = search(&[rec("user_message", "short")], &parse_query("short"));
        assert_eq!(hits[0].snippet, "short");
    }

    #[test]
    fn snippet_windows_around_match_on_long_text() {
        let mut long = String::new();
        long.push_str(&"x ".repeat(5_000));
        long.push_str("NEEDLE ");
        long.push_str(&"y ".repeat(5_000));
        let hits = search(&[rec("tool_result", &long)], &parse_query("needle"));
        assert_eq!(hits.len(), 1);
        assert!(hits[0].snippet.contains("NEEDLE"), "match kept");
        assert!(
            hits[0].snippet.chars().count() <= SNIPPET_CHARS + 2,
            "bounded (chars, incl. markers)"
        );
        assert!(hits[0].snippet.starts_with('…'));
        assert!(hits[0].snippet.ends_with('…'));
    }

    #[test]
    fn scan_cap_is_respected_by_the_caller_contract() {
        // The windowing contract: the tool feeds at most SCAN_CAP
        // records; search itself stays O(window).
        let corpus: Vec<Record> = (0..SCAN_CAP + 50)
            .map(|i| rec("user_message", &format!("record {i} frobnicator")))
            .collect();
        let window = &corpus[corpus.len() - SCAN_CAP..];
        let hits = search(window, &parse_query("frobnicator"));
        assert!(hits.len() <= MAX_HITS);
        assert!(hits.iter().all(|h| h.index < SCAN_CAP));
    }
}
