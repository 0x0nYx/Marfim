//! Port of oracle `src/intel/brain/sketch.js` — Space-Saving heavy hitters
//! (Metwally et al. 2005) and t-digest quantiles (Dunning 2019, k1 scale
//! function `k(q) = (delta/2)*asin(2q-1)`) for disk and log sizing.
//!
//! Streaming questions, deterministic for a given input order, capped —
//! values beyond the caps ABSTAIN rather than lie. The carried Space-Saving
//! count is an OVERESTIMATE reported, never hidden; sizing reports are
//! honest about the path taken (EXACT sorted quantiles at n <= 1000, TDIGEST
//! above, ABSTAIN past 200000).
//!
//! NARROWINGS (per the P4-B differential method):
//! - `pyRound` is the shared inlined copy at
//!   [`crate::brain::pooling::py_round`].
//! - `Math.asin` maps to `f64::asin`; verified BIT-identical to the Node
//!   engine on the fixture probe domain (V8 and the platform libm agree on
//!   these inputs). Rebuild merge decisions depend on the k1 limit at
//!   <=1-ulp tolerance; the differential fixtures replay full centroid
//!   ladders, so any structural drift would fail.
//! - `toFixed`/`toLocaleString('en-US')` (renderSizing) are implemented
//!   exactly (ES spec: round-half-up over the EXACT binary value; grouping)
//!   in [`js_to_fixed`] / [`js_locale_int`].
//! - `String(number)` for quantile-point keys maps to Rust `Display`
//!   (shortest round-trip); the exponent forms beyond |x| >= 1e21 differ and
//!   are not exercised.

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap};

use serde_json::{json, Map, Value};

use crate::brain::pooling::{jnum, py_round};

/// Space-Saving item cap (oracle `SS_MAX_ITEMS`).
pub const SS_MAX_ITEMS: usize = 200_000;
/// t-digest value cap (oracle `DIGEST_MAX_VALUES`).
pub const DIGEST_MAX_VALUES: usize = 200_000;

/// Error surfaces of the sketch module (oracle `RangeError`s).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SketchError {
    /// Oracle: `throw new RangeError('k must be >= 1')`.
    KInvalid,
}

impl std::fmt::Display for SketchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SketchError::KInvalid => write!(f, "k must be >= 1"),
        }
    }
}

impl std::error::Error for SketchError {}

// ---------------------------------------------------------------------------
// JS numeric-rendering helpers (exact, cited in the module doc narrowing).
// ---------------------------------------------------------------------------

/// 5^0 .. 5^27 — shared exact-arithmetic table (see `js_to_fixed`).
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

fn decompose(x: f64) -> (u64, i32) {
    let bits = x.to_bits();
    let biased = ((bits >> 52) & 0x7ff) as i32;
    let mant = bits & 0x000f_ffff_ffff_ffff;
    if biased == 0 {
        (mant, -1074)
    } else {
        (mant | (1u64 << 52), biased - 1075)
    }
}

