//! Port of oracle `src/intel/brain/ranking.js` — the shared pairwise ranking
//! primitive: the online `EloLadder` (Elo 1978; the Bernoulli(1) online
//! special case of Bradley-Terry) and the batch `bradleyTerry` logistic fit
//! (Bradley & Terry 1952) with adaptive step decay, plus the `ladderReport`
//! that renders both views and their Kendall-tau agreement.
//!
//! Pure module: no I/O, no randomness (the batch fit is deterministic
//! gradient ascent with a fixed iteration count and order).
//!
//! NARROWINGS (per the P4-B differential method):
//! - `pyRound` is the shared inlined copy at
//!   [`crate::brain::pooling::py_round`]; `pyRepr` (oracle
//!   `util/pystruct.js`) is inlined below for the string-repr error message.
//! - Transcendentals: the oracle's `Math.exp` and `10 ** z` map to
//!   `f64::exp` / `f64::powf`, verified BIT-identical to the Node engine on
//!   the fixture probe domain (`_probe_exp` / `_probe_pow10` fixture lines;
//!   V8 and the platform libm agree on these inputs). Fixtures replay real
//!   gradients/ratings through both engines, so any drift beyond the
//!   documented <=1 ulp would fail the differential test.
//! - Elo comparison counts are `i64` (`Math.trunc` semantics); ratings `f64`.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};

use serde_json::{json, Map, Value};

use crate::brain::pooling::{jnum, py_round};

/// Items with fewer comparisons render "not enough data" (oracle
/// `MIN_COMPARISONS`).
pub const MIN_COMPARISONS: i64 = 3;
/// Default Elo update step (oracle `DEFAULT_ELO_K`).
pub const DEFAULT_ELO_K: f64 = 24.0;
/// Starting rating — no prior opinion (oracle `BASE_RATING`).
pub const BASE_RATING: f64 = 1200.0;

/// `10 ** z` (the oracle's Elo logistic link uses the exponentiation
/// operator, i.e. JS `Math.pow` semantics).
fn pow10(z: f64) -> f64 {
    10f64.powf(z)
}

/// Inlined from oracle `src/intel/util/pystruct.js` — `reprString`, the
/// Python `str` repr body (quote choice + escapes) used by `pyRepr` in the
/// self-play error message. Only the string surface is needed here.
fn py_repr_str(s: &str) -> String {
    let has_single = s.contains('\'');
    let has_double = s.contains('"');
    let quote = if has_single && !has_double { '"' } else { '\'' };
    let mut out = String::new();
    out.push(quote);
    for ch in s.chars() {
        let cp = ch as u32;
        if ch == quote || ch == '\\' {
            out.push('\\');
            out.push(ch);
        } else if cp >= 0x20 && cp != 0x7f {
            out.push(ch);
        } else {
            match cp {
                0x09 => out.push_str("\\t"),
                0x0a => out.push_str("\\n"),
                0x0d => out.push_str("\\r"),
                _ => out.push_str(&format!("\\x{cp:02x}")),
            }
        }
    }
    out.push(quote);
    out
}

/// Error surfaces of the ranking module (oracle `RangeError`s).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RankingError {
    /// Oracle: `throw new RangeError('an item cannot play itself')`.
    SelfPlay,
    /// Oracle: `` throw new RangeError(`item ${pyRepr(w)} cannot play itself`) ``.
    SelfPlayItem(String),
}

impl std::fmt::Display for RankingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RankingError::SelfPlay => write!(f, "an item cannot play itself"),
            RankingError::SelfPlayItem(w) => {
                write!(f, "item {} cannot play itself", py_repr_str(w))
            }
        }
    }
}

impl std::error::Error for RankingError {}

/// One sorted ladder row: rating, comparisons, honest-confidence flag.
#[derive(Debug, Clone)]
pub struct LadderRow {
    pub item: String,
    pub rating: f64,
    pub comparisons: i64,
    pub enough_data: bool,
}

impl LadderRow {
    pub fn to_json(&self) -> Value {
        json!({
            "item": self.item,
            "rating": jnum(self.rating),
            "comparisons": self.comparisons,
            "enough_data": self.enough_data,
        })
    }
}

