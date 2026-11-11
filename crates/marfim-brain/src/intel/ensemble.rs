//! Port of oracle `src/intel/ensemble.js` — the voter combination rule
//! (Dawid-Skene fused posterior) and Learn++.NSE (Elwell & Polikar 2011).
//!
//! Differential-port notes:
//! - `py_round` is inlined from oracle `src/intel/util/pymath.js::pyRound`
//!   (CPython banker's rounding over the EXACT binary double; reimplemented
//!   over u128). Oracle call sites use digit counts in {4,6}, inside the
//!   helper's exact envelope.
//! - TRANSCENDENTALS (validated at <=1-ulp tolerance by the committed
//!   fixtures `fixtures/intel/ensemble.jsonl`, per `crate::util::ulp_close`):
//!   `Math.log` (combine_votes), `Math.exp` (mathExp/sigmoid) and
//!   `Math.pow(2.718281828459045, ·)` (combine_votes softmax — mirrored
//!   literally as `f64::consts::E.powf`, which holds the same double bits as
//!   the oracle's literal).
//! - The oracle accumulates voter log-probabilities in JS-object insertion
//!   order; the port iterates a BTreeMap (alphabetical). The committed
//!   fixtures therefore list vote keys alphabetically, making both orders
//!   identical (documented in scripts/fixture-ensemble.js).
//! - JS truthiness (`Boolean(value)`, `outcomes[i] ? 1 : 0`) maps to
//!   NaN/±0 -> false; `z = z || 1.0` also catches NaN.
//! - `Math.trunc` maps to `f64::trunc`.

use serde_json::{json, Map, Value};

use crate::intel::adwin::DriftConsensus;

/// The paper's epsilon-tilde floor on member weights.
pub const EPS_FLOOR: f64 = 0.01;
pub const DEFAULT_BLOCK: i64 = 25;
pub const DEFAULT_WINDOW: i64 = 40;
pub const DEFAULT_MAX_MEMBERS: i64 = 8;

/// The 6 voters (the LSA/SVD voter was CUT — measured no-op).
pub const VOTER_KEYS: [&str; 6] = ["bm25", "ppmi", "fuzz", "coverage", "bigram", "cue"];

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

/// JS `Math.min`/`Math.max` poison on NaN (unlike Rust's f64::min/max).
fn js_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a > b {
        a
    } else {
        b
    }
}

/// exp clamped to ±700 (the paper-port's numeric guard).
pub fn math_exp(z: f64) -> f64 {
    // JS: Math.exp(Math.max(-700, Math.min(700, z))) — clamp keeps the same
    // min->max order and lets NaN flow through (clamp returns NaN for NaN)
    // Math.exp: transcendental — <=1-ulp tolerance (documented)
    z.clamp(-700.0, 700.0).exp()
}