/// ECMAScript `Number.prototype.toFixed(f)`: n minimises |n/10^f - x| with
/// ties picking the LARGER n, computed over the exact binary value; sign
/// comes from the original x (so -0.0004 -> "-0.00"). |x| >= 1e21 and
/// non-finite inputs fall back to `js_num_str`/`"NaN"` per spec; those are
/// not exercised by the brain fixtures.
fn js_to_fixed(x: f64, frac: u32) -> String {
    if x.is_nan() {
        return "NaN".to_string();
    }
    if !x.is_finite() {
        return if x > 0.0 {
            "Infinity".to_string()
        } else {
            "-Infinity".to_string()
        };
    }
    let neg = x < 0.0;
    let ax = x.abs();
    if ax >= 1e21 {
        let body = js_num_str(ax);
        return if neg { format!("-{body}") } else { body };
    }
    let n: i128 = if ax == 0.0 {
        0
    } else {
        let (mant, e) = decompose(ax);
        let d = frac as i32;
        let p: u128 = mant as u128 * POW5[frac as usize];
        let s: i32 = e + d;
        if s >= 0 {
            // v = x * 10^f is an integer: n = v (floor(v + 0.5) == v)
            let bitlen = 128 - p.leading_zeros() as i64;
            if bitlen + s as i64 > 126 {
                return if neg {
                    format!("-{}", js_num_str(ax))
                } else {
                    js_num_str(ax)
                };
            }
            (p << s) as i128
        } else {
            let t = (-s) as u32;
            if t >= 118 {
                // v <= 2^116 / 2^118 = 0.25 < 0.5: n = 0
                0
            } else {
                // JS toFixed rounds the MAGNITUDE half-UP (away from zero)
                // and re-applies the sign afterwards — a negative tie like
                // -0.125 → "-0.13", never "-0.12". All arithmetic on |x|.
                let two_p = (p * 2) as i128;
                let half_pow = 1i128 << (t + 1);
                let numer = two_p + (1i128 << t);
                numer / half_pow
            }
        }
    };
    let sign = if neg { "-" } else { "" };
    if frac == 0 {
        return format!("{sign}{n}");
    }
    let mut ds = n.unsigned_abs().to_string();
    let width = frac as usize + 1;
    if ds.len() < width {
        let pad = "0".repeat(width - ds.len());
        ds = format!("{pad}{ds}");
    }
    let split = ds.len() - frac as usize;
    format!("{sign}{}.{}", &ds[..split], &ds[split..])
}

/// `toLocaleString('en-US')` for integral magnitudes (Math.round output and
/// Space-Saving counts): comma groups of three. Fractional/exponent-range
/// values are not exercised by the brain fixtures.
fn js_locale_int(x: f64) -> String {
    if x.is_nan() {
        return "NaN".to_string();
    }
    if !x.is_finite() {
        return if x > 0.0 {
            "Infinity".to_string()
        } else {
            "-Infinity".to_string()
        };
    }
    if x == 0.0 {
        return "0".to_string(); // String(-0) === "0"
    }
    let neg = x < 0.0;
    let digits = format!("{}", x.abs() as u64);
    let bytes = digits.as_bytes();
    let len = bytes.len();
    let mut out = String::new();
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*b as char);
    }
    if neg {
        format!("-{out}")
    } else {
        out
    }
}

/// JS `String(number)` / template-literal rendering (shortest round-trip;
/// `-0` renders as `"0"`). Exponent forms beyond |x| >= 1e21 differ from
/// Rust's `Display` and are not exercised (documented narrowing).
fn js_num_str(x: f64) -> String {
    if x.is_nan() {
        return "NaN".to_string();
    }
    if !x.is_finite() {
        return if x > 0.0 {
            "Infinity".to_string()
        } else {
            "-Infinity".to_string()
        };
    }
    if x == 0.0 {
        return "0".to_string();
    }
    format!("{x}")
}

// ---------------------------------------------------------------------------
// SpaceSaving
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct SsEntry {
    key: String,
    count: f64,
    overestimate: f64,
}

/// Top-k counters with Space-Saving eviction (oracle `SpaceSaving`): a new
/// key arriving at a full table demotes the weakest counter and re-labels it;
/// the carried count is an OVERESTIMATE reported, never hidden. Entries keep
/// JS-Map insertion order; eviction is min by (count, key).
#[derive(Debug, Clone)]
pub struct SpaceSaving {
    pub k: usize,
    entries: Vec<SsEntry>,
    positions: HashMap<String, usize>, // lookup only; never iterated
    pub n_items: u64,
    pub n_total: f64,
}

impl SpaceSaving {
    pub fn new(k: usize) -> Result<Self, SketchError> {
        if k < 1 {
            return Err(SketchError::KInvalid);
        }
        Ok(Self {
            k,
            entries: Vec::new(),
            positions: HashMap::new(),
            n_items: 0,
            n_total: 0.0,
        })
    }

