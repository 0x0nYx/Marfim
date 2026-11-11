//! Port of oracle `src/intel/brain/pooling.js` — hierarchical partial pooling
//! for Beta posteriors (Efron & Morris 1975; the empirical-Bayes shrinkage
//! idea). Every preference posterior starts from a flat Beta(1, 1) prior; a
//! NEW arm starts from the pool's method-of-moments hierarchical prior and
//! escapes it as its own evidence accumulates.
//!
//! DETERMINISTIC: pure method of moments — no RNG, no I/O.
//!
//! NARROWINGS (per the P4-B differential method):
//! - `pyRound` (oracle `src/intel/util/pymath.js`) is inlined below as
//!   [`pymath::py_round`] with a citation; the crate-level `util.rs` is
//!   outside this task's file cluster. Only the non-negative digit counts the
//!   brain modules use (0..=27) are supported — Python's negative-digits
//!   rounding is unused here and returns the input unchanged.
//! - `poolArms`/`shrunkMean` take typed `f64` arm posteriors; the oracle
//!   coerces arbitrary `Record<string, number[]>` entries with `Number()`,
//!   which for JSON fixture inputs is the identity on numbers.
//! - `hierarchicalPrior`/`shrunkMean` results expose `to_json()` mirroring
//!   the oracle's exact key sets (the pooled result carries no `note` key).

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

/// Flat prior alpha — "no opinion" (oracle `FLAT_PRIOR_ALPHA`).
pub const FLAT_PRIOR_ALPHA: f64 = 1.0;
/// Flat prior beta — "no opinion" (oracle `FLAT_PRIOR_BETA`).
pub const FLAT_PRIOR_BETA: f64 = 1.0;
/// Floor on the shrinkage concentration `k0` (oracle `MIN_K0`).
pub const MIN_K0: f64 = 1.0;

/// Inlined from oracle `src/intel/util/pymath.js` — `pyRound`, Python
/// `round()` (banker's rounding, half-to-even) over the EXACT binary value of
/// the double. Implemented with exact `u128` integer arithmetic on the
/// decomposed double; no transcendentals, so results are bit-identical to the
/// oracle for the digit counts the brain modules use.
mod pymath {
    /// 5^0 .. 5^27 (mantissa * 5^digits always fits u128 for digits <= 27:
    /// 2^53 * 5^27 < 2^116).
    const POW5: [u128; 28] = [
        1,
        5,
        25,
        125,
        625,
        3_125,
        15_625,
        78_125,
        390_625,
        1_953_125,
        9_765_625,
        48_828_125,
        244_140_625,
        1_220_703_125,
        6_103_515_625,
        30_517_578_125,
        152_587_890_625,
        762_939_453_125,
        3_814_697_265_625,
        19_073_486_328_125,
        95_367_431_640_625,
        476_837_158_203_125,
        2_384_185_791_015_625,
        11_920_928_955_078_125,
        59_604_644_775_390_625,
        298_023_223_876_953_125,
        1_490_116_119_384_765_625,
        7_450_580_596_923_828_125,
    ];

    /// Decompose |x| into (mantissa, binary exponent): |x| = m * 2^e exactly.
    fn decompose(x: f64) -> (u64, i32) {
        let bits = x.to_bits();
        let biased = ((bits >> 52) & 0x7ff) as i32;
        let mant = bits & 0x000f_ffff_ffff_ffff;
        if biased == 0 {
            (mant, -1074) // subnormal (or zero; caller handles zero)
        } else {
            (mant | (1u64 << 52), biased - 1075)
        }
    }