/// Combined fused posterior over the candidate list given one query's votes
/// {voter: surface-or-None} and the Dawid-Skene model {sensitivity,
/// false_mass}. Missing/None votes are skipped everywhere. Unknown voters
/// default to `sens_default` / `fmass_default`. Deterministic; softmax with
/// the literal e, max-shifted, exactly like the oracle.
pub fn combine_votes(
    candidates: &[String],
    votes: &Value,
    model: &Value,
    sens_default: f64,
    fmass_default: f64,
) -> Value {
    let empty = json!({});
    let sens = model.get("sensitivity").unwrap_or(&empty);
    let fmass = model.get("false_mass").unwrap_or(&empty);
    let mut logp: Vec<f64> = vec![0.0; candidates.len()];
    let mut n_votes = 0i64;
    if let Some(map) = votes.as_object() {
        for (v, vote) in map {
            // JS: vote must be non-null and one of the candidates
            let Some(vote_str) = vote.as_str() else {
                continue;
            };
            if !candidates.iter().any(|c| c == vote_str) {
                continue;
            }
            n_votes += 1;
            let sv = sens.get(v).and_then(Value::as_f64).unwrap_or(sens_default);
            let fv = fmass
                .get(v)
                .and_then(Value::as_f64)
                .unwrap_or(fmass_default);
            for (ci, c) in candidates.iter().enumerate() {
                let p = if c == vote_str {
                    sv
                } else {
                    fv / js_max(1.0, candidates.len() as f64 - 1.0)
                };
                // Math.log: transcendental — <=1-ulp tolerance (documented)
                logp[ci] += js_max(p, 1e-12).ln();
            }
        }
    }
    let m = if candidates.is_empty() {
        0.0
    } else {
        logp.iter().copied().fold(f64::NEG_INFINITY, js_max)
    };
    let mut z = 0.0f64;
    let mut exps: Vec<f64> = Vec::with_capacity(candidates.len());
    for lp in &logp {
        // Math.pow(2.718281828459045, ·): transcendental — <=1-ulp
        // tolerance (documented); f64::consts::E is the same double
        let e = std::f64::consts::E.powf(lp - m);
        exps.push(e);
        z += e;
    }
    // JS `z = z || 1.0`: 0 and NaN both fall back to 1
    let z = if z == 0.0 || z.is_nan() { 1.0 } else { z };
    let mut posterior = Map::new();
    for (i, c) in candidates.iter().enumerate() {
        posterior.insert(c.clone(), json!(exps[i] / z));
    }
    let top = candidates.first();
    let mut backing = 0i64;
    if let Some(map) = votes.as_object() {
        for vote in map.values() {
            if let Some(vs) = vote.as_str() {
                if top.map(String::as_str) == Some(vs) {
                    backing += 1;
                }
            }
        }
    }
    json!({
        "posterior": Value::Object(posterior),
        "agreement": if n_votes > 0 {
            json!(backing as f64 / n_votes as f64)
        } else {
            json!(0)
        },
        "nVotes": n_votes,
    })
}

#[derive(Debug, Clone)]
pub struct Member {
    pub weights: Vec<f64>,
    pub bias: f64,
    pub born_at: i64,
}

/// One frozen member snapshot: (features, truncated label).
pub type Row = (Vec<f64>, i64);

/// Learn++.NSE (Elwell & Polikar 2011): an ensemble of experts born at
/// different times, re-weighted by their error on a RECENT window. Members
/// are frozen weight-vector logistic snapshots taken every `block` examples.
/// Fixed-iteration gradient descent — no RNG, fully deterministic.
#[derive(Debug, Clone)]
pub struct LearnPPNSE {
    pub n_features: i64,
    pub block: i64,
    pub window: i64,
    pub max_members: i64,
    pub min_weight: f64,
    members: Vec<Member>,
    recent: Vec<Row>,
    n_seen: i64,
    pending: Vec<Row>,
}

impl LearnPPNSE {
    /// `new LearnPPNSE(nFeatures)` with the oracle's default options.
    pub fn new(n_features: i64) -> Result<Self, String> {
        Self::with_options(
            n_features,
            DEFAULT_BLOCK,
            DEFAULT_WINDOW,
            DEFAULT_MAX_MEMBERS,
            0.02,
        )
    }

    /// `new LearnPPNSE(nFeatures, {block, window, maxMembers, minWeight})`.
    pub fn with_options(
        n_features: i64,
        block: i64,
        window: i64,
        max_members: i64,
        min_weight: f64,
    ) -> Result<Self, String> {
        if n_features < 1 {
            return Err("n_features must be >= 1".to_string());
        }
        if block < 1 || window < 1 || max_members < 1 {
            return Err("block/window/max_members must be >= 1".to_string());
        }
        if !(0.0..1.0).contains(&min_weight) {
            return Err("min_weight must be in [0, 1)".to_string());
        }
        Ok(Self {
            n_features,
            block,
            window,
            max_members,
            min_weight,
            members: Vec::new(),
            recent: Vec::new(),
            n_seen: 0,
            pending: Vec::new(),
        })
    }

    fn sigmoid(z: f64) -> f64 {
        if z >= 0.0 {
            1.0 / (1.0 + math_exp(-z))
        } else {
            let ez = math_exp(z);
            ez / (1.0 + ez)
        }
    }

    pub fn members(&self) -> &[Member] {
        &self.members
    }

    pub fn n_seen(&self) -> i64 {
        self.n_seen
    }

    pub fn predict_member(&self, member: &Member, features: &[f64]) -> f64 {
        let mut z = member.bias;
        for (i, w) in member.weights.iter().enumerate() {
            // JS: Number(features[i] ?? 0.0)
            z += w * features.get(i).copied().unwrap_or(0.0);
        }
        Self::sigmoid(z)
    }

