//! Port of oracle `src/intel/adwin.js` — ADWIN (Bifet & Gavaldà, SDM 2007
//! bucket histogram), the dual-detector drift consensus, BOCPD (Adams &
//! MacKay 2007) and the plain halves comparison.
//!
//! Differential-port notes:
//! - `py_round` is inlined from oracle `src/intel/util/pymath.js::pyRound`
//!   (CPython's banker's rounding over the EXACT binary double, originally
//!   BigInt; reimplemented here over u128). Every oracle call site uses
//!   digits in {2,3,4,6} on O(1) magnitudes, well inside the exact envelope
//!   documented on the helper.
//! - `Math.log` (in `cut_once`) and `Math.exp`/`Math.sqrt` (in `bocpd`) are
//!   transcendental: those lines are validated at <=1-ulp tolerance by the
//!   committed fixtures (`fixtures/intel/adwin.jsonl`), per the crate-wide
//!   differential doctrine (`crate::util::ulp_close`). `Math.sqrt` itself is
//!   IEEE-754 correctly rounded, hence bit-exact.
//! - `Math.trunc` maps to `f64::trunc`; `1 << i` bucket sizes map to exact
//!   integer shifts; `Math.log2(size)` on power-of-two bucket sizes maps to
//!   `trailing_zeros` (identical values, computed without float detour).

use serde_json::{json, Value};

/// Python `round(x, digits)` — banker's rounding over the EXACT binary value
/// of the double. Inlined from oracle `src/intel/util/pymath.js::pyRound`
/// (BigInt algorithm reimplemented over u128).
///
/// Exactness envelope (superset of every oracle call site, which uses digits
/// in {2,3,4,6} on O(1) magnitudes):
/// - `digits` in `[0, 20]`: exact for any finite non-zero double (values
///   with `e >= 0` are integers and pass through; `-e > 127` rounds to +0.0
///   because `mant*10^digits*2 < 2^-e` holds there);
/// - `digits < 0`: exact inside a documented envelope (`e <= 70` or `-e <=
///   27`, `-digits <= 30`) and passes `x` through outside it;
/// - non-finite input and `±0.0` pass through unchanged (the oracle refuses
///   to invent a value, mirroring CPython raising).
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
            // integer value, or 10^digits divisible by 2^-e: no fractional
            // part below the rounding digit — exact pass-through
            return x;
        }
        if digits > 20 {
            return x; // outside the exact envelope; no oracle call site
        }
        if -e > 127 {
            return 0.0; // |x| < 2^-74: num*2 < den for digits <= 20 -> rounds to 0
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
        // negative digits: round to tens/hundreds/... (no oracle call site
        // in this module uses negative digits)
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

/// JS `Math.max(a, b)` semantics: NaN poisons, -0/+0 -> +0 when tied.
fn js_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a > b {
        a
    } else {
        b
    }
}

/// statistics.describe-style sample variance (n-1 denominator).
fn sample_variance(xs: &[f64]) -> f64 {
    let n = xs.len();
    if n < 2 {
        return 0.0;
    }
    let mean = xs.iter().sum::<f64>() / n as f64;
    let mut acc = 0.0;
    for x in xs {
        acc += (x - mean) * (x - mean);
    }
    acc / (n - 1) as f64
}

/// ADWIN (SDM 2007 variant): exponential histogram whose buckets each
/// summarize a run of 2^i consecutive elements as `[total, variance]`, at
/// most `max_buckets` per row. The OLDER subwindow is cut when the two
/// subwindow means differ by more than the Hoeffding bound. Alarms latch
/// until `reset`. Deterministic; no RNG.
#[derive(Debug, Clone)]
pub struct ADWIN {
    pub delta: f64,
    pub max_buckets: i64,
    pub min_width: i64,
    /// rows[i] holds buckets of size 2^i, oldest -> newest; each bucket is
    /// [total, variance]. width/mean are derived from the buckets.
    rows: Vec<Vec<[f64; 2]>>,
    n: i64,
    alarm_at: Option<i64>,
}

impl Default for ADWIN {
    fn default() -> Self {
        // the oracle's constructor defaults (0.002, 5, 32) — valid by
        // construction, so Default cannot fail
        Self {
            delta: 0.002,
            max_buckets: 5,
            min_width: 32,
            rows: vec![Vec::new()],
            n: 0,
            alarm_at: None,
        }
    }
}

