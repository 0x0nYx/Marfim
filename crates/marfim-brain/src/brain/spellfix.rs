//! Port of oracle `src/intel/brain/spellfix.js` — typo correction against the
//! vault's own vocabulary.
//!
//! No external dictionary: works fully offline and never flags
//! project-specific jargon as wrong. Builds a SymSpell-style delete-index —
//! every word's 1- and 2-delete variants map back to it — so a rare word
//! that is a near-miss (edit distance <= 2) of a much more common word in
//! the same corpus is very likely a typo of it. `deletes(word, depth)` is
//! exported for reuse (kneser_ney's char model prefilter consumes the same
//! routine).
//!
//! NARROWINGS (per the P4-B differential method):
//! - `words` (oracle `nlp.js`) is inlined below: lowercase `[a-z0-9]+` runs;
//!   non-ASCII characters separate words exactly like the oracle regex.
//!   Word length uses `chars().count()` (== UTF-16 `length` for the ASCII
//!   vocabularies this module indexes; astral inputs are not exercised).
//! - `deletes` returns the variants in JS `Set` iteration order (insertion
//!   order, deduped across levels) — the fixtures pin that order.
//! - Oracle `counter()` (util/pystruct.js) is mirrored by an
//!   insertion-ordered `Vec<(String, i64)>` plus a lookup map that is never
//!   iterated (determinism doctrine).

use std::collections::{HashMap, HashSet};

use serde_json::{json, Map, Value};

