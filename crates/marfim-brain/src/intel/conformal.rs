//! Port of oracle `src/intel/conformal.js` — split-conformal calibration,
//! committee disagreement and incremental Page-Hinkley drift.
//!
//! Differential-port notes:
//! - `py_round` is inlined from oracle `src/intel/util/pymath.js::pyRound`
//!   (CPython banker's rounding over the EXACT binary double, originally
//!   BigInt; reimplemented over u128). Oracle call sites use digits in
//!   {2,3,4}, inside the helper's exact envelope.
//! - `Math.ceil`/`Math.sqrt`/`Math.min`/`Math.max` over finite arguments are
//!   elementary or correctly-rounded -> bit-exact. No transcendentals occur
//!   in this module (the population std-dev is a `sqrt`).
//! - JS truthiness for `votes[i] ?? 0.0` maps to null/missing -> 0.0.
//! - JS `Array.prototype.sort` with a numeric comparator is stable; Rust's
//!   `sort_by` is stable too, so tie order matches the oracle.

use serde_json::{json, Value};

/// Python `round(x, digits)` — banker's rounding over the EXACT binary value
/// of the double. Inlined from oracle `src/intel/util/pymath.js::pyRound`
/// (BigInt algorithm reimplemented over u128). Exact for the oracle's call
/// envelope (digits in [0, 20]; `-e > 127` rounds to +0.0; `digits < 0`
/// inside a documented sub-envelope; non-finite and ±0 pass through).
fn py_round(x: f64, digits: i32) -> f64 {
    if !x.is_finite() || x == 0.0 {
        return x;
    }
    let neg = x < 0.0;
    let bits = x.abs().to_bits();
    let biased = ((bits >> 52) & 0x7ff) as i64;
    let raw = bits & 0x000f_ffff_ffff_ffff;
    let (mant, e): (u128, i64) = if biased == 0 {
        (u128::from(raw), -1074)
    } else {
        (u128::from(raw | 0x0010_0000_0000_0000), biased - 1075)
    };
    if digits >= 0 {
        if e >= 0 || i64::from(digits) >= -e {
            return x;
        }
        if digits > 20 {
            return x; // outside the exact envelope; no oracle call site
        }
        if -e > 127 {
            return 0.0;
        }
        let num = mant * 10u128.pow(digits as u32);
        let den = 1u128 << (-e);
        let mut q = num / den;
        let r = num % den;
        let twice = r * 2;
        if twice > den || (twice == den && q % 2 == 1) {
            q += 1;
        }
        let sign = if neg && q != 0 { "-" } else { "" };
        if digits == 0 {
            return format!("{sign}{q}").parse().unwrap_or(x);
        }
        let ds = q.to_string();
        let padded = format!("{ds:0>width$}", width = digits as usize + 1);
        let cut = padded.len() - digits as usize;
        format!("{sign}{}.{}", &padded[..cut], &padded[cut..])
            .parse()
            .unwrap_or(x)
    } else {
        let nd = (-(i64::from(digits))) as u32;
        if nd > 30 {
            return x;
        }
        let (num, den): (u128, u128) = if e >= 0 {
            if e > 70 {
                return x;
            }
            (mant << e, 1)
        } else {
            if -e > 27 {
                return x;
            }
            (mant, 1u128 << (-e))
        };
        let den = den * 10u128.pow(nd);
        let mut q = num / den;
        let r = num % den;
        let twice = r * 2;
        if twice > den || (twice == den && q % 2 == 1) {
            q += 1;
        }
        let sign = if neg && q != 0 { "-" } else { "" };
        format!("{sign}{}", q * 10u128.pow(nd)).parse().unwrap_or(x)
    }
}

/// Split-conformal prediction: from a calibration set of (score, outcome)
/// pairs it derives the smallest score level s such that at least
/// (1 - alpha) of past accepted routes scored >= s.
#[derive(Debug, Clone)]
pub struct ConformalCalibrator {
    pub alpha: f64,
    pub max_history: usize,
    /// scores of ACCEPTED routes only
    scores: Vec<f64>,
    all_scores: Vec<f64>,
    all_outcomes: Vec<i64>, // 1 = accepted ('applied'/'approved'), else 0
}

