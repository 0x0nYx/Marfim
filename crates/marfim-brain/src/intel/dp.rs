//! Port of oracle `src/intel/dp.js` — Laplace-mechanism differential
//! privacy for export artifacts.
//!
//! Mechanism (Dwork, McSherry, Nissim & Smith 2006), composed per Dwork &
//! Roth 2014. Each noised value = value + Laplace(0, b), b =
//! sensitivity/epsilon, sampled EXACTLY by inverse CDF from ONE uniform draw:
//!
//! ```text
//! u = rng.random()   // one uniform in [0, 1)
//! noise = -b * sign(u - 0.5) * ln(1 - 2*|u - 0.5|)
//! ```
//!
//! which is the standard inverse of the Laplace CDF. Deterministic given the
//! rng; one uniform draw per call. The tail u == 0.0 exactly (measure ~2^-53)
//! would make ln(0) undefined; the tail is floored at TAIL_FLOOR = 1e-15 so
//! the noise is finite — a documented tail treatment of the mechanism's own
//! sampling edge, never a clamp of user data.
//!
//! REPRODUCIBLE-NOISE TRADEOFF (verbatim from the oracle): by default the
//! seed is derived deterministically from the diff's content id
//! (sha256[:12] of the canonical render, parsed as hex), so noising the same
//! diff twice yields byte-identical output. Pass an explicit seed for fresh
//! noise. Re-noising an already-noised diff (a dp provenance marker
//! present) is REJECTED.
//!
//! PORT NOTES:
//! - The oracle's `rng` is the CPython-exact MT19937
//!   (`src/intel/util/mt19937.js`); the PRNG is re-implemented here
//!   module-locally, bit-for-bit, so seeded runs match the fixtures exactly.
//! - `fixedRng(u)` becomes [`FixedRng`], a stub whose single uniform is
//!   fixed — it pins the inverse-CDF formula in tests.
//! - `pyRound`/`pyFormat` (`src/intel/util/pymath.js`) are re-implemented
//!   here with exact integer arithmetic (half-to-even over the EXACT binary
//!   value of the double).
//! - `sha256Hex` (`src/intel/util/canonicalJson.js`) is re-implemented here
//!   (pure SHA-256, lowercase hex) and shared with `dedupe.rs`.
//! - NARROWING: the oracle accepts `seed` as number OR bigint; Rust's type
//!   system only carries finite numbers (`f64`), so the bigint branch and
//!   the "seed must be an integer or null" runtime rejection for non-number
//!   seeds are unrepresentable (a non-finite f64 still errors, message
//!   identical). localeCompare-based row sorting is replicated with plain
//!   codepoint ordering; all fixture row texts are lowercase-ASCII where
//!   Node's localeCompare coincides with codepoint order (asserted in the
//!   fixture generator).

pub const DEFAULT_EPSILON: f64 = 1.0;
pub const N_SENSITIVITY: f64 = 1.0;
pub const P_SENSITIVITY: f64 = 1.0;
pub const TAIL_FLOOR: f64 = 1e-15;

pub const DIFF_HEADER: &str = "CAELESTIA LEXICON DIFF v1";

pub const GUARANTEE: &str = "noised-evidence DP: per-row n/p are Laplace-noised (Dwork et al. 2006); row PRESENCE is exact unless presence_keep is set — not full row-level DP";

const GENERIC_COMMENT_SUFFIX: &str =
    " — counts and rates are Laplace-noised (Dwork et al. 2006); presence is exact; see cortex/dp.js";

/// Error message carrier — byte-exact against the oracle's Error.message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DpError(pub String);

impl std::fmt::Display for DpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DpError {}

/// Any `{random()}` uniform source (oracle: MT19937 or fixedRng stub).
pub trait Random {
    fn random(&mut self) -> f64;
}

/// A rng stub whose single uniform is fixed — pins the inverse-CDF formula
/// (oracle `fixedRng(u)`).
#[derive(Debug, Clone)]
pub struct FixedRng {
    pub u: f64,
}

impl Random for FixedRng {
    fn random(&mut self) -> f64 {
        self.u
    }
}

// ---------------------------------------------------------------------------
// MT19937 — CPython-exact Mersenne Twister, ported from the oracle's
// src/intel/util/mt19937.js (seed via init_by_array over the 32-bit
// little-endian words of the ABSOLUTE seed value; random() = genrand_res53).
// Only the surface dp.js needs is ported (seed + random()).
// ---------------------------------------------------------------------------

const MT_N: usize = 624;
const MT_M: usize = 397;
const MATRIX_A: u32 = 0x9908_b0df;
const UPPER_MASK: u32 = 0x8000_0000;
const LOWER_MASK: u32 = 0x7fff_ffff;

/// CPython-exact MT19937 (private to this module; fixtures pin it).
#[derive(Debug, Clone)]
pub struct Mt19937 {
    mt: [u32; MT_N],
    mti: usize,
}

impl Mt19937 {
    pub fn new(seed: u64) -> Self {
        let mut t = Self {
            mt: [0; MT_N],
            mti: MT_N + 1,
        };
        t.seed(seed);
        t
    }