    /// Python `round(x, digits)` — half-to-even over the exact binary value.
    /// Non-finite input passes through unchanged (as in the oracle). Digit
    /// counts outside 0..=27 are unused by the brain modules and return `x`.
    pub fn py_round(x: f64, digits: i32) -> f64 {
        if !x.is_finite() || x == 0.0 {
            return x; // preserves -0.0, as the oracle's `x === 0` branch does
        }
        if !(0..=27).contains(&digits) {
            return x; // documented narrowing: negative digits unused
        }
        let d = digits as u32;
        let neg = x < 0.0;
        let (mant, e) = decompose(x.abs());
        // exact value of x * 10^digits = mant * 5^digits * 2^(e + digits)
        let p: u128 = mant as u128 * POW5[d as usize];
        let s: i32 = e + digits;
        let q: u128 = if s >= 0 {
            // v is an integer: no fractional remainder to round.
            let bitlen = 128 - p.leading_zeros() as i64;
            if bitlen + s as i64 > 127 {
                // Exact fallback: v exceeds u128, but s >= 0 means x is
                // integral, so round(v) / 10^d == x as a double, exactly.
                return x;
            }
            p << s
        } else {
            let t = (-s) as u32;
            if t >= 128 {
                0 // v < 0.5 for every representable p here; no tie possible
            } else {
                let mut q = p >> t;
                let r = p & ((1u128 << t) - 1);
                let half = 1u128 << (t - 1);
                if r > half || (r == half && q % 2 == 1) {
                    q += 1;
                }
                q
            }
        };
        // result = nearest double to q / 10^digits (JS builds the same
        // decimal string and parses it; Rust's f64 parser is correctly
        // rounded, matching JS `Number(str)`).
        let mut ds = q.to_string();
        if digits == 0 {
            let sign = if neg && q != 0 { "-" } else { "" };
            return format!("{sign}{ds}").parse().unwrap_or(x);
        }
        let width = d as usize + 1;
        if ds.len() < width {
            let pad = "0".repeat(width - ds.len());
            ds = format!("{pad}{ds}");
        }
        let split = ds.len() - d as usize;
        let sign = if neg && q != 0 { "-" } else { "" };
        format!("{sign}{}.{}", &ds[..split], &ds[split..])
            .parse()
            .unwrap_or(x)
    }
}

pub use pymath::py_round;

/// JSON rendering of a JS number the way `JSON.stringify` emits it: integral
/// values as bare integers (`27`, and `-0` as `0`), everything else as the
/// shortest round-trip decimal, non-finite as `null`.
pub(crate) fn jnum(x: f64) -> Value {
    if !x.is_finite() {
        return Value::Null; // JSON.stringify(NaN / Infinity) === "null"
    }
    if x == 0.0 {
        return json!(0i64); // JSON.stringify(-0) === "0"
    }
    if x.fract() == 0.0 && x.abs() < 9.007_199_254_740_992e15 {
        return json!(x as i64);
    }
    match serde_json::Number::from_f64(x) {
        Some(n) => Value::Number(n),
        None => Value::Null,
    }
}

/// Error surfaces of the pooling module (oracle `RangeError`s).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PoolingError {
    /// Oracle: `throw new RangeError('no arms to pool')`.
    NoArms,
}

impl std::fmt::Display for PoolingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PoolingError::NoArms => write!(f, "no arms to pool"),
        }
    }
}

impl std::error::Error for PoolingError {}

/// The method-of-moments hyperparameters over a pool of Beta posteriors
/// (oracle `poolArms` return shape).
#[derive(Debug, Clone)]
pub struct PooledArms {
    pub mu0: f64,
    pub k0: f64,
    pub tau2: f64,
    pub n_arms: usize,
    pub evidence_arms: usize,
    pub no_evidence_arms: Vec<String>,
    pub total_evidence: f64,
    pub pooled: bool,
    /// Only present on the unpooled (no-evidence) result, as in the oracle.
    pub note: Option<String>,
}

impl PooledArms {
    pub fn to_json(&self) -> Value {
        let mut obj = Map::new();
        obj.insert("mu0".into(), jnum(self.mu0));
        obj.insert("k0".into(), jnum(self.k0));
        obj.insert("tau2".into(), jnum(self.tau2));
        obj.insert("n_arms".into(), json!(self.n_arms));
        obj.insert("evidence_arms".into(), json!(self.evidence_arms));
        obj.insert(
            "no_evidence_arms".into(),
            json!(self.no_evidence_arms.to_vec()),
        );
        obj.insert("total_evidence".into(), jnum(self.total_evidence));
        obj.insert("pooled".into(), json!(self.pooled));
        if let Some(note) = &self.note {
            obj.insert("note".into(), json!(note));
        }
        Value::Object(obj)
    }
}