/// Online Elo ratings over named items (oracle `EloLadder`). Each pair record
/// is (winner, loser); ratings start at BASE_RATING (no prior opinion).
///
/// The JS `_rating`/`_count` helpers INSERT the default on first access —
/// `expected`/`ladder` mutate the maps exactly like the oracle, so those
/// methods take `&mut self`.
#[derive(Debug, Clone)]
pub struct EloLadder {
    pub ratings: BTreeMap<String, f64>,
    pub counts: BTreeMap<String, i64>,
    pub k: f64,
}

impl Default for EloLadder {
    fn default() -> Self {
        Self::new(None, None, DEFAULT_ELO_K)
    }
}

impl EloLadder {
    pub fn new(
        ratings: Option<&BTreeMap<String, f64>>,
        counts: Option<&BTreeMap<String, f64>>,
        k: f64,
    ) -> Self {
        let mut r = BTreeMap::new();
        for (name, value) in ratings.into_iter().flatten() {
            r.insert(name.clone(), *value);
        }
        let mut c = BTreeMap::new();
        for (name, value) in counts.into_iter().flatten() {
            // oracle: Math.trunc(Number(value))
            c.insert(name.clone(), value.trunc() as i64);
        }
        Self {
            ratings: r,
            counts: c,
            k,
        }
    }

    fn rating_entry(&mut self, name: &str) -> f64 {
        *self.ratings.entry(name.to_string()).or_insert(BASE_RATING)
    }

    fn count_entry(&mut self, name: &str) -> i64 {
        *self.counts.entry(name.to_string()).or_insert(0)
    }

    /// One pairwise outcome: winner beats loser.
    pub fn observe(&mut self, winner: &str, loser: &str) -> Result<(), RankingError> {
        if winner == loser {
            return Err(RankingError::SelfPlay);
        }
        let rw = self.rating_entry(winner);
        let rl = self.rating_entry(loser);
        let expected_w = 1.0 / (1.0 + pow10((rl - rw) / 400.0));
        let delta = self.k * (1.0 - expected_w);
        self.ratings.insert(winner.to_string(), rw + delta);
        self.ratings.insert(loser.to_string(), rl - delta);
        let cw = self.count_entry(winner) + 1;
        let cl = self.count_entry(loser) + 1;
        self.counts.insert(winner.to_string(), cw);
        self.counts.insert(loser.to_string(), cl);
        Ok(())
    }

    /// P(a beats b) under the current ratings (the logistic BT link).
    pub fn expected(&mut self, a: &str, b: &str) -> f64 {
        let ra = self.rating_entry(a);
        let rb = self.rating_entry(b);
        1.0 / (1.0 + pow10((rb - ra) / 400.0))
    }

    /// Sorted ladder rows: rating desc, then item asc. With `names`, exactly
    /// that order's items are rendered (defaults are inserted on access, as
    /// in the oracle); without, every rated item in sorted order.
    pub fn ladder(&mut self, names: Option<&[String]>) -> Vec<LadderRow> {
        let items: Vec<String> = match names {
            Some(ns) => ns.to_vec(),
            None => self.ratings.keys().cloned().collect(),
        };
        let mut rows: Vec<LadderRow> = items
            .iter()
            .map(|name| {
                let comparisons = self.count_entry(name);
                LadderRow {
                    item: name.clone(),
                    rating: py_round(self.rating_entry(name), 1),
                    comparisons,
                    enough_data: comparisons >= MIN_COMPARISONS,
                }
            })
            .collect();
        rows.sort_by(|a, b| {
            b.rating
                .partial_cmp(&a.rating)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.item.cmp(&b.item))
        });
        rows
    }

    /// Plain-dict state (oracle `toDict`).
    pub fn to_dict(&self) -> Value {
        let ratings: Map<String, Value> = self
            .ratings
            .iter()
            .map(|(k, v)| (k.clone(), jnum(*v)))
            .collect();
        let counts: Map<String, Value> = self
            .counts
            .iter()
            .map(|(k, v)| (k.clone(), json!(v)))
            .collect();
        json!({ "ratings": ratings, "counts": counts, "k": jnum(self.k) })
    }

    /// Rebuild from plain-dict state (oracle `fromDict` static).
    pub fn from_dict(data: Option<&Value>) -> EloLadder {
        let empty = Map::new();
        let d = match data {
            Some(Value::Object(m)) => m,
            _ => &empty,
        };
        let mut ratings: BTreeMap<String, f64> = BTreeMap::new();
        if let Some(Value::Object(m)) = d.get("ratings") {
            for (k, v) in m {
                if let Some(f) = v.as_f64() {
                    ratings.insert(k.clone(), f);
                }
            }
        }
        let mut counts: BTreeMap<String, f64> = BTreeMap::new();
        if let Some(Value::Object(m)) = d.get("counts") {
            for (k, v) in m {
                if let Some(f) = v.as_f64() {
                    counts.insert(k.clone(), f);
                }
            }
        }
        let k = d.get("k").and_then(Value::as_f64).unwrap_or(DEFAULT_ELO_K);
        EloLadder::new(Some(&ratings), Some(&counts), k)
    }
}