    /// CPython random_seed(): key = 32-bit LE words of abs(seed); 0 -> [0].
    pub fn seed(&mut self, value: u64) {
        let mut words: Vec<u32> = Vec::new();
        if value == 0 {
            words.push(0);
        } else {
            let mut v = value;
            while v > 0 {
                words.push((v & 0xffff_ffff) as u32);
                v >>= 32;
            }
        }
        self.init_by_array(&words);
    }

    fn init_genrand(&mut self, s: u32) {
        self.mt[0] = s;
        for i in 1..MT_N {
            let prev = self.mt[i - 1];
            self.mt[i] = 1812433253u32
                .wrapping_mul(prev ^ (prev >> 30))
                .wrapping_add(i as u32);
        }
        self.mti = MT_N;
    }

    /// CPython random_init_by_array / MT init_by_array.
    fn init_by_array(&mut self, init_key: &[u32]) {
        self.init_genrand(19650218);
        let mut i = 1usize;
        let mut j = 0usize;
        let key_length = init_key.len();
        let mut k = MT_N.max(key_length);
        while k > 0 {
            let prev = self.mt[i - 1];
            self.mt[i] = (self.mt[i] ^ (prev ^ (prev >> 30)).wrapping_mul(1664525))
                .wrapping_add(init_key[j])
                .wrapping_add(j as u32);
            i += 1;
            j += 1;
            if i >= MT_N {
                self.mt[0] = self.mt[MT_N - 1];
                i = 1;
            }
            if j >= key_length {
                j = 0;
            }
            k -= 1;
        }
        let mut k = MT_N - 1;
        while k > 0 {
            let prev = self.mt[i - 1];
            self.mt[i] = (self.mt[i] ^ (prev ^ (prev >> 30)).wrapping_mul(1566083941))
                .wrapping_sub(i as u32);
            i += 1;
            if i >= MT_N {
                self.mt[0] = self.mt[MT_N - 1];
                i = 1;
            }
            k -= 1;
        }
        self.mt[0] = 0x8000_0000; // MSB is 1; assuring non-zero initial array
        self.mti = MT_N;
    }

    /// Standard MT19937 genrand_int32 with tempering.
    fn genrand_int32(&mut self) -> u32 {
        if self.mti >= MT_N {
            let mut kk = 0usize;
            while kk < MT_N - MT_M {
                let y = (self.mt[kk] & UPPER_MASK) | (self.mt[kk + 1] & LOWER_MASK);
                self.mt[kk] = self.mt[kk + MT_M] ^ (y >> 1) ^ if y & 1 != 0 { MATRIX_A } else { 0 };
                kk += 1;
            }
            while kk < MT_N - 1 {
                let y = (self.mt[kk] & UPPER_MASK) | (self.mt[kk + 1] & LOWER_MASK);
                self.mt[kk] =
                    self.mt[kk - (MT_N - MT_M)] ^ (y >> 1) ^ if y & 1 != 0 { MATRIX_A } else { 0 };
                kk += 1;
            }
            let y = (self.mt[MT_N - 1] & UPPER_MASK) | (self.mt[0] & LOWER_MASK);
            self.mt[MT_N - 1] =
                self.mt[MT_M - 1] ^ (y >> 1) ^ if y & 1 != 0 { MATRIX_A } else { 0 };
            self.mti = 0;
        }
        let mut y = self.mt[self.mti];
        self.mti += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c_5680;
        y ^= (y << 15) & 0xefc6_0000;
        y ^= y >> 18;
        y
    }
}

impl Random for Mt19937 {
    /// CPython random.random(): 53-bit genrand_res53.
    fn random(&mut self) -> f64 {
        let a = (self.genrand_int32() >> 5) as f64; // 27 bits
        let b = (self.genrand_int32() >> 6) as f64; // 26 bits
        (a * 67108864.0 + b) * (1.0 / 9007199254740992.0)
    }
}

// ---------------------------------------------------------------------------
// JS-number and Python-math helpers (origin comments inline).
// ---------------------------------------------------------------------------

/// JS `Number -> String` for the values dp.js interpolates into messages
/// and the method string. Rust's `{}` already produces the shortest
/// round-trip form; the special forms match JS spellings.
pub(crate) fn js_num(x: f64) -> String {
    if x.is_nan() {
        return "NaN".to_string();
    }
    if x == f64::INFINITY {
        return "Infinity".to_string();
    }
    if x == f64::NEG_INFINITY {
        return "-Infinity".to_string();
    }
    if x == 0.0 {
        return "0".to_string(); // JS String(-0) === "0"
    }
    format!("{x}")
}

/// Decompose |x| into (mantissa, binary exponent) — the exact m*2^e
/// (origin: oracle util/pymath.js decomposeDouble). Shared with dedupe.rs.
pub(crate) fn decompose_f64(x: f64) -> (u64, i32) {
    let bits = x.to_bits();
    let biased = ((bits >> 52) & 0x7ff) as i32;
    let mant = bits & 0x000f_ffff_ffff_ffff;
    if biased == 0 {
        return (mant, -1074); // subnormal (or zero)
    }
    (mant | (1u64 << 52), biased - 1075)
}