impl ADWIN {
    /// `new ADWIN(delta = 0.002, maxBuckets = 5, minWidth = 32)`; errors
    /// carry the oracle's exact exception messages.
    pub fn new(delta: f64, max_buckets: i64, min_width: i64) -> Result<Self, String> {
        if !(0.0 < delta && delta < 1.0) {
            return Err("delta in (0, 1)".to_string());
        }
        if max_buckets < 2 {
            return Err("max_buckets >= 2".to_string());
        }
        if min_width < 2 {
            return Err("min_width >= 2".to_string());
        }
        Ok(Self {
            delta,
            max_buckets,
            min_width,
            rows: vec![Vec::new()],
            n: 0,
            alarm_at: None,
        })
    }

    /// The bucket list as ONE chronological sequence oldest->newest:
    /// (size, total, variance) triples. Higher rows are always older.
    pub fn flat(&self) -> Vec<(i64, f64, f64)> {
        let mut out = Vec::new();
        for i in (0..self.rows.len()).rev() {
            let size = (1u64 << i) as i64;
            for b in &self.rows[i] {
                out.push((size, b[0], b[1]));
            }
        }
        out
    }

    /// Merge the two OLDEST buckets of any overflowing row into the next row.
    fn compress(&mut self) {
        let mut i = 0usize;
        while i < self.rows.len() {
            while self.rows[i].len() > self.max_buckets as usize {
                let b1 = self.rows[i][0];
                let b2 = self.rows[i][1];
                self.rows[i].drain(0..2);
                if i + 1 >= self.rows.len() {
                    self.rows.push(Vec::new());
                }
                let n1 = (1u64 << i) as f64;
                let n2 = n1;
                let total = b1[0] + b2[0];
                let m1 = b1[0] / n1;
                let m2 = b2[0] / n2;
                let mean = total / (n1 + n2);
                let variance =
                    (n1 * (b1[1] + m1 * m1) + n2 * (b2[1] + m2 * m2)) / (n1 + n2) - mean * mean;
                // Math.max(variance, 0.0): negative -> 0, NaN passes through
                let variance = if variance < 0.0 { 0.0 } else { variance };
                self.rows[i + 1].push([total, variance]);
            }
            i += 1;
        }
    }

    /// Reinstall the window from a flat bucket list (after a cut).
    fn rebuild(&mut self, flat: &[(i64, f64, f64)]) {
        self.rows = vec![Vec::new()];
        for (size, total, variance) in flat {
            // Math.log2(size) for power-of-two sizes == trailing_zeros
            let row = size.trailing_zeros() as usize;
            while row + 1 > self.rows.len() {
                self.rows.push(Vec::new());
            }
            self.rows[row].push([*total, *variance]);
        }
    }

    /// The paper's cut check over every bucket-boundary split point.
    /// W0 = the oldest prefix; cut (drop W0) at the first split where
    /// |mu0 - mu1| >= epsilon_cut. Below min_width nothing is checked.
    fn cut_once(&mut self) -> bool {
        let flat = self.flat();
        let width: i64 = flat.iter().map(|b| b.0).sum();
        if width < self.min_width {
            return false;
        }
        let total: f64 = flat.iter().map(|b| b.1).sum();
        let mut n0: i64 = 0;
        let mut t0 = 0.0f64;
        for k in 0..flat.len().saturating_sub(1) {
            let (size, bucket_total, _) = flat[k];
            n0 += size;
            t0 += bucket_total;
            let n1 = width - n0;
            if n1 <= 0 {
                break;
            }
            let mu0 = t0 / n0 as f64;
            let mu1 = (total - t0) / n1 as f64;
            let m = 1.0 / (1.0 / n0 as f64 + 1.0 / n1 as f64);
            // Math.log below is transcendental: <=1-ulp tolerance applies
            let epsilon = ((4.0 / self.delta).ln() / (2.0 * m)).sqrt();
            if (mu0 - mu1).abs() >= epsilon {
                self.rebuild(&flat[k + 1..]);
                return true;
            }
        }
        false
    }

    fn width_total(&self) -> (i64, f64) {
        let flat = self.flat();
        let mut width = 0i64;
        let mut total = 0.0f64;
        for b in &flat {
            width += b.0;
            total += b.1;
        }
        (width, total)
    }