    /// The ensemble's weighted-average probability; None before the first
    /// block completes (no experts, no invented prediction).
    pub fn predict(&self, features: &[f64]) -> Option<f64> {
        if self.members.is_empty() {
            return None;
        }
        let weights = self.member_weights();
        let total: f64 = weights.iter().sum();
        if total <= 0.0 {
            return None;
        }
        let mut acc = 0.0f64;
        for (i, member) in self.members.iter().enumerate() {
            acc += weights[i] * self.predict_member(member, features);
        }
        Some(acc / total)
    }

    /// One labeled example. Every `block` examples the CURRENT pending batch
    /// trains a FRESH binary logistic -> frozen member; the oldest member is
    /// dropped when over max_members (age, never accuracy).
    pub fn observe(&mut self, features: &[f64], label: f64) -> Value {
        let row: Row = (features.to_vec(), label.trunc() as i64);
        self.recent.push(row.clone());
        self.pending.push(row);
        self.n_seen += 1;
        let mut born = false;
        if self.pending.len() >= self.block as usize {
            born = true;
            let (weights, bias) = Self::train_member(&self.pending);
            self.members.push(Member {
                weights,
                bias,
                born_at: self.n_seen,
            });
            self.pending.clear();
            if self.members.len() > self.max_members as usize {
                self.members.remove(0); // age out the OLDEST — deterministic
            }
            self.reweight();
        }
        json!({ "n_seen": self.n_seen, "members": self.members.len(), "born": born })
    }

    /// A fresh binary logistic per block: fixed-iteration gradient descent
    /// (rate 0.3, 150 epochs, batch-mean gradients, weights/bias rounded 6,
    /// no RNG). Frozen at birth, never updated again.
    pub fn train_member(rows: &[Row]) -> (Vec<f64>, f64) {
        let width = rows.first().map_or(0, |r| r.0.len());
        let mut w = vec![0.0f64; width];
        let mut b = 0.0f64;
        let rate = 0.3f64;
        for _epoch in 0..150 {
            let mut gw = vec![0.0f64; width];
            let mut gb = 0.0f64;
            for (feats, label) in rows {
                let mut z = b;
                for (j, wj) in w.iter().enumerate() {
                    z += wj * feats.get(j).copied().unwrap_or(0.0);
                }
                let p = Self::sigmoid(z);
                let err = p - *label as f64;
                // JS: gw[j] += err * feats[j] (missing features are 0.0 —
                // zipping adds nothing for the tail, which is equivalent)
                for (gw_j, feat_j) in gw.iter_mut().zip(feats.iter()) {
                    *gw_j += err * feat_j;
                }
                gb += err;
            }
            let n = rows.len() as f64;
            for j in 0..width {
                w[j] -= rate * gw[j] / n;
            }
            b -= rate * gb / n;
        }
        (
            w.iter().map(|wi| py_round(*wi, 6)).collect(),
            py_round(b, 6),
        )
    }

    /// w_k ~ 1/(windowed error + eps_floor), floored at min_weight,
    /// L1-normalized. Errors recomputed EVERY call from the kept tail — no
    /// stale cached weights. Empty tail -> raw 1.0 per member.
    pub fn member_weights(&self) -> Vec<f64> {
        if self.members.is_empty() {
            return Vec::new();
        }
        let skip = self.recent.len().saturating_sub(self.window as usize);
        let tail = &self.recent[skip..];
        let mut raw: Vec<f64> = Vec::with_capacity(self.members.len());
        for member in &self.members {
            if tail.is_empty() {
                raw.push(1.0);
                continue;
            }
            let mut err = 0.0f64;
            for (feats, label) in tail {
                let p = self.predict_member(member, feats);
                err += (p - *label as f64).abs(); // L1 loss on probabilities
            }
            err /= tail.len() as f64;
            raw.push(1.0 / (err + EPS_FLOOR));
        }
        let total: f64 = raw.iter().sum();
        if total <= 0.0 {
            return vec![1.0 / raw.len() as f64; raw.len()];
        }
        // Math.max(min_weight, r / total)
        let weights: Vec<f64> = raw
            .iter()
            .map(|r| js_max(self.min_weight, r / total))
            .collect();
        let norm: f64 = weights.iter().sum();
        weights.iter().map(|w| w / norm).collect()
    }