/// Python `round(x, digits)` — half-to-even over the EXACT binary value of
/// the double (origin: oracle util/pymath.js pyRound). `digits` may be 0..=15
/// (dp.js only ever uses 0 implicitly and 2); `x` non-finite passes through
/// unchanged, exactly like the oracle. Values whose exact scaling would
/// exceed 128-bit arithmetic are returned unchanged, which is provably the
/// same result (their ulp is far below the rounding quantum).
pub(crate) fn py_round(x: f64, digits: i32) -> f64 {
    if !x.is_finite() || x == 0.0 {
        return x; // 0 and -0 preserved
    }
    let d = digits.clamp(0, 15) as u32;
    let neg = x < 0.0;
    let (mant, e) = decompose_f64(x.abs());
    let m = mant as u128 * 10u128.pow(d); // < 2^53 * 2^50 = 2^103
    let q: u128 = if e >= 0 {
        let shift = e as u32;
        if shift >= 128 || (m >> (127 - shift)) != 0 {
            return x; // huge value: rounding to 2 decimals cannot change it
        }
        m << shift
    } else {
        let k = (-e) as u32;
        if k >= 128 {
            // den = 2^k exceeds u128, but den > 2r for every r < 2^103,
            // so the half-even tie can never fire: round down.
            0
        } else {
            let den = 1u128 << k;
            let q0 = m / den;
            let r = m % den;
            let twice = r * 2; // r < den <= 2^127 -> 2r < 2^128, no overflow
            if twice > den || (twice == den && q0 % 2 == 1) {
                q0 + 1
            } else {
                q0
            }
        }
    };
    let sign = if neg && q != 0 { -1.0 } else { 1.0 };
    sign * (q as f64) / (10u128.pow(d) as f64)
}

/// Python `f"{p:.2f}"` — half-even at 2 places, always 2 decimals
/// (oracle dp.js pyFormat2). The value is already banker-rounded to a
/// 2-decimal rational, so `%.2f` re-rendering is exact.
fn py_format2(p: f64) -> String {
    format!("{:.2}", py_round(p, 2))
}

/// Python `%.6g` (origin: oracle util/pymath.js pyFormat, spec 'g', prec 6).
/// The %f branch (exponent in [-4, 6)) is the exercised path; the %e branch
/// is ported best-effort for completeness.
pub(crate) fn py_format_g(x: f64) -> String {
    if x.is_nan() {
        return "nan".to_string();
    }
    if x == f64::INFINITY {
        return "inf".to_string();
    }
    if x == f64::NEG_INFINITY {
        return "-inf".to_string();
    }
    if x == 0.0 {
        return "0".to_string();
    }
    let sign = if x < 0.0 { "-" } else { "" };
    let ax = x.abs();
    let exp = ax.log10().floor();
    if !exp.is_finite() {
        return format!("{sign}{ax}");
    }
    let exp = exp as i32;
    if !(-4..6).contains(&exp) {
        // %e with 5 fraction digits, trailing zeros stripped, Python exponent
        let s = format!("{:.5e}", ax);
        let (mant, e2) = match s.split_once('e') {
            Some(pair) => pair,
            None => return format!("{sign}{ax}"),
        };
        let ev: i64 = e2.parse().unwrap_or(0);
        let mant = mant.trim_end_matches('0').trim_end_matches('.');
        let sign_e = if ev < 0 { "-" } else { "+" };
        format!("{sign}{mant}e{sign_e}{:02}", ev.abs())
    } else {
        let frac = (6 - 1 - exp).max(0) as usize;
        let mut s = format!("{:.*}", frac, ax);
        if s.contains('.') {
            while s.ends_with('0') {
                s.pop();
            }
            if s.ends_with('.') {
                s.pop();
            }
        }
        format!("{sign}{s}")
    }
}

/// Pure SHA-256 over UTF-8 bytes -> lowercase hex digest
/// (origin: oracle util/canonicalJson.js sha256Hex). Shared with dedupe.rs.
pub(crate) fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let bit_len = (data.len() as u64) * 8;
    let padded_len = (((data.len() + 8) >> 6) + 1) << 6;
    let mut msg = vec![0u8; padded_len];
    msg[..data.len()].copy_from_slice(data);
    msg[data.len()] = 0x80;
    let len_bytes = bit_len.to_be_bytes();
    msg[padded_len - 8..].copy_from_slice(&len_bytes);

    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut w = [0u32; 64];
    for off in (0..padded_len).step_by(64) {
        for (i, chunk) in msg[off..off + 64].as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ ((!v[4]) & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v[7] = v[6];
            v[6] = v[5];
            v[5] = v[4];
            v[4] = v[3].wrapping_add(t1);
            v[3] = v[2];
            v[2] = v[1];
            v[1] = v[0];
            v[0] = t1.wrapping_add(t2);
        }
        for i in 0..8 {
            h[i] = h[i].wrapping_add(v[i]);
        }
    }
    let mut out = String::with_capacity(64);
    for word in h {
        out.push_str(&format!("{word:08x}"));
    }
    out
}