/// Method-of-moments hyperparameters over `{name: [alpha, beta]}`.
///
/// Evidence-only accounting: `s_i = max(0, alpha_i - prior_alpha)` and
/// `n_i = max(0, (alpha_i - prior_alpha) + (beta_i - prior_beta))`; the
/// prior's own mass is never counted as evidence. Arms are processed in
/// sorted-name order (BTreeMap iteration == oracle `Object.keys().sort()`).
pub fn pool_arms(
    arms: &BTreeMap<String, [f64; 2]>,
    prior_alpha: f64,
    prior_beta: f64,
) -> Result<PooledArms, PoolingError> {
    if arms.is_empty() {
        return Err(PoolingError::NoArms);
    }
    let mut rates: Vec<f64> = Vec::new();
    let mut evidence: Vec<f64> = Vec::new();
    let mut s_total = 0.0f64;
    let mut n_total = 0.0f64;
    let mut no_evidence: Vec<String> = Vec::new();
    for (name, pair) in arms {
        let (a, b) = (pair[0], pair[1]);
        let s = (a - prior_alpha).max(0.0);
        let n = ((a - prior_alpha) + (b - prior_beta)).max(0.0);
        if n <= 0.0 {
            no_evidence.push(name.clone());
            continue;
        }
        s_total += s;
        n_total += n;
        rates.push(s / n);
        evidence.push(n);
    }
    if rates.is_empty() {
        return Ok(PooledArms {
            mu0: 0.5,
            k0: 0.0,
            tau2: 0.0,
            n_arms: arms.len(),
            evidence_arms: 0,
            no_evidence_arms: no_evidence,
            total_evidence: 0.0,
            pooled: false,
            note: Some(
                "no arm has observed evidence — nothing to borrow from; the flat prior stands"
                    .to_string(),
            ),
        });
    }
    let mu0 = s_total / n_total;
    let m = rates.len();
    let mean_rate = rates.iter().sum::<f64>() / m as f64;
    let var_observed = rates
        .iter()
        .map(|r| (r - mean_rate) * (r - mean_rate))
        .sum::<f64>()
        / m as f64;
    // expected sampling noise around the POOLED mean (the moments
    // decomposition: Var(p_i) = tau^2 + mu0(1-mu0)/n_i)
    let mean_sampling_var = evidence
        .iter()
        .map(|n| (mu0 * (1.0 - mu0)) / n)
        .sum::<f64>()
        / m as f64;
    let tau2 = (var_observed - mean_sampling_var).max(0.0);
    let k0_raw = if mu0 <= 0.0 || mu0 >= 1.0 || tau2 <= 1e-12 {
        // every arm agreed (or the pool sits at an extreme): FULL pooling —
        // the concentration caps at the pool's own evidence
        n_total
    } else {
        (mu0 * (1.0 - mu0)) / tau2
    };
    let k0 = MIN_K0.max(k0_raw.min(n_total));
    Ok(PooledArms {
        mu0: py_round(mu0, 6),
        k0: py_round(k0, 6),
        tau2: py_round(tau2, 6),
        n_arms: arms.len(),
        evidence_arms: m,
        no_evidence_arms: no_evidence,
        total_evidence: py_round(n_total, 6),
        pooled: true,
        note: None,
    })
}

/// The prior a NEW arm should start from, given the pool: centered exactly at
/// the pooled mean with concentration `k0`. The 0.5 Jeffreys-style floor
/// keeps an extreme pool from producing an improper prior. Unpooled (no
/// evidence anywhere) => FLAT prior (oracle `hierarchicalPrior`).
#[derive(Debug, Clone)]
pub struct HierarchicalPrior {
    pub alpha: f64,
    pub beta: f64,
    pub prior_mean: f64,
    pub pseudo_counts: f64,
    pub is_flat: bool,
    /// Only present on the flat (unpooled) result, as in the oracle.
    pub note: Option<String>,
}

impl HierarchicalPrior {
    pub fn to_json(&self) -> Value {
        let mut obj = Map::new();
        obj.insert("alpha".into(), jnum(self.alpha));
        obj.insert("beta".into(), jnum(self.beta));
        obj.insert("prior_mean".into(), jnum(self.prior_mean));
        obj.insert("pseudo_counts".into(), jnum(self.pseudo_counts));
        obj.insert("is_flat".into(), json!(self.is_flat));
        if let Some(note) = &self.note {
            obj.insert("note".into(), json!(note));
        }
        Value::Object(obj)
    }
}