    /// Feed one accept (true) / reject (false); returns the oracle's report
    /// dict {drift, width, estimate, alarm_at}. `drift` is the EVENT (a cut
    /// this update); alarm_at latches.
    pub fn update(&mut self, accepted: bool) -> Value {
        self.rows[0].push([if accepted { 1.0 } else { 0.0 }, 0.0]);
        self.n += 1;
        self.compress();
        let mut drift = false;
        while self.cut_once() {
            drift = true;
            if self.alarm_at.is_none() {
                self.alarm_at = Some(self.n);
            }
        }
        let (width, total) = self.width_total();
        json!({
            "drift": drift,
            "width": width,
            "estimate": if width != 0 {
                json!(py_round(total / width as f64, 6))
            } else {
                json!(0.0)
            },
            "alarm_at": self.alarm_at,
        })
    }

    pub fn n(&self) -> i64 {
        self.n
    }

    pub fn alarm_at(&self) -> Option<i64> {
        self.alarm_at
    }

    pub fn width(&self) -> i64 {
        self.width_total().0
    }

    pub fn estimate(&self) -> f64 {
        let (width, total) = self.width_total();
        if width != 0 {
            py_round(total / width as f64, 6)
        } else {
            0.0
        }
    }

    pub fn status(&self) -> Value {
        let flat = self.flat();
        let mut width = 0i64;
        let mut total = 0.0f64;
        for b in &flat {
            width += b.0;
            total += b.1;
        }
        let estimate = if width != 0 {
            json!(py_round(total / width as f64, 6))
        } else {
            Value::Null
        };
        let message = match self.alarm_at {
            Some(at) => {
                format!("acceptance-rate mean shift detected — the window was cut at element {at}")
            }
            None if width >= self.min_width => "no drift detected".to_string(),
            None => {
                format!(
                    "warmup: width {width} < {}, no cut is checked yet",
                    self.min_width
                )
            }
        };
        json!({
            "n": self.n,
            "width": width,
            "estimate": estimate,
            "alarmed": self.alarm_at.is_some(),
            "alarm_at": self.alarm_at,
            "delta": self.delta,
            "max_buckets": self.max_buckets,
            "min_width": self.min_width,
            "buckets": flat.len(),
            "warmup": width < self.min_width,
            "message": message,
        })
    }

    pub fn reset(&mut self) -> &mut Self {
        self.rows = vec![Vec::new()];
        self.n = 0;
        self.alarm_at = None;
        self
    }

    /// The oracle's `toDict()` — the caller's persisted-state JSON path.
    pub fn to_json(&self) -> Value {
        let flat = self.flat();
        let mut width = 0i64;
        let mut total = 0.0f64;
        for b in &flat {
            width += b.0;
            total += b.1;
        }
        let estimate = if width != 0 {
            json!(py_round(total / width as f64, 6))
        } else {
            Value::Null
        };
        json!({
            "delta": self.delta,
            "max_buckets": self.max_buckets,
            "min_width": self.min_width,
            "n": self.n,
            "alarm_at": self.alarm_at,
            "width": width,
            "estimate": estimate,
            "buckets": flat.iter().map(|b| json!([b.0, b.1, b.2])).collect::<Vec<_>>(),
        })
    }

    /// The oracle's `fromDict(d)` — missing keys keep current values.
    pub fn from_json(&mut self, d: &Value) -> &mut Self {
        if let Some(v) = d.get("delta").and_then(Value::as_f64) {
            self.delta = v;
        }
        if let Some(v) = d.get("max_buckets").and_then(Value::as_i64) {
            self.max_buckets = v;
        }
        if let Some(v) = d.get("min_width").and_then(Value::as_i64) {
            self.min_width = v;
        }
        self.n = d.get("n").and_then(Value::as_i64).unwrap_or(0);
        self.alarm_at = match d.get("alarm_at") {
            Some(v) if !v.is_null() => v.as_i64().or_else(|| v.as_f64().map(|f| f.trunc() as i64)),
            _ => None,
        };
        self.rows = vec![Vec::new()];
        if let Some(arr) = d.get("buckets").and_then(Value::as_array) {
            for b in arr {
                let size = b.get(0).and_then(Value::as_f64).unwrap_or(1.0);
                let total = b.get(1).and_then(Value::as_f64).unwrap_or(0.0);
                let variance = b.get(2).and_then(Value::as_f64).unwrap_or(0.0);
                // oracle invariant: bucket sizes are powers of two, so
                // Math.log2(size) is an exact integer (trailing_zeros here)
                let row = (size.max(1.0) as u64).trailing_zeros() as usize;
                while row + 1 > self.rows.len() {
                    self.rows.push(Vec::new());
                }
                self.rows[row].push([total, variance]);
            }
        }
        self
    }
}