/// Batch Bradley-Terry fit result (oracle `bradleyTerry` return shape).
/// NOTE: the oracle's EMPTY-pairs result carries only
/// `strengths`/`n_pairs`/`iterations` — `to_json` mirrors that.
#[derive(Debug, Clone)]
pub struct BTResult {
    pub strengths: BTreeMap<String, f64>,
    pub counts: BTreeMap<String, i64>,
    pub n_pairs: usize,
    pub iterations: usize,
    pub algorithm: &'static str,
}

impl BTResult {
    pub fn to_json(&self) -> Value {
        if self.n_pairs == 0 {
            // oracle empty-shape: no counts, no algorithm key
            let strengths: Map<String, Value> = self
                .strengths
                .iter()
                .map(|(k, v)| (k.clone(), jnum(*v)))
                .collect();
            return json!({ "strengths": strengths, "n_pairs": 0, "iterations": 0 });
        }
        let strengths: Map<String, Value> = self
            .strengths
            .iter()
            .map(|(k, v)| (k.clone(), jnum(*v)))
            .collect();
        let counts: Map<String, Value> = self
            .counts
            .iter()
            .map(|(k, v)| (k.clone(), json!(v)))
            .collect();
        json!({
            "strengths": strengths,
            "counts": counts,
            "n_pairs": self.n_pairs,
            "iterations": self.iterations,
            "algorithm": self.algorithm,
        })
    }
}

const BT_ALGORITHM: &str = "bradley-terry logistic fit, deterministic gradient \
     ascent (Bradley & Terry 1952; the proposal's \
     verification plan governs the tests)";