pub fn hierarchical_prior(pooled: &PooledArms) -> HierarchicalPrior {
    if !pooled.pooled {
        return HierarchicalPrior {
            alpha: 1.0,
            beta: 1.0,
            prior_mean: 0.5,
            pseudo_counts: 0.0,
            is_flat: true,
            note: Some(pooled.note.clone().unwrap_or_default()),
        };
    }
    let alpha = (pooled.k0 * pooled.mu0).max(0.5);
    let beta = (pooled.k0 * (1.0 - pooled.mu0)).max(0.5);
    HierarchicalPrior {
        alpha: py_round(alpha, 6),
        beta: py_round(beta, 6),
        prior_mean: py_round(alpha / (alpha + beta), 6),
        pseudo_counts: py_round(pooled.k0, 6),
        is_flat: false,
        note: None,
    }
}

/// One EXISTING arm's shrunk acceptance estimate — the Efron-Morris estimator
/// `(s + k0*mu0) / (n + k0)` with the shrinkage weight reported visibly
/// (oracle `shrunkMean`).
#[derive(Debug, Clone)]
pub struct ShrunkMean {
    pub estimate: f64,
    pub own_evidence: f64,
    pub shrinkage_weight: f64,
}

impl ShrunkMean {
    pub fn to_json(&self) -> Value {
        json!({
            "estimate": jnum(self.estimate),
            "own_evidence": jnum(self.own_evidence),
            "shrinkage_weight": jnum(self.shrinkage_weight),
        })
    }
}

pub fn shrunk_mean(
    alpha: f64,
    beta: f64,
    pooled: &PooledArms,
    prior_alpha: f64,
    prior_beta: f64,
) -> ShrunkMean {
    let s = (alpha - prior_alpha).max(0.0);
    let n = ((alpha - prior_alpha) + (beta - prior_beta)).max(0.0);
    let estimate = (s + pooled.k0 * pooled.mu0) / (n + pooled.k0);
    ShrunkMean {
        estimate: py_round(estimate, 6),
        own_evidence: py_round(n, 6),
        shrinkage_weight: py_round(pooled.k0 / (n + pooled.k0), 6),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arms(pairs: &[(&str, f64, f64)]) -> BTreeMap<String, [f64; 2]> {
        pairs
            .iter()
            .map(|(n, a, b)| ((*n).to_string(), [*a, *b]))
            .collect()
    }

    #[test]
    fn py_round_is_bankers_half_to_even() {
        assert_eq!(py_round(0.5, 0), 0.0);
        assert_eq!(py_round(1.5, 0), 2.0);
        assert_eq!(py_round(2.5, 0), 2.0);
        assert_eq!(py_round(-2.5, 0), -2.0);
        assert_eq!(py_round(0.125, 2), 0.12); // exact binary 0.125 -> tie -> even
        assert_eq!(py_round(2.675, 2), 2.67); // 2.675 is really 2.6749999...
        assert_eq!(py_round(0.0, 6), 0.0);
        assert_eq!(py_round(-0.0, 6), -0.0);
        assert_eq!(py_round(f64::INFINITY, 3), f64::INFINITY);
        assert_eq!(py_round(0.9999995, 6), 1.0);
        assert_eq!(py_round(1e-7, 6), 0.0); // below the halfway point at 6 digits
    }

    #[test]
    fn no_evidence_pool_yields_flat_prior() {
        let pooled = pool_arms(&arms(&[("a", 1.0, 1.0), ("b", 1.0, 1.0)]), 1.0, 1.0)
            .unwrap_or_else(|e| panic!("pool: {e}"));
        assert!(!pooled.pooled);
        let prior = hierarchical_prior(&pooled);
        assert!(prior.is_flat);
        assert_eq!(prior.alpha, 1.0);
        assert_eq!(prior.beta, 1.0);
        assert_eq!(
            prior.note.as_deref(),
            Some("no arm has observed evidence — nothing to borrow from; the flat prior stands")
        );
    }

    #[test]
    fn all_agree_pool_fully_pools() {
        let pooled = pool_arms(
            &arms(&[("red", 4.0, 1.0), ("green", 7.0, 4.0), ("blue", 10.0, 7.0)]),
            1.0,
            1.0,
        )
        .unwrap_or_else(|e| panic!("pool: {e}"));
        assert!(pooled.pooled);
        assert_eq!(pooled.k0, 27.0); // capped at the pool's own evidence
        assert_eq!(pooled.tau2, 0.0);
        assert!(pooled.note.is_none());
    }

    #[test]
    fn empty_pool_is_an_error() {
        assert!(matches!(
            pool_arms(&BTreeMap::new(), 1.0, 1.0),
            Err(PoolingError::NoArms)
        ));
        assert_eq!(PoolingError::NoArms.to_string(), "no arms to pool");
    }
}
