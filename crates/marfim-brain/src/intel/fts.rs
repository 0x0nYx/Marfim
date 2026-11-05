//! Port of oracle `src/intel/fts.js` — in-memory full-text-search store, the
//! pure substitute for the FTS5-backed index (sqlite is not a Node builtin;
//! the original already treats FTS5 as an OPTIONAL accelerator with the pure
//! BM25 as the default). Same contract shape:
//!
//! - [`FtsIndex::index`] (triples) -> ok
//! - [`FtsIndex::search`] -> top-k `{docId, title, score}` ranked by BM25
//! - malformed/degenerate queries degrade to an empty result — never a crash
//!
//! Oracle spec: analysis/caelestia/A1-nlu-vectorize.md (MODULE: fts_store.py).
//! NARROWING (documented divergence): the oracle iterates `this._docs` in JS
//! `Map` insertion order; this port uses a `BTreeMap`. Search output is
//! order-independent (results are totally sorted by score desc, then docId),
//! and the BM25 statistics (df counts, average length) are order-insensitive
//! sums, so parity holds for every observable.

use std::collections::BTreeMap;

/// Oracle fts.js K1 (BM25 k1).
pub const K1: f64 = 1.5;
/// Oracle fts.js B (BM25 b).
pub const B: f64 = 0.75;

/// Mirrors the oracle's `ftsAvailable()`: the pure index is always
/// "available" (the FTS5/sqlite accelerator is optional upstream).
pub fn fts_available() -> bool {
    true
}

/// FTS5-equivalent tokenizer: lowercase ASCII-alphanumeric runs
/// (`String.toLowerCase()` then `/[a-z0-9]+/g`; every other character —
/// including accented letters — separates runs, exactly like the oracle).
pub fn tokenize(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in lower.chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            cur.push(ch);
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// One ranked search hit ({docId, title, score} in the oracle).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SearchHit {
    #[serde(rename = "docId")]
    pub doc_id: String,
    pub title: String,
    pub score: f64,
}

/// In-memory BM25 full-text index (oracle `FtsIndex`).
#[derive(Debug, Default, Clone)]
pub struct FtsIndex {
    ok: bool,
    docs: BTreeMap<String, (String, String)>, // docId -> (title, body)
    tokens: BTreeMap<String, Vec<String>>,    // docId -> tokens
    df: BTreeMap<String, i64>,
    avg_len: f64,
    stale: bool,
}

impl FtsIndex {
    pub fn new() -> Self {
        Self {
            ok: fts_available(),
            docs: BTreeMap::new(),
            tokens: BTreeMap::new(),
            df: BTreeMap::new(),
            avg_len: 0.0,
            stale: false,
        }
    }

    /// Index (or replace) one document by id (oracle `add`, title '').
    pub fn add(&mut self, id: &str, text: &str) -> &mut Self {
        self.add_doc(id, "", text);
        self
    }

    /// Index one document with a separate title (title tokens count too).
    pub fn add_doc(&mut self, doc_id: &str, title: &str, body: &str) -> &mut Self {
        let toks = tokenize(&format!("{title} {body}"));
        self.docs
            .insert(doc_id.to_string(), (title.to_string(), body.to_string()));
        self.tokens.insert(doc_id.to_string(), toks);
        self.stale = true;
        self
    }

    /// (docId, title, body) triples — the fts_store.index shape. The oracle
    /// returns `{ok: true}`; the port returns the ok flag as a bool.
    pub fn index(&mut self, docs: &[(String, String, String)]) -> bool {
        for (doc_id, title, body) in docs {
            self.add_doc(doc_id, title, body);
        }
        true
    }

    /// Oracle `_refresh` — lazily rebuild df counts and the average length.
    fn refresh(&mut self) {
        if !self.stale {
            return;
        }
        self.df = BTreeMap::new();
        let mut total = 0.0;
        for toks in self.tokens.values() {
            total += toks.len().max(1) as f64;
            let mut seen: BTreeMap<&str, ()> = BTreeMap::new();
            for t in toks {
                if seen.insert(t.as_str(), ()).is_none() {
                    *self.df.entry(t.clone()).or_insert(0) += 1;
                }
            }
        }
        let n = self.tokens.len();
        self.avg_len = total / n.max(1) as f64;
        self.stale = false;
    }