impl Default for ConformalCalibrator {
    fn default() -> Self {
        // the oracle's constructor defaults (0.1, 400) — valid by
        // construction, so Default cannot fail
        Self {
            alpha: 0.1,
            max_history: 400,
            scores: Vec::new(),
            all_scores: Vec::new(),
            all_outcomes: Vec::new(),
        }
    }
}

impl ConformalCalibrator {
    /// `new ConformalCalibrator()` — oracle defaults (alpha 0.1, 400).
    pub fn new() -> Result<Self, String> {
        Self::with_options(0.1, 400)
    }

    /// `new ConformalCalibrator(alpha, maxHistory)`; errors carry the
    /// oracle's exact exception messages.
    pub fn with_options(alpha: f64, max_history: usize) -> Result<Self, String> {
        if !(0.0 < alpha && alpha < 1.0) {
            return Err("alpha in (0, 1)".to_string());
        }
        Ok(Self {
            alpha,
            max_history,
            scores: Vec::new(),
            all_scores: Vec::new(),
            all_outcomes: Vec::new(),
        })
    }

    /// outcome: 'applied' | 'approved' | 'rejected' | 'abstained'.
    pub fn observe(&mut self, score: f64, outcome: &str) -> &mut Self {
        self.all_scores.push(score);
        let accepted = outcome == "applied" || outcome == "approved";
        self.all_outcomes.push(if accepted { 1 } else { 0 });
        if accepted {
            self.scores.push(score);
        }
        if self.all_scores.len() > self.max_history {
            let overflow = self.all_scores.len() - self.max_history;
            self.all_scores.drain(0..overflow);
            self.all_outcomes.drain(0..overflow);
        }
        if self.scores.len() > self.max_history {
            let overflow = self.scores.len() - self.max_history;
            self.scores.drain(0..overflow);
        }
        self
    }

    /// Conformal quantile: ceil((n+1)(1-alpha))/n empirical quantile of
    /// accepted-route scores. None when there is not enough data (< 5).
    pub fn threshold(&self, alpha: Option<f64>) -> Option<f64> {
        let a = alpha.unwrap_or(self.alpha);
        let n = self.scores.len();
        if n < 5 {
            return None;
        }
        let mut sorted = self.scores.clone();
        sorted.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
        let rank = ((n as f64 + 1.0) * (1.0 - a)).ceil();
        // Math.min(n, Math.max(1, ceil)) — equivalent to the JS nesting
        let rank = (rank as usize).min(n).max(1);
        sorted.get(rank - 1).copied()
    }

    pub fn verdict(&self, score: f64, alpha: Option<f64>) -> Value {
        let t = self.threshold(alpha);
        let a = alpha.unwrap_or(self.alpha);
        let n = self.scores.len();
        match t {
            None => json!({
                "covered": Value::Null,
                "guarantee": Value::Null,
                "reason": format!(
                    "insufficient calibration data ({n} accepted routes; need >= 5) — no distribution-free claim available"
                ),
                "score": py_round(score, 4),
                "alpha": a,
            }),
            Some(threshold) => {
                let covered = score >= threshold;
                json!({
                    "covered": covered,
                    "guarantee": format!(
                        "routes at or above {} were accepted >= {} of the time historically (distribution-free, n={n})",
                        py_round(threshold, 3),
                        py_round(1.0 - a, 2),
                    ),
                    "reason": if covered {
                        "score clears the conformal threshold"
                    } else {
                        "score below the conformal threshold"
                    },
                    "score": py_round(score, 4),
                    "threshold": py_round(threshold, 4),
                    "alpha": a,
                })
            }
        }
    }