// ---------------------------------------------------------------------------
// Row model.
// ---------------------------------------------------------------------------

/// One lexicon-diff row. `None` = the field is absent/null in the oracle
/// (JS collapses null and undefined at every use site here).
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct DiffRow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dp_epsilon: Option<f64>,
}

/// The noised diff: the rendered TEXT or the export row dicts (oracle union).
#[derive(Debug, Clone)]
pub enum DiffInput {
    Rows(Vec<DiffRow>),
    Text(String),
}

/// noiseDiff options (oracle destructuring defaults: epsilon = 1.0,
/// seed = null, minCountFloor = true, date = '', presenceKeep = null).
#[derive(Debug, Clone, Default)]
pub struct NoiseOptions {
    pub epsilon: Option<f64>,
    pub seed: Option<f64>,
    pub min_count_floor: bool,
    pub date: String,
    pub presence_keep: Option<f64>,
}

/// One noised output row (oracle outRows entries).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct NoisedRow {
    pub text: String,
    pub surface: String,
    pub n: i64,
    pub p: f64,
    pub label: i64,
    pub dp_epsilon: f64,
}

/// Full noiseDiff report — byte-exact field set with the oracle's return.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct NoiseReport {
    pub text: String,
    pub rows: Vec<NoisedRow>,
    pub epsilon: f64,
    pub seed: f64,
    pub rows_in: usize,
    pub rows_out: usize,
    pub rows_dropped: usize,
    pub n_floored_at_zero: usize,
    pub n_negative_raw: usize,
    pub p_clipped: usize,
    pub p_clipped_low: usize,
    pub p_clipped_high: usize,
    pub guarantee: &'static str,
    pub method: String,
    pub notes: Vec<String>,
}

fn row_text(row: &DiffRow) -> &str {
    row.text.as_deref().unwrap_or("")
}

fn row_surface(row: &DiffRow) -> &str {
    row.surface.as_deref().unwrap_or("")
}

// ---------------------------------------------------------------------------
// Core exports.
// ---------------------------------------------------------------------------

/// value + Laplace(0, b) noise with b = sensitivity / epsilon, sampled
/// exactly by the inverse CDF from ONE uniform draw of `rng`. sensitivity
/// <= 0 and epsilon <= 0 are REJECTED with the oracle's message.
pub fn laplace_noise(
    value: f64,
    sensitivity: f64,
    epsilon: f64,
    rng: &mut dyn Random,
) -> Result<f64, DpError> {
    let sens = sensitivity;
    let eps = epsilon;
    if sens <= 0.0 {
        return Err(DpError(format!(
            "sensitivity must be > 0 (got {})",
            js_num(sens)
        )));
    }
    if eps <= 0.0 {
        return Err(DpError(format!(
            "epsilon must be > 0 (got {})",
            js_num(eps)
        )));
    }
    let b = sens / eps;
    Ok(laplace_draw(value, b, rng))
}

/// The validated inverse-CDF draw (oracle laplaceNoise body after checks).
fn laplace_draw(value: f64, b: f64, rng: &mut dyn Random) -> f64 {
    let u = rng.random();
    let offset = u - 0.5;
    let sign = if offset > 0.0 {
        1.0
    } else if offset < 0.0 {
        -1.0
    } else {
        0.0 // u == 0.5: median, noise 0
    };
    let mut magnitude = 1.0 - 2.0 * offset.abs();
    if magnitude < TAIL_FLOOR {
        magnitude = TAIL_FLOOR;
    }
    value - b * sign * magnitude.ln()
}

/// Stable id for one imported set: sha256[:12] of the canonical rows
/// (id -> same set; any row change -> different id).
pub fn diff_id(rows: &[DiffRow]) -> String {
    let mut sorted: Vec<&DiffRow> = rows.iter().collect();
    sorted.sort_by(|a, b| {
        row_text(a)
            .cmp(row_text(b))
            .then_with(|| row_surface(a).cmp(row_surface(b)))
    });
    let canonical = render_rows_sorted(&sorted, "canonical");
    let hex = sha256_hex(canonical.as_bytes());
    hex.chars().take(12).collect()
}

/// The canonical diff text — deterministic for the same rows (+ date).
pub fn render_rows(rows: &[DiffRow], date: &str) -> String {
    let mut sorted: Vec<&DiffRow> = rows.iter().collect();
    sorted.sort_by(|a, b| {
        row_text(a)
            .cmp(row_text(b))
            .then_with(|| row_surface(a).cmp(row_surface(b)))
    });
    render_rows_sorted(&sorted, date)
}