    /// Observe one occurrence of `key` carrying `weight` (non-positive
    /// weights are ignored, as in the oracle).
    pub fn add(&mut self, key: &str, weight: f64) {
        if weight <= 0.0 {
            return;
        }
        self.n_items += 1;
        self.n_total += weight;
        if let Some(&i) = self.positions.get(key) {
            self.entries[i].count += weight;
            return;
        }
        if self.entries.len() < self.k {
            self.positions.insert(key.to_string(), self.entries.len());
            self.entries.push(SsEntry {
                key: key.to_string(),
                count: weight,
                overestimate: 0.0,
            });
            return;
        }
        // evict the minimum by (count, key) and carry its count as error;
        // entries is non-empty here because k >= 1 (proven at construction)
        let mut victim = 0usize;
        for i in 1..self.entries.len() {
            let e = &self.entries[i];
            let v = &self.entries[victim];
            if e.count < v.count || (e.count == v.count && e.key < v.key) {
                victim = i;
            }
        }
        let carried = self.entries[victim].count;
        let removed = self.entries[victim].key.clone();
        self.entries.remove(victim);
        self.positions.remove(&removed);
        for (i, e) in self.entries.iter().enumerate() {
            self.positions.insert(e.key.clone(), i);
        }
        self.positions.insert(key.to_string(), self.entries.len());
        self.entries.push(SsEntry {
            key: key.to_string(),
            count: carried + weight,
            overestimate: carried,
        });
    }

    /// Ranked hitters: `{key, count, max_error, share}`; share rounded 4.
    pub fn heavy_hitters(&self) -> Vec<HeavyHitter> {
        let mut sorted: Vec<&SsEntry> = self.entries.iter().collect();
        sorted.sort_by(|a, b| {
            b.count
                .partial_cmp(&a.count)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.key.cmp(&b.key))
        });
        sorted
            .into_iter()
            .map(|e| HeavyHitter {
                key: e.key.clone(),
                count: e.count,
                max_error: e.overestimate,
                share: if self.n_total != 0.0 {
                    py_round(e.count / self.n_total, 4)
                } else {
                    0.0
                },
            })
            .collect()
    }

    fn count_of(&self, key: &str) -> Option<f64> {
        self.positions.get(key).map(|&i| self.entries[i].count)
    }

    fn error_of(&self, key: &str) -> Option<f64> {
        self.positions
            .get(key)
            .map(|&i| self.entries[i].overestimate)
    }

    /// Combine two sketches over disjoint streams: sums counts and
    /// conservatively carries both errors (oracle `merge`).
    pub fn merge(&self, other: &SpaceSaving) -> Result<SpaceSaving, SketchError> {
        let mut merged = SpaceSaving::new(self.k)?;
        merged.n_items = self.n_items + other.n_items;
        merged.n_total = self.n_total + other.n_total;
        let keys: BTreeSet<&String> = self
            .entries
            .iter()
            .map(|e| &e.key)
            .chain(other.entries.iter().map(|e| &e.key))
            .collect();
        let mut scored: Vec<(String, f64, f64)> = keys
            .into_iter()
            .map(|key| {
                let count = self.count_of(key).unwrap_or(0.0) + other.count_of(key).unwrap_or(0.0);
                let err = self.error_of(key).unwrap_or(0.0) + other.error_of(key).unwrap_or(0.0);
                (key.clone(), count, err)
            })
            .collect();
        // count DESC, key ASC — a total order, so the outcome is independent
        // of the union's iteration order
        scored.sort_by(|x, y| {
            y.1.partial_cmp(&x.1)
                .unwrap_or(Ordering::Equal)
                .then_with(|| x.0.cmp(&y.0))
        });
        for (key, count, err) in scored.into_iter().take(merged.k) {
            merged.positions.insert(key.clone(), merged.entries.len());
            merged.entries.push(SsEntry {
                key,
                count,
                overestimate: err,
            });
        }
        Ok(merged)
    }

    /// Full sketch state (fixture capture shape).
    pub fn to_json(&self) -> Value {
        let mut counts = Map::new();
        let mut overestimate = Map::new();
        for e in &self.entries {
            counts.insert(e.key.clone(), jnum(e.count));
            overestimate.insert(e.key.clone(), jnum(e.overestimate));
        }
        json!({
            "k": self.k,
            "counts": counts,
            "overestimate": overestimate,
            "nItems": self.n_items,
            "nTotal": jnum(self.n_total),
        })
    }
}

