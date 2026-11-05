//! Port of oracle `src/intel/abers.js` — Venn-Abers honest probability
//! calibration (weighted PAVA isotonic regression, Venn-Abers predictive
//! sets, weighted conformal quantile, ECE reporting).
//!
//! Differential-port notes:
//! - `py_round` is inlined from oracle `src/intel/util/pymath.js::pyRound`
//!   (CPython banker's rounding over the EXACT binary double; reimplemented
//!   over u128). Oracle call sites use digit counts in {4}, inside the
//!   helper's exact envelope.
//! - Every float op in this module is elementary (+,-,*,/,sqrt,floor,min,
//!   max, comparisons) — the whole module is validated bit-exact by the
//!   committed fixtures (`fixtures/intel/abers.jsonl`).
//! - JS `Array.prototype.sort` with a numeric comparator is stable; Rust's
//!   `sort_by` is stable too, so PAVA tie order (insertion order) matches.
//! - JS truthiness for outcomes (`outcomes[i] ? 1 : 0`): 0, -0 and NaN
//!   count as 0; every other number counts as 1.
//! - `j % folds` with `folds == 0` is NaN in JS, and `NaN !== NaN` is true,
//!   so every row is kept; the port mirrors that explicitly.

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

/// Weighted PAVA core (pool-adjacent-violators) over (score, outcome) pairs
/// with per-pair weights; `weights` (when given) must have `pairs.len()`
/// entries — the public wrapper enforces that. Returns the fitted step
/// function as [x, y] pairs with x ascending and y the pooled means.
fn pava_fit(pairs: &[[f64; 2]], weights: Option<&[f64]>) -> Vec<[f64; 2]> {
    let mut data: Vec<(f64, f64, f64)> = pairs
        .iter()
        .enumerate()
        .map(|(i, p)| (p[0], p[1], weights.map_or(1.0, |w| w[i])))
        .collect();
    // stable: JS sort keeps insertion order on equal scores, so does Rust
    data.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    // blocks: [sumW, sumWy, mean, xs]
    let mut blocks: Vec<(f64, f64, f64, Vec<f64>)> = Vec::new();
    for (x, y, w) in data {
        if w <= 0.0 {
            continue;
        }
        blocks.push((w, w * y, y / w, vec![x]));
        while blocks.len() >= 2 {
            let tail = blocks[blocks.len() - 1].2;
            let prev = blocks[blocks.len() - 2].2;
            if prev > tail + 1e-15 {
                let b2 = blocks.pop().unwrap_or_default();
                let b1 = blocks.pop().unwrap_or_default();
                blocks.push((
                    b1.0 + b2.0,
                    b1.1 + b2.1,
                    (b1.1 + b2.1) / (b1.0 + b2.0),
                    b1.3.iter().chain(&b2.3).copied().collect(),
                ));
            } else {
                break;
            }
        }
    }
    let mut steps: Vec<[f64; 2]> = Vec::new();
    for (_, _, mean, xs) in &blocks {
        for x in xs {
            steps.push([*x, *mean]); // one (x, y) per calibration point
        }
    }
    steps
}

/// Weighted PAVA over (score, outcome) pairs sorted by score. Returns the
/// fitted step function as [x, y] pairs (monotone non-decreasing map).
/// `Err` on weights length mismatch; w <= 0 rows are dropped.
pub fn isotonic_fit(pairs: &[[f64; 2]], weights: Option<&[f64]>) -> Result<Vec<[f64; 2]>, String> {
    if let Some(w) = weights {
        if w.len() != pairs.len() {
            return Err("weights must match pairs".to_string());
        }
    }
    Ok(pava_fit(pairs, weights))
}

/// Evaluate the PAVA step map at x (right-continuous; below the first
/// calibration point it returns the first pooled mean). Empty -> 0.5.
pub fn isotonic_predict(fit_steps: &[[f64; 2]], x: f64) -> f64 {
    if fit_steps.is_empty() {
        return 0.5;
    }
    let mut lo: isize = 0;
    let mut hi: isize = fit_steps.len() as isize - 1;
    let mut best = fit_steps[0][1];
    while lo <= hi {
        let mid = (lo + hi) / 2; // == Math.floor((lo + hi) / 2) here
        if fit_steps[mid as usize][0] <= x {
            best = fit_steps[mid as usize][1];
            lo = mid + 1;
        } else {
            hi = mid - 1;
        }
    }
    best
}

/// (p_lo, p_hi, midpoint) for one test score — the exact distribution-free
/// calibration interval (Vovk & Petej 2014, IVAP form).
pub fn venn_abers(calib: &[[f64; 2]], test_p: f64) -> [f64; 3] {
    let mut with_one = calib.to_vec();
    with_one.push([test_p, 1.0]);
    let hi = isotonic_predict(&pava_fit(&with_one, None), test_p);
    let mut with_zero = calib.to_vec();
    with_zero.push([test_p, 0.0]);
    let lo_raw = isotonic_predict(&pava_fit(&with_zero, None), test_p);
    let lo = if lo_raw < hi { lo_raw } else { hi }; // numerical ordering guard
    [lo, hi, (lo + hi) / 2.0]
}