/// The sort in the oracle is `localeCompare`-based; fixtures restrict row
/// texts to lowercase-ASCII where Node's localeCompare == codepoint order
/// (asserted generator-side), so plain byte ordering replicates it exactly.
fn render_rows_sorted(sorted: &[&DiffRow], date: &str) -> String {
    let mut lines = vec![if date.is_empty() {
        DIFF_HEADER.to_string()
    } else {
        format!("{DIFF_HEADER}  ({date})")
    }];
    for row in sorted {
        let sign = if row.label.unwrap_or(1.0).trunc() == 1.0 {
            "+"
        } else {
            "-"
        };
        let n = row.n.unwrap_or(1.0).trunc();
        let p = row.p.unwrap_or(0.0);
        lines.push(format!(
            "{sign}{} -> {}  (n={}, p={})",
            row_text(row),
            row_surface(row),
            js_num(n),
            py_format2(p)
        ));
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Python `str(float)` / f-string float for the marker (oracle
/// pyReprFloat): integer-valued numbers get a trailing ".0" (str(1.0) =
/// "1.0"); everything else uses the shortest round-trip spelling. The only
/// divergence is integer-valued |x| >= 1e21 (JS would print "1e+21.0"),
/// which no realistic epsilon reaches.
fn py_repr_float(x: f64) -> String {
    if x.is_finite() && x == x.trunc() {
        format!("{x}.0")
    } else {
        js_num(x)
    }
}

/// The provenance marker: Python f"(dp: epsilon={float(epsilon)})".
fn marker(epsilon: f64) -> String {
    format!("(dp: epsilon={})", py_repr_float(epsilon))
}

/// JSON.stringify of a string (used in the parse-error message).
fn json_quote(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default()
}

fn is_row_ws_byte(c: u8) -> bool {
    c == b' ' || c == b'\t' || c == b'\n' || c == b'\r' || c == 0x0b || c == 0x0c
}

/// Parse the rendered diff back into rows using the ONE row-format regex
/// (hand-rolled with the same lazy/backtracking semantics). Returns
/// (rows, date). Unparseable non-comment lines are REJECTED with the
/// oracle's message.
fn rows_from_text(text: &str) -> Result<(Vec<DiffRow>, String), DpError> {
    let mut rows: Vec<DiffRow> = Vec::new();
    let mut date = String::new();
    let mut lines: Vec<&str> = text.split('\n').collect();
    // Python splitlines: a trailing newline does not produce a final "" line
    if !lines.is_empty() && lines[lines.len() - 1].is_empty() {
        lines.pop();
    }
    for (idx, raw) in lines.iter().enumerate() {
        let stripped = raw.trim();
        if stripped.is_empty() || stripped.starts_with('#') {
            continue;
        }
        if let Some(rest) = stripped.strip_prefix(DIFF_HEADER) {
            let rest = rest.trim();
            if rest.len() >= 2 && rest.starts_with('(') && rest.ends_with(')') && date.is_empty() {
                date = rest[1..rest.len() - 1].to_string();
            }
            continue;
        }
        match parse_row_line(stripped) {
            Some(row) => rows.push(row),
            None => {
                let head: String = stripped.chars().take(60).collect();
                return Err(DpError(format!(
                    "line {}: unparseable diff row, refused: {}",
                    idx + 1,
                    json_quote(&head)
                )));
            }
        }
    }
    Ok((rows, date))
}

/// Mirror of ROW_RE
/// `^[ \t]*([+-])(.*?\S)[ \t]*->[ \t]*(\S+)(?:[ \t]*\(n=(\d+),[ \t]*p=([0-9.]+)\))?(?:[ \t]*\(dp:[ \t]*epsilon=([0-9.]+)\))?[ \t]*$`
/// with the regex engine's lazy-expansion + optional-group backtracking
/// order reproduced exactly.
fn parse_row_line(line: &str) -> Option<DiffRow> {
    let first = line.chars().next()?;
    if first != '+' && first != '-' {
        return None;
    }
    let rest = &line[1..]; // sign is ASCII: byte 1 is a char boundary
    for e in 1..=rest.len() {
        if !rest.is_char_boundary(e) {
            continue;
        }
        let candidate = &rest[..e];
        // .*?\S: the candidate must end with a non-whitespace character
        let Some(last_ch) = candidate.chars().next_back() else {
            continue;
        };
        if is_row_ws_byte(last_ch as u32 as u8) && (last_ch as u32) < 0x80 {
            continue;
        }
        // [ \t]* then '->'
        let mut j = e;
        while j < rest.len() && (rest.as_bytes()[j] == b' ' || rest.as_bytes()[j] == b'\t') {
            j += 1;
        }
        if !rest[j..].starts_with("->") {
            continue;
        }
        let after = &rest[j + 2..];
        // [ \t]* then (\S+): run of non-whitespace bytes (non-ASCII bytes
        // count as \S)
        let mut k0 = 0usize;
        while k0 < after.len() && (after.as_bytes()[k0] == b' ' || after.as_bytes()[k0] == b'\t') {
            k0 += 1;
        }
        let after = &after[k0..];
        let surf_end = after
            .bytes()
            .position(is_row_ws_byte)
            .unwrap_or(after.len());
        if surf_end == 0 {
            continue;
        }
        let surface_raw = &after[..surf_end];
        // optional groups, in the regex's backtracking order:
        // (n,p)(dp) -> (n,p) -> (dp) -> none, then [ \t]*$
        for (want_np, want_dp) in [(true, true), (true, false), (false, true), (false, false)] {
            let mut k = surf_end;
            let mut n_val = 1.0f64;
            let mut p_val = 0.0f64;
            let mut dp_val: Option<f64> = None;
            if want_np {
                match try_np_group(&after[k..]) {
                    Some((n, p, used)) => {
                        n_val = n;
                        p_val = p;
                        k += used;
                    }
                    None => continue,
                }
            }
            if want_dp {
                match try_dp_group(&after[k..]) {
                    Some((dp, used)) => {
                        dp_val = Some(dp);
                        k += used;
                    }
                    None => continue,
                }
            }
            let tail = &after[k..];
            if tail.bytes().all(is_row_ws_byte) {
                let p = p_val.clamp(0.0, 1.0);
                return Some(DiffRow {
                    text: Some(candidate.trim().to_string()),
                    surface: Some(surface_raw.trim().to_string()),
                    n: Some(n_val),
                    p: Some(p),
                    label: Some(if first == '+' { 1.0 } else { 0.0 }),
                    dp_epsilon: dp_val,
                });
            }
        }
    }
    None
}

/// `[ \t]*\(n=(\d+),[ \t]*p=([0-9.]+)\)` -> (n, p, bytes consumed).
fn try_np_group(s: &str) -> Option<(f64, f64, usize)> {
    let b = s.as_bytes();
    let mut k = 0usize;
    while k < b.len() && (b[k] == b' ' || b[k] == b'\t') {
        k += 1;
    }
    if !s[k..].starts_with("(n=") {
        return None;
    }
    k += 3;
    let digits_start = k;
    while k < b.len() && b[k].is_ascii_digit() {
        k += 1;
    }
    if k == digits_start {
        return None;
    }
    let n: f64 = s[digits_start..k].parse().ok()?; // parseInt(m[4], 10)
    if k >= b.len() || b[k] != b',' {
        return None;
    }
    k += 1;
    while k < b.len() && (b[k] == b' ' || b[k] == b'\t') {
        k += 1;
    }
    if !s[k..].starts_with("p=") {
        return None;
    }
    k += 2;
    let num_start = k;
    while k < b.len() && (b[k].is_ascii_digit() || b[k] == b'.') {
        k += 1;
    }
    if k == num_start {
        return None;
    }
    let p: f64 = s[num_start..k].parse().ok()?; // parseFloat(m[5])
    if k >= b.len() || b[k] != b')' {
        return None;
    }
    k += 1;
    Some((n, p, k))
}

/// `[ \t]*\(dp:[ \t]*epsilon=([0-9.]+)\)` -> (epsilon, bytes consumed).
fn try_dp_group(s: &str) -> Option<(f64, usize)> {
    let b = s.as_bytes();
    let mut k = 0usize;
    while k < b.len() && (b[k] == b' ' || b[k] == b'\t') {
        k += 1;
    }
    if !s[k..].starts_with("(dp:") {
        return None;
    }
    k += 4;
    while k < b.len() && (b[k] == b' ' || b[k] == b'\t') {
        k += 1;
    }
    if !s[k..].starts_with("epsilon=") {
        return None;
    }
    k += 8;
    let num_start = k;
    while k < b.len() && (b[k].is_ascii_digit() || b[k] == b'.') {
        k += 1;
    }
    if k == num_start {
        return None;
    }
    let eps: f64 = s[num_start..k].parse().ok()?; // parseFloat(m[6])
    if k >= b.len() || b[k] != b')' {
        return None;
    }
    k += 1;
    Some((eps, k))
}

/// sha256[:12] parsed back as hex (oracle `parseInt(diffId(rows), 16)`).
fn content_seed_of(rows: &[DiffRow]) -> u64 {
    let id = diff_id(rows);
    u64::from_str_radix(&id, 16).unwrap_or(0)
}

/// Noise one lexicon-diff export artifact (Laplace mechanism). `diff` is the
/// rendered diff TEXT or the export row dicts; an input already carrying a
/// dp provenance marker is REJECTED. Per row, in input order, the rng draws
/// are fixed-order: the presence draw first (when enabled), then n, then p.
/// Same (diff, epsilon, seed) -> byte-identical output; writes nothing.
pub fn noise_diff(diff: DiffInput, options: &NoiseOptions) -> Result<NoiseReport, DpError> {
    let eps = options.epsilon.unwrap_or(DEFAULT_EPSILON);
    if eps <= 0.0 {
        return Err(DpError(format!(
            "epsilon must be > 0 (got {})",
            js_num(eps)
        )));
    }
    let keep = match options.presence_keep {
        None => None,
        Some(k) => {
            if !(0.0 < k && k <= 1.0) {
                return Err(DpError(format!(
                    "presence_keep must be within (0, 1] (got {})",
                    js_num(k)
                )));
            }
            Some(k)
        }
    };

    // normalize input to rows (reusing the ONE row-format regex)
    let (rows, parsed_date): (Vec<DiffRow>, String) = match diff {
        DiffInput::Text(text) => {
            let (r, d) = rows_from_text(&text)?;
            (r, d)
        }
        DiffInput::Rows(v) => (v, String::new()),
    };
    if rows.is_empty() {
        return Err(DpError(
            "no rows to noise — the export is empty".to_string(),
        ));
    }
    for row in &rows {
        if row.dp_epsilon.is_some() {
            return Err(DpError(
                "this diff is already noised (a dp provenance marker is present) — re-noising \
                 would compose epsilon while claiming one epsilon; export the exact diff and \
                 noise once"
                    .to_string(),
            ));
        }
    }

    // the seed: derived from the content id (reproducible) or explicit
    let content_seed = content_seed_of(&rows);
    let seed_int: i64 = match options.seed {
        None => content_seed as i64,
        Some(s) => {
            if s.is_finite() {
                s.trunc() as i64
            } else {
                return Err(DpError(format!(
                    "seed must be an integer or null (got {})",
                    js_num(s)
                )));
            }
        }
    };
    // MT19937 takes the ABSOLUTE seed value (CPython random_seed)
    let mut rng = Mt19937::new(seed_int.unsigned_abs());

    let marker_str = marker(eps);
    let mut out_rows: Vec<NoisedRow> = Vec::new();
    let mut n_floored = 0usize;
    let mut n_negative_raw = 0usize;
    let mut p_clip_lo = 0usize;
    let mut p_clip_hi = 0usize;
    let mut dropped = 0usize;
    for row in &rows {
        if let Some(k) = keep {
            if rng.random() >= k {
                dropped += 1;
                continue;
            }
        }
        // n: Laplace noise, nearest integer (Math.floor(x + 0.5)), floored at
        // zero when min_count_floor — every floor COUNTED, never silent
        let raw_n = row.n.unwrap_or(1.0).trunc();
        let noised_n = laplace_draw(raw_n, N_SENSITIVITY / eps, &mut rng);
        let mut n_int = (noised_n + 0.5).floor() as i64;
        if n_int < 0 {
            if options.min_count_floor {
                n_int = 0;
                n_floored += 1;
            } else {
                n_negative_raw += 1;
            }
        }
        // p: Laplace noise, clipped to the valid [0, 1] RANGE and the clip
        // COUNTED and reported (DP post-processing, honest about itself)
        let raw_p = row.p.unwrap_or(0.0);
        let mut noised_p = laplace_draw(raw_p, P_SENSITIVITY / eps, &mut rng);
        if noised_p < 0.0 {
            noised_p = 0.0;
            p_clip_lo += 1;
        } else if noised_p > 1.0 {
            noised_p = 1.0;
            p_clip_hi += 1;
        }
        out_rows.push(NoisedRow {
            text: row.text.clone().unwrap_or_default(),
            surface: row.surface.clone().unwrap_or_default(),
            n: n_int,
            p: py_round(noised_p, 2),
            label: if row.label.unwrap_or(1.0).trunc() == 1.0 {
                1
            } else {
                0
            },
            dp_epsilon: eps,
        });
    }

    let used_date = if options.date.is_empty() {
        parsed_date
    } else {
        options.date.clone()
    };
    // the artifact: same canonical header, one provenance comment, and every
    // row carrying its marker — a reviewer can never mistake a noised diff
    let mut lines = vec![if used_date.is_empty() {
        DIFF_HEADER.to_string()
    } else {
        format!("{DIFF_HEADER}  ({used_date})")
    }];
    lines.push(format!("# {}{}", marker_str.trim(), GENERIC_COMMENT_SUFFIX));
    for row in &out_rows {
        let sign = if row.label == 1 { '+' } else { '-' };
        lines.push(format!(
            "{sign}{} -> {}  (n={}, p={}) {}",
            row.text,
            row.surface,
            js_num(row.n as f64),
            py_format2(row.p),
            marker_str
        ));
    }
    let mut text = lines.join("\n");
    text.push('\n');

    let method = format!(
        "Laplace mechanism (Dwork, McSherry, Nissim & Smith 2006): n and p noised at sensitivity \
         n={}, p={}, epsilon={} per export; composition: k sequential exports ~ k*epsilon",
        py_format_g(N_SENSITIVITY),
        py_format_g(P_SENSITIVITY),
        py_format_g(eps)
    );
    let seed_note = if seed_int as u64 == content_seed {
        "derived from the diff's content id — reproducible noise, NOT independent across exports \
         of the same content"
    } else {
        "explicitly supplied"
    };
    let notes = vec![
        GUARANTEE.to_string(),
        "composition: k sequential exports compose to roughly k*epsilon (Dwork & Roth 2014); the \
         export path is user-initiated and manual, so the practical bound is how often you export"
            .to_string(),
        format!("seed {} {}", js_num(seed_int as f64), seed_note),
    ];

    let rows_out = out_rows.len();
    let rows_in = rows.len();
    Ok(NoiseReport {
        text,
        rows: out_rows,
        epsilon: eps,
        seed: seed_int as f64,
        rows_in,
        rows_out,
        rows_dropped: dropped,
        n_floored_at_zero: n_floored,
        n_negative_raw,
        p_clipped: p_clip_lo + p_clip_hi,
        p_clipped_low: p_clip_lo,
        p_clipped_high: p_clip_hi,
        guarantee: GUARANTEE,
        method,
        notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed(u: f64) -> FixedRng {
        FixedRng { u }
    }

    #[test]
    fn mt19937_matches_cpython_first_draws() {
        // CPython: random.Random(42).random() -> 0.6394267984578837
        let mut rng = Mt19937::new(42);
        assert_eq!(rng.random(), 0.6394267984578837);
        assert_eq!(rng.random(), 0.025010755222666936);
        assert_eq!(rng.random(), 0.27502931836911926);
        // seed 0 -> key [0]
        let mut rng0 = Mt19937::new(0);
        assert_eq!(rng0.random(), 0.8444218515250481);
        // negative seeds use the absolute value
        let mut rng_neg = Mt19937::new((-42i64).unsigned_abs());
        assert_eq!(rng_neg.random(), 0.6394267984578837);
    }

    #[test]
    fn py_round_is_bankers_half_even() {
        assert_eq!(py_round(0.125, 2), 0.12);
        assert_eq!(py_round(0.375, 2), 0.38);
        assert_eq!(py_round(0.5, 0), 0.0);
        assert_eq!(py_round(1.5, 0), 2.0);
        assert_eq!(py_round(2.5, 0), 2.0);
        assert_eq!(py_round(-0.5, 0), -0.0);
        assert_eq!(py_round(2.675, 2), 2.67); // 2.675 is 2.67499999... in binary
        assert_eq!(py_round(0.0, 2), 0.0);
        assert_eq!(py_round(f64::INFINITY, 2), f64::INFINITY);
    }

    #[test]
    fn laplace_inverse_cdf_pins_formula() {
        // u == 0.5 -> zero noise
        let mut r = fixed(0.5);
        assert_eq!(
            laplace_noise(100.0, 1.0, 1.0, &mut r).unwrap_or(f64::NAN),
            100.0
        );
        // u == 0 -> tail floored: value + ln(1e-15)
        let mut r = fixed(0.0);
        let v = laplace_noise(100.0, 1.0, 1.0, &mut r).unwrap_or(f64::NAN);
        assert!(crate::util::ulp_close(v, 100.0 + (1e-15f64).ln()));
        // errors carry the oracle's message
        let mut r = fixed(0.5);
        assert_eq!(
            laplace_noise(1.0, 0.0, 1.0, &mut r).err().map(|e| e.0),
            Some("sensitivity must be > 0 (got 0)".to_string())
        );
        assert_eq!(
            laplace_noise(1.0, 1.0, 0.0, &mut r).err().map(|e| e.0),
            Some("epsilon must be > 0 (got 0)".to_string())
        );
    }

    #[test]
    fn sha256_known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn rows_from_text_roundtrip_and_reject() {
        let rows = vec![DiffRow {
            text: Some("alpha".into()),
            surface: Some("beta".into()),
            n: Some(3.0),
            p: Some(0.5),
            label: Some(1.0),
            dp_epsilon: None,
        }];
        let text = render_rows(&rows, "");
        let (back, date) = rows_from_text(&text).unwrap_or_default();
        assert!(date.is_empty());
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].text.clone().unwrap_or_default(), "alpha");
        assert_eq!(back[0].n, Some(3.0));
        assert_eq!(back[0].p, Some(0.5));
        assert_eq!(back[0].label, Some(1.0));
        let err = rows_from_text("CAELESTIA LEXICON DIFF v1\ngarbage line\n")
            .err()
            .map(|e| e.0);
        assert_eq!(
            err,
            Some("line 2: unparseable diff row, refused: \"garbage line\"".to_string())
        );
    }

    #[test]
    fn noise_diff_reproducible_and_guards() {
        let row = |t: &str| DiffRow {
            text: Some(t.to_string()),
            surface: Some(t.to_string()),
            n: Some(10.0),
            p: Some(0.5),
            label: Some(1.0),
            dp_epsilon: None,
        };
        let rows = vec![row("alpha"), row("bravo")];
        let opts = NoiseOptions::default();
        let a = noise_diff(DiffInput::Rows(rows.clone()), &opts)
            .unwrap_or_else(|e| panic!("noise: {e}"));
        let b = noise_diff(DiffInput::Rows(rows), &opts).unwrap_or_else(|e| panic!("noise: {e}"));
        assert_eq!(a, b, "same (diff, epsilon, seed) -> byte-identical output");
        assert_eq!(a.rows.len(), 2);
        // already-noised rows are rejected
        let marked = DiffRow {
            text: Some("x".into()),
            surface: Some("x".into()),
            n: Some(1.0),
            p: Some(0.5),
            label: Some(1.0),
            dp_epsilon: Some(1.0),
        };
        assert!(noise_diff(DiffInput::Rows(vec![marked]), &opts).is_err());
        assert!(noise_diff(DiffInput::Rows(vec![]), &opts).is_err());
    }
}