/// Inlined from oracle `src/intel/brain/nlp.js` — raw words of the text
/// (lowercased `[a-z0-9]+` runs).
fn words(text: &str) -> Vec<String> {
    let s = text.to_lowercase();
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for ch in s.chars() {
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

/// All delete-variants of the word down to `depth` deletions (including the
/// word itself), in JS `Set` iteration order: breadth-first levels, each
/// deduped against everything seen so far (oracle `deletes`).
pub fn deletes(word: &str, depth: usize) -> Vec<String> {
    let mut variants: Vec<String> = vec![word.to_string()];
    let mut seen: HashSet<String> = HashSet::from_iter([word.to_string()]);
    let mut frontier: Vec<String> = vec![word.to_string()];
    for _ in 0..depth {
        let mut nxt: Vec<String> = Vec::new();
        let mut nxt_seen: HashSet<String> = HashSet::new();
        for w in &frontier {
            let chars: Vec<char> = w.chars().collect();
            for i in 0..chars.len() {
                let candidate: String = chars[..i].iter().chain(chars[i + 1..].iter()).collect();
                if nxt_seen.insert(candidate.clone()) {
                    nxt.push(candidate);
                }
            }
        }
        for v in &nxt {
            if seen.insert(v.clone()) {
                variants.push(v.clone());
            }
        }
        frontier = nxt;
    }
    variants
}

/// One suggestion row (oracle `suggest` entries).
#[derive(Debug, Clone)]
pub struct Suggestion {
    pub typo: String,
    pub count: i64,
    pub likely: String,
    pub likely_count: i64,
}

impl Suggestion {
    pub fn to_json(&self) -> Value {
        json!({
            "typo": self.typo,
            "count": self.count,
            "likely": self.likely,
            "likely_count": self.likely_count,
        })
    }
}

/// Typo correction index over the vault's own vocabulary (oracle
/// `SpellIndex`). Counts preserve first-seen insertion order (JS `counter()`
/// Map semantics); the delete-index maps variants to member words.
#[derive(Debug, Clone)]
pub struct SpellIndex {
    /// words shorter than this are never indexed
    pub min_word_len: usize,
    /// a word with count <= rare_max is "rare"
    pub rare_max: i64,
    /// a candidate needs count >= common_min to blame
    pub common_min: i64,
    counts: Vec<(String, i64)>,
    counts_pos: HashMap<String, usize>, // lookup only; never iterated
    index: HashMap<String, Vec<String>>,
}

impl Default for SpellIndex {
    fn default() -> Self {
        Self::new(3, 1, 5)
    }
}

impl SpellIndex {
    pub fn new(min_word_len: usize, rare_max: i64, common_min: i64) -> Self {
        Self {
            min_word_len,
            rare_max,
            common_min,
            counts: Vec::new(),
            counts_pos: HashMap::new(),
            index: HashMap::new(),
        }
    }

    /// The vocabulary counts in first-seen insertion order (oracle
    /// `this.counts`, a `counter()` Map).
    pub fn counts(&self) -> &[(String, i64)] {
        &self.counts
    }

    fn count_of(&self, word: &str) -> Option<i64> {
        self.counts_pos.get(word).map(|&i| self.counts[i].1)
    }

    /// Build counts + delete index over the corpus texts (chainable; resets
    /// any previous state, as in the oracle).
    pub fn build(&mut self, texts: &[&str]) -> &mut Self {
        self.counts.clear();
        self.counts_pos.clear();
        self.index.clear();
        for t in texts {
            for w in words(t) {
                if w.chars().count() >= self.min_word_len {
                    match self.counts_pos.get(&w) {
                        Some(&i) => self.counts[i].1 += 1,
                        None => {
                            self.counts_pos.insert(w.clone(), self.counts.len());
                            self.counts.push((w, 1));
                        }
                    }
                }
            }
        }
        for (w, _) in &self.counts {
            for d in deletes(w, 2) {
                let bucket = self.index.entry(d).or_default();
                if !bucket.iter().any(|member| member == w) {
                    bucket.push(w.clone());
                }
            }
        }
        self
    }

    /// Rare words that are a near-miss of a much more common corpus word,
    /// sorted by the common word's count (desc, stable). The candidate scan
    /// is lexicographic and keeps the FIRST count-maximum, exactly like the
    /// oracle.
    pub fn suggest(&self, top: usize) -> Vec<Suggestion> {
        let mut out: Vec<Suggestion> = Vec::new();
        for (w, n) in &self.counts {
            if *n > self.rare_max {
                continue;
            }
            let mut candidates: Vec<String> = Vec::new();
            let mut cand_seen: HashSet<String> = HashSet::new();
            for d in deletes(w, 2) {
                if let Some(bucket) = self.index.get(&d) {
                    for c in bucket {
                        if cand_seen.insert(c.clone()) {
                            candidates.push(c.clone());
                        }
                    }
                }
            }
            candidates.retain(|c| c != w);
            candidates.sort(); // oracle: [...candidates].sort()
            let mut best: Option<(String, i64)> = None;
            for c in &candidates {
                let cn = self.count_of(c).unwrap_or(0);
                let beats_best = match best.as_ref() {
                    None => true,
                    Some((_, bn)) => cn > *bn,
                };
                if cn >= self.common_min && beats_best {
                    best = Some((c.clone(), cn));
                }
            }
            if let Some((likely, likely_count)) = best {
                out.push(Suggestion {
                    typo: w.clone(),
                    count: *n,
                    likely,
                    likely_count,
                });
            }
        }
        // stable sort on the insertion-ordered copy, then truncate
        out.sort_by_key(|e| std::cmp::Reverse(e.likely_count));
        out.truncate(top);
        out
    }

    /// Fixture capture shape: counts as an object (order-insensitive).
    pub fn to_json(&self) -> Value {
        let mut counts = Map::new();
        for (w, n) in &self.counts {
            counts.insert(w.clone(), json!(n));
        }
        json!({ "counts": counts })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deletes_levels_breadth_first_with_dedup() {
        assert_eq!(deletes("ab", 0), vec!["ab"]);
        assert_eq!(deletes("ab", 1), vec!["ab", "b", "a"]);
        assert_eq!(deletes("", 2), vec![""]);
        // depth 3 on "ab" yields the empty word too (node-verified:
        // deletes(ab,3) = [ab, b, a, ''])
        assert_eq!(deletes("ab", 3).len(), 4);
        assert!(deletes("ab", 3).iter().any(|w| w.is_empty()));
        // order: level-1 variants first, then level-2, deduped
        let d2 = deletes("aba", 2);
        assert_eq!(d2[0], "aba");
        assert!(d2.contains(&"ba".to_string()));
        assert!(d2.contains(&"aa".to_string()));
        assert!(d2.contains(&"ab".to_string()));
        assert!(d2.contains(&"a".to_string()));
        assert!(d2.contains(&"b".to_string()));
    }

    #[test]
    fn near_miss_of_common_word_is_flagged() {
        let mut ix = SpellIndex::new(3, 1, 5);
        ix.build(&["the the the the the teh quick"]);
        let suggestions = ix.suggest(20);
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].typo, "teh");
        assert_eq!(suggestions[0].likely, "the");
    }

    #[test]
    fn common_words_are_never_suspects() {
        let mut ix = SpellIndex::default();
        ix.build(&["alpha alpha alpha alpha alpha beta beta beta beta beta"]);
        assert!(ix.suggest(20).is_empty());
    }

    #[test]
    fn build_resets_previous_state() {
        let mut ix = SpellIndex::default();
        ix.build(&["reset me please reset me"]);
        ix.build(&["fresh words only fresh words"]);
        assert!(ix.counts().iter().all(|(w, _)| w != "reset"));
        assert_eq!(ix.count_of("fresh"), Some(2));
    }
}