/// The split-conformal quantile under importance WEIGHTS. Unweighted
/// (`weights = None`): exactly the ConformalCalibrator threshold rule —
/// the ceil((n+1)(1-alpha))/n order statistic. Weighted: the (n+1)(1-alpha)/n
/// mass quantile of the reweighted distribution. `Ok(None)` when n < 5 or
/// total weight <= 0; `Err` on bad alpha or a weights length mismatch.
pub fn weighted_conformal_quantile(
    scores: &[f64],
    weights: Option<&[f64]>,
    alpha: f64,
) -> Result<Option<f64>, String> {
    let n = scores.len();
    if n < 5 {
        return Ok(None);
    }
    if !(0.0 < alpha && alpha < 1.0) {
        return Err("alpha in (0, 1)".to_string());
    }
    let weights = match weights {
        None => {
            let mut s = scores.to_vec();
            s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let rank = ((n as f64 + 1.0) * (1.0 - alpha)).ceil();
            let rank = (rank as usize).min(n).max(1);
            return Ok(s.get(rank - 1).copied());
        }
        Some(w) => w,
    };
    if weights.len() != n {
        return Err("weights must match scores".to_string());
    }
    let mut pairs: Vec<(f64, f64)> = scores
        .iter()
        .zip(weights.iter())
        .map(|(s, w)| (*s, *w))
        .collect();
    pairs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let total: f64 = pairs.iter().map(|p| p.1).sum();
    if total <= 0.0 {
        return Ok(None);
    }
    // (n+1)/n finite-sample correction matches the unweighted rule in the
    // equal-weights limit
    let target = 1.0f64.min(((n as f64 + 1.0) / n as f64) * (1.0 - alpha));
    let mut cum = 0.0f64;
    for (s, w) in &pairs {
        cum += w / total;
        if cum >= target - 1e-12 {
            return Ok(Some(*s));
        }
    }
    Ok(pairs.last().map(|p| p.0))
}

/// Expected Calibration Error over equal-width bins (edges [0,1]):
/// sum over bins of |acc - conf| * bin_weight. Empty scores -> 0.0.
pub fn ece(scores: &[f64], outcomes: &[f64], bins: usize) -> f64 {
    if scores.is_empty() || bins == 0 {
        return 0.0;
    }
    let mut bin_count = vec![0usize; bins];
    let mut bin_correct = vec![0.0f64; bins];
    let mut bin_conf = vec![0.0f64; bins];
    for (i, &si) in scores.iter().enumerate() {
        let p = 1.0f64.min(0.0f64.max(si));
        let mut b = (p * bins as f64).floor() as usize;
        if b >= bins {
            b = bins - 1; // p == 1.0 lands in the top bin
        }
        bin_count[b] += 1;
        // JS truthiness: outcomes[i] ? 1 : 0 (0, -0, NaN, missing -> 0)
        let o = match outcomes.get(i) {
            Some(v) if !v.is_nan() && *v != 0.0 => 1.0,
            _ => 0.0,
        };
        bin_correct[b] += o;
        bin_conf[b] += p;
    }
    let mut total = 0.0f64;
    for b in 0..bins {
        if bin_count[b] == 0 {
            continue;
        }
        let acc = bin_correct[b] / bin_count[b] as f64;
        let conf = bin_conf[b] / bin_count[b] as f64;
        total += (bin_count[b] as f64 / scores.len() as f64) * (acc - conf).abs();
    }
    total
}