    pub fn empirical_accuracy(&self, min_score: f64) -> Option<f64> {
        let mut count = 0usize;
        let mut sum = 0.0f64;
        for i in 0..self.all_scores.len() {
            if self.all_scores[i] >= min_score {
                sum += self.all_outcomes[i] as f64;
                count += 1;
            }
        }
        if count == 0 {
            None
        } else {
            Some(sum / f64::from(count as u32))
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "alpha": self.alpha,
            "scores": self.scores,
            "all_scores": self.all_scores,
            "all_outcomes": self.all_outcomes,
        })
    }

    pub fn from_json(&mut self, d: &Value) -> &mut Self {
        if let Some(v) = d.get("alpha").and_then(Value::as_f64) {
            self.alpha = v;
        }
        let nums = |key: &str| -> Vec<f64> {
            d.get(key)
                .and_then(Value::as_array)
                .map(|a| a.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect())
                .unwrap_or_default()
        };
        self.scores = nums("scores");
        self.all_scores = nums("all_scores");
        self.all_outcomes = d
            .get("all_outcomes")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|v| {
                        v.as_i64()
                            .or_else(|| v.as_f64().map(|f| f.trunc() as i64))
                            .unwrap_or(0)
                    })
                    .collect()
            })
            .unwrap_or_default();
        self
    }
}

/// Rank the requests worth TEACHING on (active learning). Disagreement is
/// the population std-dev of the members' raw scores (NOT vote entropy).
/// `candidates` are JSON objects (at least {"text": ...}); `committee_votes`
/// are one JSON array or object per strategy mapping candidate INDEX ->
/// raw score. `Err` carries the oracle's exact message on a length mismatch.
pub fn query_by_committee(
    candidates: &[Value],
    committee_votes: &[Value],
    top_k: usize,
) -> Result<Vec<Value>, String> {
    let first = committee_votes
        .first()
        .cloned()
        .unwrap_or_else(|| json!({}));
    let vote_count = |votes: &Value| -> usize {
        match votes {
            Value::Array(a) => a.len(),
            Value::Object(m) => m.len(),
            _ => 0, // JS Object.keys(primitive) is [] — no votes counted
        }
    };
    if candidates.len() != vote_count(&first) {
        return Err("votes must score every candidate".to_string());
    }
    let mut ranked: Vec<(f64, Value)> = Vec::new();
    for (i, cand) in candidates.iter().enumerate() {
        let mut scores: Vec<f64> = Vec::with_capacity(committee_votes.len());
        for votes in committee_votes {
            let raw = match votes {
                Value::Array(a) => a.get(i).cloned(),
                Value::Object(m) => m.get(&i.to_string()).cloned(),
                _ => None,
            };
            // JS: votes[i] ?? 0.0 — null and undefined both become 0.0
            scores.push(match raw {
                None | Some(Value::Null) => 0.0,
                Some(v) => v.as_f64().unwrap_or(0.0),
            });
        }
        let n = scores.len();
        let mean = scores.iter().sum::<f64>() / n as f64;
        let mut var_pop = 0.0f64;
        for v in &scores {
            var_pop += (v - mean) * (v - mean);
        }
        var_pop /= n as f64;
        let disagreement = var_pop.sqrt();
        let mut row = cand.clone();
        if let Some(obj) = row.as_object_mut() {
            obj.insert("disagreement".to_string(), json!(py_round(disagreement, 4)));
            obj.insert("mean_score".to_string(), json!(py_round(mean, 4)));
        }
        ranked.push((disagreement, row));
    }
    // stable on ties, exactly like the JS sort
    ranked.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    Ok(ranked
        .into_iter()
        .take(top_k)
        .map(|(_, mut row)| {
            if let Some(obj) = row.as_object_mut() {
                obj.insert(
                    "why".to_string(),
                    json!("strategies disagree — labelling this buys the most information for the router"),
                );
            }
            row
        })
        .collect())
}

/// Incremental Page-Hinkley over the accept/reject stream, oriented to detect
/// a DROP in acceptance: the standard upward PH on the negated stream
/// (x' = 1 - x). One alarm LATCHES (alarm_at set) until `reset`.
#[derive(Debug, Clone)]
pub struct PageHinkleyDrift {
    pub threshold: f64,
    pub alpha: f64,
    pub delta: f64,
    pub warmup: i64,
    n: i64,
    mean: f64, // EMA of the rejection stream
    sum_xt: f64,
    min_ht: f64, // f64::INFINITY until the first observation
    alarm_at: Option<i64>,
}