    /// The oracle recomputes weights here without persisting them
    /// (member_weights is derived state) — kept for API parity.
    pub fn reweight(&mut self) -> &mut Self {
        let _ = self.member_weights();
        self
    }

    pub fn to_json(&self) -> Value {
        let skip = self.recent.len().saturating_sub(self.window as usize);
        json!({
            "n_features": self.n_features,
            "block": self.block,
            "window": self.window,
            "max_members": self.max_members,
            "min_weight": self.min_weight,
            "members": self.members.iter().map(|m| json!({
                "weights": m.weights,
                "bias": m.bias,
                "born_at": m.born_at,
            })).collect::<Vec<_>>(),
            "recent": self.recent[skip..].iter().map(|(f, l)| json!([f, l])).collect::<Vec<_>>(),
            "n_seen": self.n_seen,
        })
    }

    /// The oracle's `static fromDict(d)` — revalidates through the
    /// constructor (the Math.trunc(...) || 1 guards included).
    pub fn from_json(d: &Value) -> Result<Self, String> {
        let num = |key: &str, default: f64| -> f64 {
            d.get(key).and_then(Value::as_f64).unwrap_or(default)
        };
        // JS: Math.trunc(Number(data.n_features ?? 1)) || 1
        let nf_trunc = num("n_features", 1.0).trunc();
        let n_features = if nf_trunc == 0.0 || nf_trunc.is_nan() {
            1
        } else {
            nf_trunc as i64
        };
        let block = num("block", DEFAULT_BLOCK as f64).trunc() as i64;
        let window = num("window", DEFAULT_WINDOW as f64).trunc() as i64;
        let max_members = num("max_members", DEFAULT_MAX_MEMBERS as f64).trunc() as i64;
        let min_weight = num("min_weight", 0.02);
        let mut ens = Self::with_options(n_features, block, window, max_members, min_weight)?;
        if let Some(arr) = d.get("members").and_then(Value::as_array) {
            for m in arr {
                let weights: Vec<f64> = m
                    .get("weights")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect())
                    .unwrap_or_default();
                let bias = m.get("bias").and_then(Value::as_f64).unwrap_or(0.0);
                let born_at = m
                    .get("born_at")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0)
                    .trunc() as i64;
                ens.members.push(Member {
                    weights,
                    bias,
                    born_at,
                });
            }
        }
        if let Some(arr) = d.get("recent").and_then(Value::as_array) {
            for r in arr {
                let feats: Vec<f64> = r
                    .get(0)
                    .and_then(Value::as_array)
                    .map(|a| a.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect())
                    .unwrap_or_default();
                let label = r.get(1).and_then(Value::as_f64).unwrap_or(0.0).trunc() as i64;
                ens.recent.push((feats, label));
            }
        }
        ens.n_seen = num("n_seen", 0.0).trunc() as i64;
        Ok(ens)
    }
}