/// One heavy-hitters row (oracle `heavyHitters` entries).
#[derive(Debug, Clone)]
pub struct HeavyHitter {
    pub key: String,
    pub count: f64,
    pub max_error: f64,
    pub share: f64,
}

impl HeavyHitter {
    pub fn to_json(&self) -> Value {
        json!({
            "key": self.key,
            "count": jnum(self.count),
            "max_error": jnum(self.max_error),
            "share": jnum(self.share),
        })
    }
}

// ---------------------------------------------------------------------------
// TDigest
// ---------------------------------------------------------------------------

/// A t-digest with the k1 scale function `k(q) = (delta/2)*asin(2q-1)`
/// (oracle `TDigest`): cluster size bounds grow toward the middle so the
/// tails stay sharp. Values buffer, then rebuild in sorted order with the k1
/// size bound.
#[derive(Debug, Clone)]
pub struct TDigest {
    pub delta: f64,
    pub buffer_cap: usize,
    /// `[mean, weight]` pairs, sorted by mean.
    pub centroids: Vec<(f64, f64)>,
    pub buffer: Vec<(f64, f64)>,
    pub n: u64,
    pub total: f64,
    pub min_value: Option<f64>,
    pub max_value: Option<f64>,
}

impl TDigest {
    pub fn new(delta: f64, buffer_cap: usize) -> Self {
        Self {
            delta,
            buffer_cap,
            centroids: Vec::new(),
            buffer: Vec::new(),
            n: 0,
            total: 0.0,
            min_value: None,
            max_value: None,
        }
    }

    /// k1 scale function.
    fn k1(&self, q: f64) -> f64 {
        let qc = q.clamp(0.0, 1.0);
        (self.delta / 2.0) * (2.0 * qc - 1.0).asin()
    }

    /// Observe one value with `weight` (non-positive weights are ignored).
    pub fn add(&mut self, value: f64, weight: f64) {
        if weight <= 0.0 {
            return;
        }
        self.n += 1;
        self.total += weight;
        if self.min_value.is_none_or(|cur| value < cur) {
            self.min_value = Some(value);
        }
        if self.max_value.is_none_or(|cur| value > cur) {
            self.max_value = Some(value);
        }
        self.buffer.push((value, weight));
        if self.buffer.len() >= self.buffer_cap {
            self.rebuild();
        }
    }