/// ECE before/after calibration over deterministic held-out folds.
/// Fold i (every folds-th row) is scored by calibrators fitted on the OTHER
/// rows, so the after numbers are out-of-sample. Raw ECE is fold-free.
pub fn ece_report(rows: &[[f64; 2]], folds: usize, bins: usize) -> Value {
    let raw = ece(
        &rows.iter().map(|r| r[0]).collect::<Vec<_>>(),
        &rows.iter().map(|r| r[1]).collect::<Vec<_>>(),
        bins,
    );
    let mut iso_vals: Vec<[f64; 2]> = Vec::new();
    let mut va_vals: Vec<[f64; 2]> = Vec::new();
    let mut wide = 0i64;
    for i in 0..rows.len() {
        // JS folds == 0 makes j % folds NaN and NaN !== NaN true: keep all
        let train: Vec<[f64; 2]> = rows
            .iter()
            .enumerate()
            .filter(|(j, _)| folds == 0 || j % folds != i % folds)
            .map(|(_, r)| *r)
            .collect();
        if train.is_empty() {
            continue;
        }
        let steps = pava_fit(&train, None);
        let p = rows[i][0];
        let o = rows[i][1];
        iso_vals.push([isotonic_predict(&steps, p), o]);
        let [lo, hi, mid] = venn_abers(&train, p);
        va_vals.push([mid, o]);
        if hi - lo > 0.34 {
            wide += 1;
        }
    }
    let iso_ece = ece(
        &iso_vals.iter().map(|r| r[0]).collect::<Vec<_>>(),
        &iso_vals.iter().map(|r| r[1]).collect::<Vec<_>>(),
        bins,
    );
    let va_ece = ece(
        &va_vals.iter().map(|r| r[0]).collect::<Vec<_>>(),
        &va_vals.iter().map(|r| r[1]).collect::<Vec<_>>(),
        bins,
    );
    json!({
        "n": rows.len(),
        "ece_raw": py_round(raw, 4),
        "ece_isotonic": py_round(iso_ece, 4),
        "ece_venn_abers": py_round(va_ece, 4),
        "wide_va_intervals": wide,
        "folds": folds,
        "note": "out-of-sample: fold i scored by calibrators fitted on the other folds (deterministic, no RNG)",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::ulp_close;

    #[test]
    fn empty_inputs() {
        assert!(isotonic_fit(&[], None).unwrap_or_default().is_empty());
        assert!(ulp_close(isotonic_predict(&[], 0.3), 0.5));
        assert!(ulp_close(ece(&[], &[], 5), 0.0));
        assert_eq!(
            weighted_conformal_quantile(&[0.1, 0.2, 0.3, 0.4], None, 0.1),
            Ok(None)
        );
    }

    #[test]
    fn single_point_is_identity() {
        let steps = isotonic_fit(&[[0.7, 0.3]], None).unwrap_or_default();
        assert_eq!(steps.len(), 1);
        assert!(ulp_close(steps[0][1], 0.3));
        assert!(ulp_close(venn_abers(&[[0.5, 1.0]], 0.5)[2], 0.75));
    }

    #[test]
    fn violators_pool_to_monotone() {
        let steps = isotonic_fit(&[[0.1, 0.8], [0.2, 0.2], [0.3, 0.9], [0.4, 0.1]], None)
            .unwrap_or_default();
        // every fitted y must be non-decreasing in x
        for w in steps.windows(2) {
            assert!(w[0][0] <= w[1][0]);
            assert!(w[0][1] <= w[1][1] + 1e-12);
        }
        // this fixture case pools everything to the global mean
        assert!(ulp_close(steps[0][1], 0.5));
    }

    #[test]
    fn zero_and_negative_weights_dropped() {
        let steps = isotonic_fit(
            &[[0.1, 0.9], [0.2, 0.1], [0.3, 0.8]],
            Some(&[2.0, 0.0, 1.0]),
        )
        .unwrap_or_default();
        assert_eq!(steps.len(), 2); // the w=0 row is gone
        let err = isotonic_fit(&[[0.1, 0.9]], Some(&[1.0, 2.0])).unwrap_err();
        assert_eq!(err, "weights must match pairs");
    }

    #[test]
    fn ece_boundaries() {
        // p == 1.0 must land in the top bin (floor clamp), p == 0 in bin 0
        let v = ece(&[1.0, 0.0], &[1.0, 0.0], 5);
        assert!(ulp_close(v, 0.0));
        let perfect = ece(
            &[0.05, 0.15, 0.25, 0.35, 0.45, 0.55, 0.65, 0.75, 0.85, 0.95],
            &[0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0],
            10,
        );
        assert!((0.0..=1.0).contains(&perfect));
    }

    #[test]
    fn wcq_bad_alpha_is_err() {
        let scores = [0.1, 0.2, 0.3, 0.4, 0.5];
        match weighted_conformal_quantile(&scores, None, 0.0) {
            Ok(_) => panic!("bad alpha must be an error"),
            Err(e) => assert_eq!(e, "alpha in (0, 1)"),
        }
    }

    #[test]
    fn wcq_unweighted_matches_order_statistic() {
        // rank = ceil(6 * 0.9) = 6 -> min(5) -> 5th sorted of 5 -> max
        let scores = [0.12, 0.4, 0.55, 0.61, 0.8];
        let q = weighted_conformal_quantile(&scores, None, 0.1)
            .unwrap_or_default()
            .unwrap_or(0.0);
        assert!(ulp_close(q, 0.8));
    }

    #[test]
    fn venn_abers_brackets_isotonic() {
        let calib = [
            [0.1, 0.0],
            [0.2, 0.0],
            [0.4, 1.0],
            [0.6, 1.0],
            [0.8, 1.0],
            [0.9, 0.0],
        ];
        let [lo, hi, mid] = venn_abers(&calib, 0.5);
        assert!(lo <= mid && mid <= hi);
        let iso = isotonic_predict(&isotonic_fit(&calib, None).unwrap_or_default(), 0.5);
        assert!(iso >= lo && iso <= hi);
    }
}