impl Default for PageHinkleyDrift {
    fn default() -> Self {
        // the oracle's constructor defaults (8.0, 0.98, 0.01, 25)
        Self {
            threshold: 8.0,
            alpha: 0.98,
            delta: 0.01,
            warmup: 25,
            n: 0,
            mean: 0.0,
            sum_xt: 0.0,
            min_ht: f64::INFINITY,
            alarm_at: None,
        }
    }
}

impl PageHinkleyDrift {
    /// `new PageHinkleyDrift()` — oracle defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// `new PageHinkleyDrift(threshold, alpha, delta, warmup)`.
    pub fn with_options(threshold: f64, alpha: f64, delta: f64, warmup: i64) -> Self {
        Self {
            threshold,
            alpha,
            delta,
            warmup,
            ..Self::default()
        }
    }

    pub fn update(&mut self, accepted: bool) -> Value {
        let x = if accepted { 0.0 } else { 1.0 }; // negated stream: reject == 1
        if self.n == 0 {
            self.mean = x;
        }
        self.n += 1;
        self.mean = self.alpha * self.mean + (1.0 - self.alpha) * x;
        self.sum_xt += x - self.mean - self.delta;
        self.min_ht = self.min_ht.min(self.sum_xt);
        let stat = self.sum_xt - self.min_ht;
        let alarmed = self.n >= self.warmup && stat > self.threshold && self.alarm_at.is_none();
        if alarmed {
            self.alarm_at = Some(self.n);
        }
        json!({ "n": self.n, "stat": py_round(stat, 4), "alarmed": alarmed })
    }

    pub fn reset(&mut self) -> &mut Self {
        self.alarm_at = None;
        self.n = 0;
        self.sum_xt = 0.0;
        self.min_ht = f64::INFINITY;
        self.mean = 0.0;
        self
    }

    pub fn status(&self) -> Value {
        let stat = self.sum_xt - self.min_ht;
        json!({
            "n": self.n,
            "statistic": py_round(stat, 4),
            "threshold": self.threshold,
            "alarmed": self.alarm_at.is_some(),
            "alarm_at": self.alarm_at,
            "message": if self.alarm_at.is_some() {
                "acceptance rate has DROPPED — your preferences seem to have shifted; weights will adapt, but a cortex reset-learning is available"
            } else {
                "no drift detected"
            },
        })
    }

    pub fn n(&self) -> i64 {
        self.n
    }

    pub fn alarm_at(&self) -> Option<i64> {
        self.alarm_at
    }

    pub fn to_json(&self) -> Value {
        json!({
            "threshold": self.threshold,
            "alpha": self.alpha,
            "delta": self.delta,
            "warmup": self.warmup,
            "n": self.n,
            "mean": self.mean,
            "sum_xt": self.sum_xt,
            "min_ht": if self.min_ht == f64::INFINITY { Value::Null } else { json!(self.min_ht) },
            "alarm_at": self.alarm_at,
        })
    }

    pub fn from_json(&mut self, d: &Value) -> &mut Self {
        if let Some(v) = d.get("threshold").and_then(Value::as_f64) {
            self.threshold = v;
        }
        if let Some(v) = d.get("alpha").and_then(Value::as_f64) {
            self.alpha = v;
        }
        if let Some(v) = d.get("delta").and_then(Value::as_f64) {
            self.delta = v;
        }
        if let Some(v) = d.get("warmup").and_then(Value::as_i64) {
            self.warmup = v;
        }
        self.n = d.get("n").and_then(Value::as_i64).unwrap_or(0);
        // oracle quirk preserved: a MISSING mean restores to 1.0, not 0.0
        self.mean = d.get("mean").and_then(Value::as_f64).unwrap_or(1.0);
        self.sum_xt = d.get("sum_xt").and_then(Value::as_f64).unwrap_or(0.0);
        self.min_ht = match d.get("min_ht") {
            Some(v) if !v.is_null() => v.as_f64().unwrap_or(f64::INFINITY),
            _ => f64::INFINITY,
        };
        self.alarm_at = match d.get("alarm_at") {
            Some(v) if !v.is_null() => v.as_i64().or_else(|| v.as_f64().map(|f| f.trunc() as i64)),
            _ => None,
        };
        self
    }
}