    /// Oracle `_bm25` — BM25 score of the query tokens against one document.
    fn bm25(&mut self, query_tokens: &[String], doc_id: &str) -> f64 {
        self.refresh();
        let Some(toks) = self.tokens.get(doc_id) else {
            return 0.0;
        };
        if toks.is_empty() {
            return 0.0;
        }
        let mut tf: BTreeMap<&str, i64> = BTreeMap::new();
        for t in toks {
            *tf.entry(t.as_str()).or_insert(0) += 1;
        }
        let n = self.tokens.len();
        let dl = toks.len().max(1) as f64;
        let avg = self.avg_len;
        let norm = K1 * (1.0 - B + B * dl / avg);
        let mut total = 0.0;
        for term in query_tokens {
            let f = tf.get(term.as_str()).copied().unwrap_or(0);
            if f == 0 {
                continue;
            }
            let df = self.df.get(term.as_str()).copied().unwrap_or(0);
            let idf = ((n as f64 - df as f64 + 0.5) / (df as f64 + 0.5) + 1.0).ln();
            total += (idf * (f as f64 * (K1 + 1.0))) / (f as f64 + norm);
        }
        total
    }

    /// Top-k {docId, title, score} by BM25, best first. A malformed or empty
    /// query is an empty result, never a crash (the FTS5 syntax guard).
    /// Takes `&mut self` because the oracle refreshes its statistics lazily
    /// inside the search path.
    pub fn search(&mut self, query: &str, limit: usize) -> Vec<SearchHit> {
        if !self.ok {
            return Vec::new();
        }
        let q_tokens = tokenize(query);
        if q_tokens.is_empty() {
            return Vec::new();
        }
        let mut scored: Vec<(f64, String)> = Vec::new();
        for doc_id in self.docs.keys().cloned().collect::<Vec<_>>() {
            let score = self.bm25(&q_tokens, &doc_id);
            if score > 0.0 {
                scored.push((score, doc_id));
            }
        }
        // oracle: sort by score desc, then docId ascending (JS `<` on
        // strings = UTF-16 code-unit order; == codepoint order for the BMP).
        scored.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.1.cmp(&b.1))
        });
        scored.truncate(limit);
        scored
            .into_iter()
            .map(|(score, doc_id)| {
                let title = self
                    .docs
                    .get(&doc_id)
                    .map(|(t, _)| t.clone())
                    .unwrap_or_default();
                SearchHit {
                    doc_id,
                    title,
                    score,
                }
            })
            .collect()
    }

    /// Oracle `close` — clears all documents and statistics.
    pub fn close(&mut self) {
        self.docs.clear();
        self.tokens.clear();
        self.df.clear();
        self.stale = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_splits_and_lowercases() {
        assert_eq!(tokenize("Hello, World! 42"), vec!["hello", "world", "42"]);
        assert!(tokenize("").is_empty());
        assert!(tokenize("!!!").is_empty());
        // accented letters are separators (lowercase first, then [a-z0-9]+)
        assert_eq!(tokenize("Héllo Wörld"), vec!["h", "llo", "w", "rld"]);
        assert_eq!(tokenize("BAR-34"), vec!["bar", "34"]);
    }

    #[test]
    fn fts_available_mirrors_oracle_contract() {
        assert!(fts_available());
    }

    #[test]
    fn empty_and_degenerate_queries_degrade_to_empty() {
        let mut ix = FtsIndex::new();
        ix.add("d1", "the quick brown fox");
        assert!(ix.search("", 10).is_empty());
        assert!(ix.search("!!!", 10).is_empty());
        assert!(ix.search("zebra", 10).is_empty());
        assert!(ix.search("quick", 0).is_empty());
        ix.close();
        assert!(ix.search("quick", 10).is_empty());
    }

    #[test]
    fn bm25_ranks_by_score_then_docid() {
        let mut ix = FtsIndex::new();
        ix.add("b", "identical words");
        ix.add("a", "identical words");
        let hits = ix.search("identical", 10);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].doc_id, "a");
        assert_eq!(hits[1].doc_id, "b");
        assert_eq!(hits[0].score, hits[1].score);
        // re-add replaces the document
        ix.add("a", "other stuff");
        assert!(ix.search("identical", 10).iter().all(|h| h.doc_id != "a"));
    }
}