/// Pure consensus over the two PERSISTED state dicts.
pub fn consensus(ph_state: &Value, adwin_state: &Value) -> Value {
    let ph_alarm = ph_state.get("alarm_at").is_some_and(|v| !v.is_null());
    let ad_alarm = adwin_state.get("alarm_at").is_some_and(|v| !v.is_null());
    let flagged = ph_alarm && ad_alarm;
    let summary = if flagged {
        "FLAGGED — both detectors alarmed".to_string()
    } else {
        format!(
            "not flagged (page-hinkley {}, adwin {})",
            if ph_alarm { "alarmed" } else { "quiet" },
            if ad_alarm { "alarmed" } else { "quiet" }
        )
    };
    json!({
        "page_hinkley": {
            "alarmed": ph_alarm,
            "alarm_at": ph_state.get("alarm_at").cloned().unwrap_or(Value::Null),
            "n": ph_state.get("n").cloned().unwrap_or(Value::Null),
            "rejection_ema": ph_state.get("mean").cloned().unwrap_or(Value::Null),
        },
        "adwin": {
            "alarmed": ad_alarm,
            "alarm_at": adwin_state.get("alarm_at").cloned().unwrap_or(Value::Null),
            "width": adwin_state.get("width").cloned().unwrap_or(Value::Null),
            "estimate": adwin_state.get("estimate").cloned().unwrap_or(Value::Null),
        },
        "flag_drift": flagged,
        "rule": "drift is flagged to the user only when BOTH detectors have alarmed (dual agreement suppresses single-detector false alarms); each detector's own state is always reported individually",
        "summary": summary,
    })
}

/// One PageHinkleyDrift + one ADWIN over the SAME acceptance stream.
#[derive(Debug, Clone, Default)]
pub struct DriftConsensus {
    pub ph: crate::intel::conformal::PageHinkleyDrift,
    pub adwin: ADWIN,
}

impl DriftConsensus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_detectors(ph: crate::intel::conformal::PageHinkleyDrift, adwin: ADWIN) -> Self {
        Self { ph, adwin }
    }

    pub fn update(&mut self, accepted: bool) -> Value {
        let ph_row = self.ph.update(accepted);
        let ad_row = self.adwin.update(accepted);
        let mut out = consensus(&self.ph.to_json(), &self.adwin.to_json());
        if let Some(obj) = out.as_object_mut() {
            obj.insert(
                "last_update".to_string(),
                json!({"page_hinkley": ph_row, "adwin": ad_row}),
            );
        }
        out
    }

    pub fn status(&self) -> Value {
        consensus(&self.ph.to_json(), &self.adwin.to_json())
    }

    pub fn reset(&mut self) -> &mut Self {
        self.ph.reset();
        self.adwin.reset();
        self
    }

    pub fn to_json(&self) -> Value {
        json!({
            "page_hinkley": self.ph.to_json(),
            "adwin": self.adwin.to_json(),
        })
    }

    /// The oracle REPLACES both detectors with fresh instances reloaded from
    /// the dict (`new PageHinkleyDrift().fromDict(...)`), so any prior state
    /// is discarded here too.
    pub fn from_json(&mut self, d: &Value) -> &mut Self {
        let null = Value::Null;
        let mut ph = crate::intel::conformal::PageHinkleyDrift::default();
        ph.from_json(d.get("page_hinkley").unwrap_or(&null));
        let mut ad = ADWIN::default();
        ad.from_json(d.get("adwin").unwrap_or(&null));
        self.ph = ph;
        self.adwin = ad;
        self
    }
}