/// Batch Bradley-Terry strengths from (winner, loser) pairs (oracle
/// `bradleyTerry`). One theta per item, logistic likelihood, deterministic
/// gradient ascent (fixed order, fixed iterations, adaptive step decay
/// `step / (1 + 0.02 * it)` applied AFTER each iteration's update — so the
/// first two iterations both run at `step`). Strengths are z-like scores
/// (mean 0).
pub fn bradley_terry(
    pairs: &[(String, String)],
    iterations: usize,
    step: f64,
) -> Result<BTResult, RankingError> {
    let pair_list: Vec<(String, String)> = pairs.to_vec();
    let mut counts: BTreeMap<String, i64> = BTreeMap::new();
    for (winner, loser) in &pair_list {
        if winner == loser {
            return Err(RankingError::SelfPlayItem(winner.clone()));
        }
        *counts.entry(winner.clone()).or_insert(0) += 1;
        *counts.entry(loser.clone()).or_insert(0) += 1;
    }
    let items: Vec<String> = counts.keys().cloned().collect();
    if pair_list.is_empty() {
        return Ok(BTResult {
            strengths: BTreeMap::new(),
            counts,
            n_pairs: 0,
            iterations: 0,
            algorithm: BT_ALGORITHM,
        });
    }
    let index: HashMap<&str, usize> = items
        .iter()
        .enumerate()
        .map(|(i, name)| (name.as_str(), i))
        .collect();
    let mut theta = vec![0.0f64; items.len()];
    let mut wins = vec![0.0f64; items.len()];
    let mut losses = vec![0.0f64; items.len()];
    for (winner, loser) in &pair_list {
        // index lookups are total: items are exactly the distinct pair names
        let i = index.get(winner.as_str()).copied().unwrap_or(usize::MAX);
        let j = index.get(loser.as_str()).copied().unwrap_or(usize::MAX);
        wins[i] += 1.0;
        losses[j] += 1.0;
    }
    let mut lr = step;
    for it in 0..iterations {
        // gradient of sum log sigmoid(theta_w - theta_l)
        let mut grad = vec![0.0f64; items.len()];
        for (winner, loser) in &pair_list {
            let i = index.get(winner.as_str()).copied().unwrap_or(usize::MAX);
            let j = index.get(loser.as_str()).copied().unwrap_or(usize::MAX);
            let p = 1.0 / (1.0 + (-(theta[i] - theta[j])).exp());
            grad[i] += 1.0 - p;
            grad[j] -= 1.0 - p;
        }
        for (i, slot) in theta.iter_mut().enumerate() {
            *slot += lr * grad[i];
        }
        lr = step / (1.0 + 0.02 * it as f64); // decay: stable, deterministic
    }
    // center on mean zero (strengths are relative)
    let mean = theta.iter().sum::<f64>() / theta.len() as f64;
    let mut strengths = BTreeMap::new();
    for name in &items {
        let i = index.get(name.as_str()).copied().unwrap_or(usize::MAX);
        strengths.insert(name.clone(), py_round(theta[i] - mean, 4));
    }
    Ok(BTResult {
        strengths,
        counts,
        n_pairs: pair_list.len(),
        iterations,
        algorithm: BT_ALGORITHM,
    })
}

/// Both views over the SAME pair records — the online ladder and the batch
/// fit — plus the agreement between them (Kendall tau), reported as numbers,
/// not claims (oracle `ladderReport`).
#[derive(Debug, Clone)]
pub struct LadderReport {
    pub ladder: Vec<LadderRow>,
    pub elo: Value,
    pub bradley_terry: BTResult,
    pub agreement_kendall_tau: Option<f64>,
    pub note: String,
}

impl LadderReport {
    pub fn to_json(&self) -> Value {
        json!({
            "ladder": self.ladder.iter().map(LadderRow::to_json).collect::<Vec<_>>(),
            "elo": self.elo,
            "bradley_terry": self.bradley_terry.to_json(),
            "agreement_kendall_tau": match self.agreement_kendall_tau {
                Some(t) => jnum(t),
                None => Value::Null,
            },
            "note": self.note,
        })
    }
}