    /// Merge buffered values into the centroid ladder (k1 size bound).
    pub fn rebuild(&mut self) {
        if self.buffer.is_empty() {
            return;
        }
        let mut cs: Vec<(f64, f64)> = self
            .centroids
            .iter()
            .chain(self.buffer.iter())
            .copied()
            .collect();
        // tuples: (mean, weight)
        cs.sort_by(|a, b| {
            a.0.partial_cmp(&b.0)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.1.partial_cmp(&b.1).unwrap_or(Ordering::Equal))
        });
        self.buffer.clear();
        let total = cs.iter().map(|c| c.1).sum::<f64>();
        if total <= 0.0 {
            self.centroids = cs;
            return;
        }
        let mut out: Vec<(f64, f64)> = Vec::new();
        let mut acc_m = 0.0f64;
        let mut acc_w = 0.0f64;
        let mut open_acc = false;
        let mut w_so_far = 0.0f64;
        for (m, w) in cs {
            if !open_acc {
                acc_m = m;
                acc_w = w;
                open_acc = true;
                continue;
            }
            let q = (w_so_far + acc_w) / total;
            let step = 1.0 / total;
            let limit = total * (self.k1(q + step) - self.k1(q - step)) / 2.0;
            if acc_w + w <= limit.max(1.0) {
                acc_m = (acc_m * acc_w + m * w) / (acc_w + w);
                acc_w += w;
            } else {
                out.push((acc_m, acc_w));
                w_so_far += acc_w;
                acc_m = m;
                acc_w = w;
            }
        }
        if open_acc && acc_w > 0.0 {
            out.push((acc_m, acc_w));
        }
        self.centroids = out;
    }

    /// Interpolated quantile over the centroid ladder; `None` when empty or
    /// q out of [0, 1]. Tail centroids are near-singletons under k1, so
    /// p95/p99 land within one value of exact for typical sizing data.
    pub fn quantile(&mut self, q: f64) -> Option<f64> {
        if !self.buffer.is_empty() {
            self.rebuild(); // flush before answering
        }
        if self.centroids.is_empty() || !(0.0..=1.0).contains(&q) {
            return None;
        }
        if q <= 0.0 {
            return self.min_value;
        }
        if q >= 1.0 {
            return self.max_value;
        }
        let target = q * self.total;
        let mut cum = 0.0f64;
        for (i, (mean, weight)) in self.centroids.iter().enumerate() {
            let low = cum;
            let high = cum + weight;
            if target <= high {
                if i == 0 {
                    // interpolate between min and the first mean
                    let half = weight / 2.0;
                    if target <= half {
                        let frac = if half > 0.0 { target / half } else { 0.0 };
                        let lo = self.min_value.unwrap_or(*mean);
                        return Some(lo + frac * (mean - lo));
                    }
                    return Some(*mean);
                }
                let (prev_mean, prev_weight) = self.centroids[i - 1];
                // position inside the gap between neighboring means
                let width = weight / 2.0 + prev_weight / 2.0;
                let mut frac = if width > 0.0 {
                    (target - (low - prev_weight / 2.0)) / width
                } else {
                    0.0
                };
                frac = frac.clamp(0.0, 1.0);
                return Some(prev_mean + frac * (mean - prev_mean));
            }
            cum = high;
        }
        self.max_value
    }

    /// Merge another digest: its centroids + buffer stream through add
    /// (oracle `merge`; note the merged digest uses the DEFAULT buffer cap,
    /// exactly like `new TDigest(this.delta)`).
    pub fn merge(&self, other: &TDigest) -> TDigest {
        let mut merged = TDigest::new(self.delta, 10_000);
        merged.centroids = self.centroids.clone();
        merged.total = self.total;
        merged.n = self.n;
        merged.min_value = self.min_value;
        merged.max_value = self.max_value;
        for (value, weight) in other.centroids.iter().chain(other.buffer.iter()) {
            merged.add(*value, *weight);
        }
        merged
    }

    /// Full digest state (fixture capture shape).
    pub fn to_json(&self) -> Value {
        let pairs = |src: &[(f64, f64)]| -> Vec<Value> {
            src.iter()
                .map(|(m, w)| json!([jnum(*m), jnum(*w)]))
                .collect()
        };
        json!({
            "delta": jnum(self.delta),
            "bufferCap": self.buffer_cap,
            "centroids": pairs(&self.centroids),
            "buffer": pairs(&self.buffer),
            "n": self.n,
            "total": jnum(self.total),
            "minValue": self.min_value.map(jnum).unwrap_or(Value::Null),
            "maxValue": self.max_value.map(jnum).unwrap_or(Value::Null),
        })
    }
}

// ---------------------------------------------------------------------------
// Sizing report
// ---------------------------------------------------------------------------