/// Bayesian online changepoint detection over a numeric series (Adams &
/// MacKay 2007): run-length posterior recursion, Gaussian observation model
/// (known variance, conjugate normal prior over the segment mean). Returns
/// the oracle's report dict; `Err` carries the oracle's exact messages.
///
/// Math.exp is transcendental — <=1-ulp tolerance applies (Math.sqrt is
/// IEEE-exact).
pub fn bocpd(
    series: &[f64],
    hazard: f64,
    variance: Option<f64>,
    prior_mean: Option<f64>,
) -> Result<Value, String> {
    let n = series.len();
    if n < 4 {
        return Err("need n >= 4 observations".to_string());
    }
    if hazard <= 0.0 {
        return Err("hazard (expected run length) must be > 0".to_string());
    }
    let s: Vec<f64> = series.to_vec();
    let sample_mean = s.iter().sum::<f64>() / n as f64;
    let var_est = match variance {
        Some(v) => v,
        None => {
            // robust noise-scale default: the variance of successive first
            // differences / 2 — insensitive to the level shifts being hunted
            let mut diffs = Vec::with_capacity(n - 1);
            for i in 0..n - 1 {
                diffs.push(s[i + 1] - s[i]);
            }
            js_max(sample_variance(&diffs) / 2.0, 1e-6)
        }
    };
    let mu0 = prior_mean.unwrap_or(sample_mean);
    let prior_tau = 0.5 / var_est;

    let h = 1.0 / hazard;
    let predictive = |x: f64, mu: f64, tau: f64| -> f64 {
        let var_p = var_est + 1.0 / js_max(tau, 1e-12);
        // Math.exp: transcendental — <=1-ulp tolerance (documented)
        (-0.5 * (x - mu) * (x - mu) / var_p).exp() / (2.0 * std::f64::consts::PI * var_p).sqrt()
    };

    let mut probs: Vec<f64> = vec![1.0]; // r=0 with probability 1 at t=0
    let mut means: Vec<f64> = vec![mu0];
    let mut taus: Vec<f64> = vec![prior_tau];

    let mut map_run_lengths: Vec<i64> = Vec::new();
    let mut cp_probs: Vec<f64> = Vec::new();
    let mut segment_means: Vec<f64> = Vec::new();
    for &x in s.iter().skip(1) {
        let likelihoods: Vec<f64> = (0..means.len())
            .map(|r| predictive(x, means[r], taus[r]))
            .collect();
        let growth: Vec<f64> = (0..probs.len())
            .map(|r| (1.0 - h) * probs[r] * likelihoods[r])
            .collect();
        let prior_predictive = predictive(x, mu0, prior_tau);
        let prob_sum: f64 = probs.iter().sum();
        let cp_mass = h * prob_sum * prior_predictive;
        let mut new_probs: Vec<f64> = Vec::with_capacity(probs.len() + 1);
        new_probs.push(cp_mass);
        new_probs.extend(growth);
        let evidence_raw: f64 = new_probs.iter().sum();
        // JS `|| 1e-300`: 0 (and NaN, falsy too) fall back to 1e-300
        let evidence = if evidence_raw == 0.0 || evidence_raw.is_nan() {
            1e-300
        } else {
            evidence_raw
        };
        for p in &mut new_probs {
            *p /= evidence;
        }
        probs = new_probs;
        let mut new_means: Vec<f64> = vec![mu0];
        let mut new_taus: Vec<f64> = vec![prior_tau];
        for r in 0..means.len() {
            let tau_old = taus[r];
            let mu_old = means[r];
            let tau_new = tau_old + 1.0 / var_est;
            let mu_new = (mu_old * tau_old + x / var_est) / tau_new;
            new_means.push(mu_new);
            new_taus.push(tau_new);
        }
        means = new_means;
        taus = new_taus;
        // MAP run length (earliest-index tie-break), p(r=0), current segment mean
        let mut best_r = 0usize;
        for r in 1..probs.len() {
            if probs[r] > probs[best_r] {
                best_r = r; // strict >: earliest wins ties
            }
        }
        map_run_lengths.push(best_r as i64);
        cp_probs.push(py_round(probs[0], 4));
        segment_means.push(py_round(means[best_r], 4));
    }

    let mut changepoints: Vec<i64> = Vec::new();
    for t in 1..n {
        if map_run_lengths[t - 1] <= 1 && cp_probs[t - 1] >= 0.3 {
            changepoints.push(t as i64);
        }
    }
    Ok(json!({
        "n": n,
        "hazard": hazard,
        "map_run_length": map_run_lengths,
        "changepoint_prob": cp_probs,
        "segment_mean": segment_means,
        "changepoints": changepoints,
        "algorithm": "BOCPD (Adams & MacKay 2007), constant hazard, conjugate normal segment model; the probabilistic complement to CUSUM",
    }))
}