pub fn ladder_report(
    pairs: &[(String, String)],
    elo: Option<EloLadder>,
) -> Result<LadderReport, RankingError> {
    let pair_list: Vec<(String, String)> = pairs.to_vec();
    let mut ladder = elo.unwrap_or_default();
    for (winner, loser) in &pair_list {
        ladder.observe(winner, loser)?;
    }
    let bt = bradley_terry(&pair_list, 200, 0.05)?;
    // Kendall tau between the two orderings over items with enough data
    let ranked: Vec<LadderRow> = ladder
        .ladder(None)
        .into_iter()
        .filter(|r| r.enough_data)
        .collect();
    let mut tau: Option<f64> = None;
    if ranked.len() >= 2 {
        let elo_order: Vec<String> = ranked.iter().map(|r| r.item.clone()).collect();
        let mut bt_rows = ranked.clone();
        bt_rows.sort_by(|a, b| {
            let sa = bt.strengths.get(&a.item).copied().unwrap_or(0.0);
            let sb = bt.strengths.get(&b.item).copied().unwrap_or(0.0);
            // JS comparator: (-(sa)) - (-(sb)) == sb - sa, strength DESC
            sb.partial_cmp(&sa).unwrap_or(Ordering::Equal)
        });
        let bt_order: Vec<String> = bt_rows.iter().map(|r| r.item.clone()).collect();
        let pos_in = |arr: &[String], v: &str| -> i64 {
            arr.iter().position(|x| x == v).map_or(-1, |p| p as i64)
        };
        let mut concordant = 0i64;
        let mut discordant = 0i64;
        for i in 0..elo_order.len() {
            for j in (i + 1)..elo_order.len() {
                // position lookups are total: both orders carry the same items
                let same = (pos_in(&elo_order, &elo_order[i]) - pos_in(&elo_order, &elo_order[j]))
                    * (pos_in(&bt_order, &elo_order[i]) - pos_in(&bt_order, &elo_order[j]));
                if same > 0 {
                    concordant += 1;
                } else if same < 0 {
                    discordant += 1;
                }
            }
        }
        let denom = concordant + discordant;
        if denom != 0 {
            tau = Some(py_round((concordant - discordant) as f64 / denom as f64, 3));
        }
    }
    Ok(LadderReport {
        ladder: ladder.ladder(None),
        elo: ladder.to_dict(),
        bradley_terry: bt,
        agreement_kendall_tau: tau,
        note: format!(
            "proposals only — nothing is applied; items under {MIN_COMPARISONS} \
             comparisons render 'not enough data'"
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(w, l)| ((*w).to_string(), (*l).to_string()))
            .collect()
    }

    #[test]
    fn self_play_is_rejected_with_repr_message() {
        let err = bradley_terry(&pairs(&[("same", "same")]), 200, 0.05).unwrap_err();
        assert_eq!(err.to_string(), "item 'same' cannot play itself");
        let err = bradley_terry(&pairs(&[("o'brien", "o'brien")]), 200, 0.05).unwrap_err();
        assert_eq!(err.to_string(), "item \"o'brien\" cannot play itself");
        let mut elo = EloLadder::default();
        assert_eq!(elo.observe("x", "x"), Err(RankingError::SelfPlay));
    }

    #[test]
    fn empty_pairs_shape_has_no_counts_key() {
        let bt = bradley_terry(&[], 200, 0.05).unwrap_or_else(|e| panic!("{e}"));
        let v = bt.to_json();
        assert!(v.get("counts").is_none(), "oracle empty shape omits counts");
        assert!(
            v.get("algorithm").is_none(),
            "oracle empty shape omits algorithm"
        );
        assert_eq!(v["n_pairs"], 0);
    }

    #[test]
    fn cycle_fit_centers_strengths_at_zero() {
        let bt = bradley_terry(&pairs(&[("a", "b"), ("b", "c"), ("c", "a")]), 200, 0.05)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(bt.n_pairs, 3);
        assert_eq!(bt.iterations, 200);
        let sum: f64 = bt.strengths.values().sum();
        assert!(sum.abs() < 1e-9, "mean-zero strengths, got {sum}");
    }

    #[test]
    fn ladder_rows_sort_rating_desc_then_item() {
        let mut elo = EloLadder::new(
            Some(&BTreeMap::from([
                ("b".into(), 1100.0),
                ("a".into(), 1300.0),
            ])),
            None,
            DEFAULT_ELO_K,
        );
        let rows = elo.ladder(None);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].item, "a");
        assert_eq!(rows[1].item, "b");
        assert!(!rows[0].enough_data);
    }

    #[test]
    fn counts_are_truncated_on_load() {
        let mut elo = EloLadder::new(
            Some(&BTreeMap::from([("p".into(), 1250.5)])),
            Some(&BTreeMap::from([("p".into(), 2.9)])),
            DEFAULT_ELO_K,
        );
        assert_eq!(elo.counts.get("p"), Some(&2));
        assert_eq!(elo.ratings.get("p"), Some(&1250.5));
        assert!(elo.observe("p", "p").is_err());
    }

    #[test]
    fn report_tau_agrees_for_consistent_orders() {
        // a dominates everything: both Elo and BT rank it first
        let report = ladder_report(
            &pairs(&[
                ("a", "b"),
                ("a", "c"),
                ("a", "b"),
                ("a", "c"),
                ("b", "c"),
                ("a", "b"),
            ]),
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(report.agreement_kendall_tau, Some(1.0));
        assert!(report.note.contains("not enough data"));
    }
}