/// compare_with_adwin: the honest side-by-side — the same stream through
/// (a) the ADWIN drift consensus and (b) the NSE ensemble's weight
/// trajectory. Reports both verdicts; never declares a winner.
pub fn compare_with_adwin(stream: &[f64], block: i64) -> Result<Value, String> {
    let mut consensus_run = DriftConsensus::default();
    for &value in stream {
        // JS Boolean(value): NaN and ±0 are false, everything else true
        consensus_run.update(!value.is_nan() && value != 0.0);
    }
    let adwin_status = consensus_run.status();

    let window = std::cmp::max(10, (stream.len() / 4) as i64);
    let mut ens = LearnPPNSE::with_options(1, block, window, DEFAULT_MAX_MEMBERS, 0.02)?;
    let mut trajectory: Vec<Value> = Vec::new();
    for &value in stream {
        let row = ens.observe(&[value], value);
        if row.get("born").and_then(Value::as_bool) == Some(true) {
            trajectory.push(json!({
                "at": ens.n_seen(),
                "members": ens.members().len(),
                "weights": ens.member_weights().iter().map(|w| py_round(*w, 4)).collect::<Vec<_>>(),
            }));
        }
    }
    Ok(json!({
        "n": stream.len(),
        "adwin": {
            "flagged": adwin_status.get("flag_drift").cloned().unwrap_or(Value::Bool(false)),
            "summary": adwin_status.get("summary").cloned().unwrap_or(Value::Null),
        },
        "nse": {
            "members": ens.members().len(),
            "weights": ens.member_weights().iter().map(|w| py_round(*w, 4)).collect::<Vec<_>>(),
            "trajectory": trajectory,
        },
        "note": "two philosophies on one stream: detect-and-reset (ADWIN consensus) vs continuous re-weighting (NSE). No winner is declared from one stream — run the regret-vs-best-fixed audit (brain/regret.py) for the policy comparison",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::ulp_close;

    #[test]
    fn consts_match_oracle() {
        assert!(ulp_close(EPS_FLOOR, 0.01));
        assert_eq!(DEFAULT_BLOCK, 25);
        assert_eq!(DEFAULT_WINDOW, 40);
        assert_eq!(DEFAULT_MAX_MEMBERS, 8);
        assert_eq!(
            VOTER_KEYS,
            ["bm25", "ppmi", "fuzz", "coverage", "bigram", "cue"]
        );
    }

    #[test]
    fn math_exp_clamps() {
        assert!(ulp_close(math_exp(0.0), 1.0));
        assert!(ulp_close(math_exp(-0.0), 1.0));
        assert!(ulp_close(math_exp(800.0), math_exp(700.0)));
        assert!(ulp_close(math_exp(-800.0), math_exp(-700.0)));
        assert!(math_exp(-750.0) > 0.0);
    }

    #[test]
    fn combine_empty_and_single() {
        let empty = json!({});
        let out = combine_votes(&[], &empty, &empty, 0.5, 0.1);
        assert_eq!(out["nVotes"], 0);
        assert_eq!(out["agreement"], 0);
        let out = combine_votes(
            &["only".to_string()],
            &json!({"bm25": "only"}),
            &empty,
            0.5,
            0.1,
        );
        assert!(ulp_close(
            out["posterior"]["only"].as_f64().unwrap_or(0.0),
            1.0
        ));
        assert_eq!(out["agreement"], 1.0);
    }

    #[test]
    fn abstaining_voter_never_counts() {
        let model = json!({"sensitivity": {"bm25": 0.9}, "false_mass": {"bm25": 0.02}});
        let out = combine_votes(
            &["a".to_string(), "b".to_string()],
            &json!({"bm25": Value::Null}),
            &model,
            0.5,
            0.1,
        );
        assert_eq!(out["nVotes"], 0);
        assert_eq!(out["agreement"], 0);
    }

    #[test]
    fn nse_predicts_null_before_first_block() {
        let mut ens = LearnPPNSE::new(1).unwrap_or_else(|e| panic!("{e}"));
        for _ in 0..10 {
            ens.observe(&[0.5], 1.0);
        }
        assert!(ens.members().is_empty());
        assert_eq!(ens.predict(&[0.5]), None);
        assert!(ens.member_weights().is_empty());
    }

    #[test]
    fn nse_single_row_training() {
        let (w, b) = LearnPPNSE::train_member(&[(vec![2.0], 1)]);
        assert_eq!(w.len(), 1);
        assert!(ulp_close(b, 1.082_568));
        assert!(ulp_close(w[0], 2.165_137));
    }

    #[test]
    fn nse_evicts_oldest_member() {
        let mut ens = LearnPPNSE::with_options(1, 2, 10, 2, 0.02).unwrap_or_else(|e| panic!("{e}"));
        for f in [0.1, 0.2, 0.8, 0.9, 0.3, 0.4] {
            ens.observe(&[f], if f > 0.5 { 1.0 } else { 0.0 });
        }
        assert_eq!(ens.members().len(), 2); // max_members evicted the oldest
        assert_eq!(ens.members()[0].born_at, 4);
    }

    #[test]
    fn nse_ctor_validation() {
        assert_eq!(LearnPPNSE::new(0).unwrap_err(), "n_features must be >= 1");
        assert_eq!(
            LearnPPNSE::with_options(1, 0, 10, 8, 0.02).unwrap_err(),
            "block/window/max_members must be >= 1"
        );
        assert_eq!(
            LearnPPNSE::with_options(1, 10, 10, 8, 1.0).unwrap_err(),
            "min_weight must be in [0, 1)"
        );
    }
}