/// One sizing card: heavy hitters (when keys are given) plus quantiles.
/// Honest about the path taken: EXACT sorted quantiles when the input fits
/// (n <= 1000), TDIGEST above that, ABSTAIN past 200000 (oracle
/// `sizingReport`). Quantile-point object keys render via `String(q)`.
pub fn sizing_report(
    sizes: &[f64],
    keys: Option<&[String]>,
    k: usize,
    quantiles: Option<&[f64]>,
) -> Result<Value, SketchError> {
    let values: Vec<f64> = sizes.to_vec();
    let default_points = [0.5, 0.9, 0.95, 0.99];
    let points: &[f64] = quantiles.unwrap_or(&default_points);
    let mut out = Map::new();
    out.insert("n".into(), json!(values.len()));
    out.insert(
        "quantile_points".into(),
        json!(points.iter().map(|q| jnum(*q)).collect::<Vec<_>>()),
    );
    if values.len() > DIGEST_MAX_VALUES {
        out.insert(
            "quantiles".into(),
            json!({
                "verdict": "ABSTAIN",
                "note": format!("more than {DIGEST_MAX_VALUES} values: the digest abstains"),
            }),
        );
    } else if values.len() <= 1000 {
        let mut srt = values.clone();
        srt.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
        // oracle: qvals[String(q)] = srt[min(trunc(q * len), len - 1)] — with
        // an empty srt the index is srt[-1] === undefined and JSON.stringify
        // drops the key (as do we)
        let mut qvals = Map::new();
        for q in points {
            if srt.is_empty() {
                continue;
            }
            let idx = (q * srt.len() as f64).trunc().min(srt.len() as f64 - 1.0);
            qvals.insert(js_num_str(*q), jnum(srt[idx as usize]));
        }
        let mut qobj = Map::new();
        qobj.insert("verdict".into(), json!("EXACT"));
        qobj.insert("values".into(), Value::Object(qvals));
        if let Some(first) = srt.first() {
            qobj.insert("min".into(), jnum(*first));
        }
        if let Some(last) = srt.last() {
            qobj.insert("max".into(), jnum(*last));
        }
        qobj.insert("sum".into(), jnum(values.iter().sum::<f64>()));
        out.insert("quantiles".into(), Value::Object(qobj));
    } else {
        let mut digest = TDigest::new(100.0, 10_000);
        for x in &values {
            digest.add(*x, 1.0);
        }
        let mut qvals = Map::new();
        for q in points {
            let v = digest.quantile(*q);
            qvals.insert(js_num_str(*q), v.map(jnum).unwrap_or(Value::Null));
        }
        let mut qobj = Map::new();
        qobj.insert("verdict".into(), json!("TDIGEST"));
        qobj.insert("values".into(), Value::Object(qvals));
        qobj.insert(
            "min".into(),
            digest.min_value.map(jnum).unwrap_or(Value::Null),
        );
        qobj.insert(
            "max".into(),
            digest.max_value.map(jnum).unwrap_or(Value::Null),
        );
        qobj.insert("sum".into(), jnum(digest.total));
        qobj.insert("n_centroids".into(), json!(digest.centroids.len()));
        out.insert("quantiles".into(), Value::Object(qobj));
    }
    if let Some(keys) = keys {
        let hh = if keys.len() != values.len() {
            json!({
                "verdict": "ABSTAIN",
                "note": "keys and sizes must pair one-to-one",
            })
        } else if keys.len() > SS_MAX_ITEMS {
            json!({ "verdict": "ABSTAIN", "note": "too many items" })
        } else {
            let mut ss = SpaceSaving::new(k)?;
            for (i, key) in keys.iter().enumerate() {
                let size = values[i];
                ss.add(key, if size >= 1.0 { size.trunc() } else { 1.0 });
            }
            json!({
                "verdict": "OK",
                "k": k,
                "n_items": ss.n_items,
                "hitters": ss.heavy_hitters().iter().map(HeavyHitter::to_json).collect::<Vec<_>>(),
                "note": "counts are Space-Saving ESTIMATES with a reported max error, not exact sums",
            })
        };
        out.insert("heavy_hitters".into(), hh);
    }
    Ok(Value::Object(out))
}