/// Acceptance-rate drift across the example halves (old vs new). A report,
/// not an action: status "drift-detected" when |delta| >= 0.25. `labels` is
/// the 0/1 label stream in arrival order (strict `v === 1` equality).
pub fn halves_drift(labels: &[f64]) -> Value {
    if labels.len() < 10 {
        return json!({ "status": "insufficient-data", "examples": labels.len() });
    }
    let half = labels.len() / 2;
    let rate_of = |xs: &[f64]| -> f64 {
        let ones = xs.iter().filter(|&&v| v == 1.0).count();
        f64::from(ones as u32) / f64::from(xs.len() as u32)
    };
    let rate_old = rate_of(&labels[..half]);
    let rate_new = rate_of(&labels[half..]);
    let delta = rate_new - rate_old;
    let status = if delta.abs() >= 0.25 {
        "drift-detected"
    } else {
        "stable"
    };
    json!({
        "status": status,
        "old_rate": py_round(rate_old, 3),
        "new_rate": py_round(rate_new, 3),
        "delta": py_round(delta, 3),
        "examples": labels.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::ulp_close;

    #[test]
    fn empty_adwin_status_is_warmup() {
        let a = ADWIN::default();
        let st = a.status();
        assert_eq!(st["width"], 0);
        assert_eq!(st["estimate"], Value::Null);
        assert_eq!(st["warmup"], true);
        assert!(st["message"]
            .as_str()
            .unwrap_or("")
            .starts_with("warmup: width 0 < 32"));
    }

    #[test]
    fn single_update_no_cut() {
        let mut a = ADWIN::new(0.002, 5, 32).unwrap_or_else(|e| panic!("{e}"));
        let rep = a.update(true);
        assert_eq!(rep["drift"], false);
        assert_eq!(rep["width"], 1);
        assert_eq!(rep["alarm_at"], Value::Null);
    }

    #[test]
    fn cut_boundary_min_width_respected() {
        // below min_width nothing is cut, even with maximal disagreement
        let mut a = ADWIN::new(0.5, 5, 64).unwrap_or_else(|e| panic!("{e}"));
        for _ in 0..31 {
            assert_eq!(a.update(true)["drift"], false);
        }
        for _ in 0..31 {
            assert_eq!(a.update(false)["drift"], false, "width < min_width: no cut");
        }
        assert!(a.alarm_at().is_none());
        // at width >= min_width the same disagreement cuts immediately
        let mut b = ADWIN::new(0.5, 5, 32).unwrap_or_else(|e| panic!("{e}"));
        for _ in 0..31 {
            b.update(true);
        }
        let mut cut_seen = false;
        for _ in 0..31 {
            if b.update(false)["drift"] == true {
                cut_seen = true;
                break;
            }
        }
        assert!(cut_seen, "mean flip of 1.0 must cut once width allows");
    }

    #[test]
    fn halves_insufficient_and_boundary() {
        let short = halves_drift(&[1.0, 0.0, 1.0]);
        assert_eq!(short["status"], "insufficient-data");
        // old rate 0.5, new rate 0.25 -> |delta| == 0.25 -> drift-detected
        let labels: Vec<f64> = [1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 0, 0, 0, 0]
            .iter()
            .map(|&v| f64::from(v))
            .collect();
        let rep = halves_drift(&labels);
        assert_eq!(rep["status"], "drift-detected");
        assert!(ulp_close(rep["delta"].as_f64().unwrap_or(0.0), -0.25));
    }

    #[test]
    fn bocpd_too_short_is_err() {
        let err = bocpd(&[1.0, 2.0, 3.0], 100.0, None, None).unwrap_err();
        assert_eq!(err, "need n >= 4 observations");
    }

    #[test]
    fn bocpd_level_shift_finds_changepoint() {
        let series = [
            0.0, 0.1, -0.1, 0.05, 0.1, 0.0, 3.0, 3.1, 2.9, 3.05, 3.0, 2.95,
        ];
        let rep = bocpd(&series, 100.0, None, None).unwrap_or_else(|e| panic!("{e}"));
        let cps = rep["changepoints"]
            .as_array()
            .unwrap_or(&Vec::new())
            .clone();
        assert!(!cps.is_empty(), "expected a changepoint at the level shift");
    }

    #[test]
    fn consensus_empty_dicts_not_flagged() {
        let c = consensus(&json!({}), &json!({}));
        assert_eq!(c["flag_drift"], false);
        assert_eq!(c["page_hinkley"]["alarmed"], false);
        assert_eq!(c["adwin"]["width"], Value::Null);
    }

    #[test]
    fn py_round_bankers() {
        // half-to-even over the exact double
        assert!(ulp_close(py_round(0.5, 0), 0.0));
        assert!(ulp_close(py_round(1.5, 0), 2.0));
        assert!(ulp_close(py_round(2.5, 0), 2.0));
        assert!(ulp_close(py_round(0.125, 2), 0.12));
        assert!(ulp_close(py_round(0.135, 2), 0.14));
        assert!(ulp_close(py_round(-0.125, 2), -0.12));
        assert!(ulp_close(py_round(0.0, 4), 0.0));
    }
}