impl ConformalCalibrator {
    /// G1 helper for non-test callers that need an infallible default:
    /// the zero-arg constructor's only failure mode is a config error,
    /// which cannot occur for the default options — an (impossible) Err
    /// degrades to the bare struct rather than panicking (D6).
    pub fn default_or_empty() -> Self {
        match Self::new() {
            Ok(c) => c,
            Err(_) => Self {
                alpha: 0.1,
                max_history: 512,
                scores: Vec::new(),
                all_scores: Vec::new(),
                all_outcomes: Vec::new(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::ulp_close;

    #[test]
    fn empty_calibrator_has_no_threshold() {
        let c = ConformalCalibrator::default();
        assert_eq!(c.threshold(None), None);
        let v = c.verdict(0.5, None);
        assert_eq!(v["covered"], Value::Null);
        assert!(v["reason"].as_str().unwrap_or("").contains("need >= 5"));
    }

    #[test]
    fn threshold_at_five_observations() {
        let mut c = ConformalCalibrator::new().unwrap_or_else(|e| panic!("{e}"));
        for s in [0.9, 0.4, 0.7, 0.2, 0.6] {
            c.observe(s, "applied");
        }
        // rank = ceil(6 * 0.9) = 6 -> min(5) = 5 -> 5th sorted score = 0.9
        assert_eq!(c.threshold(None), Some(0.9));
        let v = c.verdict(0.95, None);
        assert_eq!(v["covered"], true);
    }

    #[test]
    fn history_trims_to_max() {
        let mut c = ConformalCalibrator::with_options(0.1, 5).unwrap_or_else(|e| panic!("{e}"));
        for s in [0.9, 0.8, 0.7, 0.6, 0.5, 0.4, 0.3] {
            c.observe(s, "applied");
        }
        let d = c.to_json();
        assert_eq!(d["scores"].as_array().map_or(0, Vec::len), 5);
        assert_eq!(d["all_scores"].as_array().map_or(0, Vec::len), 5);
        assert_eq!(d["all_scores"][0], 0.7);
    }

    #[test]
    fn qbc_empty_and_mismatch() {
        assert!(query_by_committee(&[], &[], 3)
            .unwrap_or_default()
            .is_empty());
        let cands = vec![json!({"text": "a"})];
        let votes = vec![json!([0.5])]; // 1 vote container, 1 candidate -> ok
        assert_eq!(
            query_by_committee(&cands, &votes, 1)
                .unwrap_or_default()
                .len(),
            1
        );
        let votes_bad = vec![json!([0.5, 0.6])]; // container scores 2 candidates vs 1
        let err = query_by_committee(&cands, &votes_bad, 1).unwrap_err();
        assert_eq!(err, "votes must score every candidate");
    }

    #[test]
    fn ph_first_update_and_no_alarm_under_warmup() {
        let mut p = PageHinkleyDrift::with_options(2.0, 0.98, 0.01, 100);
        let rep = p.update(false); // reject -> x = 1
        assert_eq!(rep["n"], 1);
        assert!(ulp_close(rep["stat"].as_f64().unwrap_or(0.0), 0.0));
        assert_eq!(rep["alarmed"], false);
        for _ in 0..50 {
            p.update(false);
        }
        assert!(p.alarm_at().is_none(), "warmup prevents early alarm");
    }

    #[test]
    fn ph_alarms_on_drop_after_warmup() {
        let mut p = PageHinkleyDrift::default();
        for _ in 0..40 {
            p.update(true);
        }
        for _ in 0..30 {
            p.update(false);
        }
        assert!(p.alarm_at().is_some(), "acceptance drop must alarm");
        p.reset();
        assert!(p.alarm_at().is_none());
        assert_eq!(p.n(), 0);
    }

    #[test]
    fn ph_missing_mean_quirk() {
        // the oracle restores mean to 1.0 when the key is absent
        let mut p = PageHinkleyDrift::default();
        p.from_json(&json!({}));
        let d = p.to_json();
        assert!(ulp_close(d["mean"].as_f64().unwrap_or(0.0), 1.0));
    }
}