/// Render a sizing card as human lines (pure; no printing) (oracle
/// `renderSizing`).
pub fn render_sizing(data: &Value) -> Vec<String> {
    let n = data.get("n").and_then(Value::as_f64).unwrap_or(0.0);
    let mut lines = vec![format!("sizing: {} values", js_num_str(n))];
    let empty = Map::new();
    let q = match data.get("quantiles") {
        Some(Value::Object(m)) => m,
        _ => &empty, // oracle: data.quantiles ?? {}
    };
    let verdict = q.get("verdict").and_then(Value::as_str).unwrap_or("");
    if verdict == "OK" || verdict == "EXACT" || verdict == "TDIGEST" {
        if let Some(Value::Object(values)) = q.get("values") {
            for (point, value) in values {
                let rendered = match value {
                    Value::Number(_) => js_locale_int(value.as_f64().unwrap_or(f64::NAN).round()),
                    Value::Null => "null".to_string(),
                    Value::Bool(b) => b.to_string(),
                    Value::String(s) => s.clone(),
                    _ => "[object Object]".to_string(),
                };
                // p${String(Math.trunc(Number(point) * 100)).padStart(2, '0')}
                let point_num = point.parse::<f64>().unwrap_or(f64::NAN);
                let label = js_num_str((point_num * 100.0).trunc());
                let padded = if label.len() < 2 {
                    format!("0{label}")
                } else {
                    label
                };
                lines.push(format!("  p{padded}: {rendered}"));
            }
        }
        lines.push(format!("  ({verdict})"));
    } else {
        let note = q.get("note").and_then(Value::as_str).unwrap_or("");
        lines.push(format!("  quantiles: ABSTAIN ({note})"));
    }
    if let Some(hh) = data.get("heavy_hitters") {
        let hh_verdict = hh.get("verdict").and_then(Value::as_str).unwrap_or("");
        if hh_verdict == "OK" {
            lines.push("  heavy hitters:".to_string());
            if let Some(hitters) = hh.get("hitters").and_then(Value::as_array) {
                for h in hitters {
                    let key = h.get("key").and_then(Value::as_str).unwrap_or("");
                    let count = h.get("count").and_then(Value::as_f64).unwrap_or(f64::NAN);
                    let share = h.get("share").and_then(Value::as_f64).unwrap_or(f64::NAN);
                    let max_error = h
                        .get("max_error")
                        .and_then(Value::as_f64)
                        .unwrap_or(f64::NAN);
                    lines.push(format!(
                        "    {key}: {} (share {}%, max error {})",
                        js_locale_int(count),
                        js_to_fixed(share * 100.0, 2),
                        js_locale_int(max_error),
                    ));
                }
            }
        } else {
            let note = hh.get("note").and_then(Value::as_str).unwrap_or("");
            lines.push(format!("  heavy hitters: ABSTAIN ({note})"));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn space_saving_evicts_min_and_carries_error() {
        let mut ss = SpaceSaving::new(3).unwrap_or_else(|e| panic!("{e}"));
        for key in ["a", "b", "a", "c", "a", "b", "d", "e", "f"] {
            ss.add(key, 1.0);
        }
        let hitters = ss.heavy_hitters();
        assert_eq!(hitters.len(), 3);
        // 'a' survived with 3 occurrences; the rest passed through eviction
        assert_eq!(hitters[0].key, "a");
        assert_eq!(hitters[0].count, 3.0);
        assert_eq!(hitters[0].max_error, 0.0);
        assert!(hitters.iter().all(|h| h.max_error >= 0.0));
        assert_eq!(ss.n_items, 9);
        assert_eq!(ss.n_total, 9.0);
    }

    #[test]
    fn zero_and_negative_weights_are_ignored() {
        let mut ss = SpaceSaving::new(2).unwrap_or_else(|e| panic!("{e}"));
        ss.add("x", 2.5);
        ss.add("z", 0.0);
        ss.add("q", -3.0);
        assert_eq!(ss.n_items, 1);
        assert_eq!(ss.n_total, 2.5);
        assert_eq!(ss.heavy_hitters().len(), 1);
    }

    #[test]
    fn invalid_k_is_an_error() {
        assert!(matches!(SpaceSaving::new(0), Err(SketchError::KInvalid)));
        assert_eq!(SketchError::KInvalid.to_string(), "k must be >= 1");
    }

    #[test]
    fn merge_sums_counts_and_errors() {
        let mut left = SpaceSaving::new(3).unwrap_or_else(|e| panic!("{e}"));
        for (k, w) in [("a", 1.0), ("a", 1.0), ("b", 1.0), ("c", 1.0)] {
            left.add(k, w);
        }
        let mut right = SpaceSaving::new(3).unwrap_or_else(|e| panic!("{e}"));
        for (k, w) in [("a", 1.0), ("d", 1.0), ("d", 1.0), ("d", 1.0), ("b", 1.0)] {
            right.add(k, w);
        }
        let merged = left.merge(&right).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(merged.n_items, 9);
        assert_eq!(merged.n_total, 9.0);
        let hitters = merged.heavy_hitters();
        assert_eq!(hitters[0].key, "a");
        assert_eq!(hitters[0].count, 3.0);
    }

    #[test]
    fn tdigest_quantile_bounds_and_flush() {
        let mut td = TDigest::new(100.0, 10_000);
        assert_eq!(td.quantile(0.5), None, "empty digest abstains");
        assert_eq!(td.quantile(-0.5), None);
        assert_eq!(td.quantile(1.5), None);
        for v in 1..=10 {
            td.add(v as f64, 1.0);
        }
        assert_eq!(td.quantile(0.0), Some(1.0));
        assert_eq!(td.quantile(1.0), Some(10.0));
        let mid = td.quantile(0.5).unwrap_or(f64::NAN);
        assert!((4.0..=7.0).contains(&mid), "median near 5.5, got {mid}");
        assert!(td.buffer.is_empty(), "quantile flushed the buffer");
    }

    #[test]
    fn to_fixed_is_exact_round_half_up() {
        assert_eq!(js_to_fixed(24.72, 2), "24.72");
        assert_eq!(js_to_fixed(0.125, 2), "0.13"); // tie -> larger n
        assert_eq!(js_to_fixed(-0.125, 2), "-0.13"); // sign from x, n from |x|
        assert_eq!(js_to_fixed(-0.0004, 2), "-0.00");
        assert_eq!(js_to_fixed(0.0, 2), "0.00");
        assert_eq!(js_to_fixed(1.005, 2), "1.00"); // 1.005 is really 1.00499...
    }

    #[test]
    fn locale_grouping_matches_en_us() {
        assert_eq!(js_locale_int(1024.0), "1,024");
        assert_eq!(js_locale_int(333.0), "333");
        assert_eq!(js_locale_int(0.0), "0");
        assert_eq!(js_locale_int(1_234_567.0), "1,234,567");
    }

    #[test]
    fn sizing_report_exact_and_abstain_paths() {
        let report =
            sizing_report(&[3.0, 1.0, 2.0], None, 8, None).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(report["quantiles"]["verdict"], "EXACT");
        assert_eq!(report["quantiles"]["min"], 1);
        assert_eq!(report["quantiles"]["max"], 3);
        assert_eq!(report["quantiles"]["values"]["0.5"], 2);

        let empty = sizing_report(&[], None, 8, None).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(empty["quantiles"]["verdict"], "EXACT");
        assert!(empty["quantiles"]["values"].as_object().is_some());

        let big = vec![0.0; DIGEST_MAX_VALUES + 1];
        let abstain = sizing_report(&big, None, 8, None).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(abstain["quantiles"]["verdict"], "ABSTAIN");
    }
}
