//! Port of oracle `src/intel/brain/minhash.js` — MinHash + LSH banding for
//! near-duplicate detection.
//!
//! Signature length `num_perm` (64); LSH splits it into bands (16) so only
//! colliding documents are compared. Jaccard is the signature agreement
//! fraction; `near_duplicates` returns verified pairs sorted by score.
//!
//! Constants from the Python origin: `_P = 2^61-1` (the Mersenne prime for
//! the `(a*h + b) mod P` permutation family), `_MAX = 2^32-1` (signatures
//! fold to 32-bit lanes), perms from a seeded MT19937 (CPython
//! `random.Random(42)` semantics) drawn as `randrange(1, P)`,
//! `randrange(0, P)` per permutation — the rejection sampler below replicates
//! CPython's `_randbelow` exactly, so the permutation table (and hence every
//! signature) is bit-identical to the oracle (fixture-pinned).
//!
//! NARROWINGS (per the P4-B differential method):
//! - Oracle helpers from sibling files are inlined here with citations:
//!   MT19937 (`util/mt19937.js`, public for differential verification),
//!   blake2b (`blake2b.js` — the crate-level `brain/blake2b.rs` is owned by
//!   the P4 coordinator in parallel, so this module carries a private
//!   u64-lane copy; only `digest_size <= 8` is supported, which is what the
//!   brain pins), and `shingles`/`words` (`nlp.js`, ASCII `[a-z0-9]+`
//!   tokenisation — non-ASCII word characters are separators exactly as the
//!   oracle regex behaves; deletion/char indexing is char-based, so astral
//!   inputs are not exercised).
//! - MT19937 seeds are non-negative `u64` (the oracle accepts negative
//!   BigInt seeds via `abs`; unused by brain modules).
//! - `pyRound` is the shared inlined copy at [`crate::brain::pooling::py_round`].

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::brain::pooling::{jnum, py_round};

/// the Mersenne prime 2^61 - 1 (oracle `_P`).
pub const _P: u64 = (1u64 << 61) - 1;
/// signature lane mask 2^32 - 1 (oracle `_MAX`; `1 << 32` is 1 in JS).
pub const _MAX: u64 = 0xffff_ffff;

// ---------------------------------------------------------------------------
// Inlined from oracle `src/intel/util/mt19937.js` — CPython-exact Mersenne
// Twister. Public so the differential fixtures can replay raw getrandbits
// draws; the permutation table is the only in-crate consumer.
// ---------------------------------------------------------------------------
pub mod mt19937 {
    const N: usize = 624;
    const M: usize = 397;
    const MATRIX_A: u32 = 0x9908_b0df;
    const UPPER_MASK: u32 = 0x8000_0000;
    const LOWER_MASK: u32 = 0x7fff_ffff;

    /// CPython `random.Random` bit-for-bit (integer seeding via
    /// `init_by_array` over the 32-bit little-endian words of the seed).
    #[derive(Debug, Clone)]
    pub struct Mt19937 {
        mt: [u32; N],
        mti: usize,
    }

    impl Mt19937 {
        pub fn new(seed: u64) -> Self {
            let mut s = Self {
                mt: [0; N],
                mti: N + 1,
            };
            s.seed(seed);
            s
        }

        /// CPython `random_seed`: key = 32-bit LE words of the seed
        /// (0 -> single word `[0]`).
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
            for i in 1..N {
                let prev = self.mt[i - 1];
                self.mt[i] = 1812433253u32
                    .wrapping_mul(prev ^ (prev >> 30))
                    .wrapping_add(i as u32);
            }
            self.mti = N;
        }

        /// CPython `random_init_by_array` / MT `init_by_array`.
        fn init_by_array(&mut self, key: &[u32]) {
            self.init_genrand(19650218);
            let mut i = 1usize;
            let mut j = 0usize;
            let key_len = key.len();
            let mut k = N.max(key_len);
            while k > 0 {
                let prev = self.mt[i - 1];
                // toUint32(toUint32(mt[i] ^ imul(prev ^ (prev >>> 30), 1664525)) + key[j] + j)
                self.mt[i] = (self.mt[i] ^ (prev ^ (prev >> 30)).wrapping_mul(1664525))
                    .wrapping_add(key[j])
                    .wrapping_add(j as u32);
                i += 1;
                j += 1;
                if i >= N {
                    self.mt[0] = self.mt[N - 1];
                    i = 1;
                }
                if j >= key_len {
                    j = 0;
                }
                k -= 1;
            }
            let mut k = N - 1;
            while k > 0 {
                let prev = self.mt[i - 1];
                self.mt[i] = (self.mt[i] ^ (prev ^ (prev >> 30)).wrapping_mul(1566083941))
                    .wrapping_sub(i as u32);
                i += 1;
                if i >= N {
                    self.mt[0] = self.mt[N - 1];
                    i = 1;
                }
                k -= 1;
            }
            self.mt[0] = 0x8000_0000; // MSB is 1; assuring non-zero initial array
            self.mti = N;
        }

        /// Standard MT19937 `genrand_int32` with tempering.
        pub fn genrand_int32(&mut self) -> u32 {
            if self.mti >= N {
                let mut kk = 0usize;
                while kk < N - M {
                    let y = (self.mt[kk] & UPPER_MASK) | (self.mt[kk + 1] & LOWER_MASK);
                    self.mt[kk] =
                        self.mt[kk + M] ^ (y >> 1) ^ if y & 1 == 1 { MATRIX_A } else { 0 };
                    kk += 1;
                }
                while kk < N - 1 {
                    let y = (self.mt[kk] & UPPER_MASK) | (self.mt[kk + 1] & LOWER_MASK);
                    // JS: mt[kk] = mt[kk + (M - N)] ^ ... — write mt[kk],
                    // read the wrapped-lower index (kk + M) - N >= 0 here
                    self.mt[kk] =
                        self.mt[(kk + M) - N] ^ (y >> 1) ^ if y & 1 == 1 { MATRIX_A } else { 0 };
                    kk += 1;
                }
                let y = (self.mt[N - 1] & UPPER_MASK) | (self.mt[0] & LOWER_MASK);
                self.mt[N - 1] = self.mt[M - 1] ^ (y >> 1) ^ if y & 1 == 1 { MATRIX_A } else { 0 };
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

        /// CPython `getrandbits(k)` for k <= 64: little-endian words with the
        /// top-bits-trimmed final word. (The permutation family needs k = 61;
        /// the shared MT clamps JS Numbers at 53 bits, which is why the oracle
        /// draws BigInts — u64 is exact here.)
        pub fn getrandbits(&mut self, k: u32) -> u64 {
            if k == 0 {
                return 0;
            }
            let words = ((k - 1) / 32 + 1) as usize;
            let mut result: u64 = 0;
            let mut remaining = k;
            for i in 0..words {
                let mut r = self.genrand_int32();
                if remaining < 32 {
                    r >>= 32 - remaining; // drop least significant bits
                }
                result |= (r as u64) << (32 * i as u32);
                remaining = remaining.saturating_sub(32);
            }
            result
        }
    }
}

use mt19937::Mt19937;

/// CPython `random._randbelow_with_getrandbits` over 61-bit widths.
fn randbelow_big(rng: &mut Mt19937, n: u64) -> u64 {
    let k = 64 - n.leading_zeros(); // n.bit_length()
    let mut r = rng.getrandbits(k);
    while r >= n {
        r = rng.getrandbits(k);
    }
    r
}

fn randrange_big(rng: &mut Mt19937, lo: u64, hi: u64) -> u64 {
    lo + randbelow_big(rng, hi - lo)
}

/// The `(a, b)` permutation table, seeded exactly like the Python origin:
/// for each permutation, `a = randrange(1, P)`, `b = randrange(0, P)`
/// (oracle `_perms`; seed defaults to 42 at every in-module call site).
pub fn _perms(num_perm: usize, seed: u64) -> Vec<(u64, u64)> {
    let mut rng = Mt19937::new(seed);
    let mut out = Vec::with_capacity(num_perm);
    for _ in 0..num_perm {
        let a = randrange_big(&mut rng, 1, _P);
        let b = randrange_big(&mut rng, 0, _P);
        out.push((a, b));
    }
    out
}

// ---------------------------------------------------------------------------
// Inlined from oracle `src/intel/brain/blake2b.js` — BLAKE2b (RFC 7693) with
// caller-chosen digest length, u64-lane arithmetic. The crate-level
// brain/blake2b.rs is owned by the P4 coordinator; this private copy keeps
// minhash self-contained and fixture-verified.
// ---------------------------------------------------------------------------
mod blake2b {
    const IV: [u64; 8] = [
        0x6a09_e667_f3bc_c908,
        0xbb67_ae85_84ca_a73b,
        0x3c6e_f372_fe94_f82b,
        0xa54f_f53a_5f1d_36f1,
        0x510e_527f_ade6_82d1,
        0x9b05_688c_2b3e_6c1f,
        0x1f83_d9ab_fb41_bd6b,
        0x5be0_cd19_137e_2179,
    ];

    /// The 10-round message-word schedule (RFC 7693 section 2.7).
    const SIGMA: [[usize; 16]; 10] = [
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
        [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
        [11, 8, 12, 0, 5, 2, 15, 13, 10, 14, 3, 6, 7, 1, 9, 4],
        [7, 9, 3, 1, 13, 12, 11, 14, 2, 6, 5, 10, 4, 0, 15, 8],
        [9, 0, 5, 7, 2, 4, 10, 15, 14, 1, 11, 12, 6, 8, 3, 13],
        [2, 12, 6, 10, 0, 11, 8, 3, 4, 13, 7, 5, 15, 14, 1, 9],
        [12, 5, 1, 15, 14, 13, 4, 10, 0, 7, 6, 3, 9, 2, 8, 11],
        [13, 11, 7, 14, 12, 1, 3, 9, 5, 0, 15, 4, 8, 6, 2, 10],
        [6, 15, 14, 9, 11, 3, 0, 8, 12, 2, 13, 7, 1, 4, 10, 5],
        [10, 2, 8, 4, 7, 6, 1, 5, 15, 11, 9, 14, 3, 12, 13, 0],
    ];

    /// One blake2b compression over a 128-byte block (16 LE words).
    fn compress(h: &mut [u64; 8], block: &[u64; 16], t: u64, is_last: bool) {
        let mut v = [0u64; 16];
        v[..8].copy_from_slice(h);
        v[8..].copy_from_slice(&IV);
        v[12] ^= t;
        if is_last {
            v[14] ^= u64::MAX; // f0 = -1 (no key in use: f1 stays 0)
        }
        fn g(v: &mut [u64; 16], a: usize, b: usize, c: usize, d: usize, x: u64, y: u64) {
            v[a] = v[a].wrapping_add(v[b]).wrapping_add(x);
            v[d] = (v[d] ^ v[a]).rotate_right(32);
            v[c] = v[c].wrapping_add(v[d]);
            v[b] = (v[b] ^ v[c]).rotate_right(24);
            v[a] = v[a].wrapping_add(v[b]).wrapping_add(y);
            v[d] = (v[d] ^ v[a]).rotate_right(16);
            v[c] = v[c].wrapping_add(v[d]);
            v[b] = (v[b] ^ v[c]).rotate_right(63);
        }
        for r in 0..12 {
            let s = &SIGMA[r % 10];
            g(&mut v, 0, 4, 8, 12, block[s[0]], block[s[1]]);
            g(&mut v, 1, 5, 9, 13, block[s[2]], block[s[3]]);
            g(&mut v, 2, 6, 10, 14, block[s[4]], block[s[5]]);
            g(&mut v, 3, 7, 11, 15, block[s[6]], block[s[7]]);
            g(&mut v, 0, 5, 10, 15, block[s[8]], block[s[9]]);
            g(&mut v, 1, 6, 11, 12, block[s[10]], block[s[11]]);
            g(&mut v, 2, 7, 8, 13, block[s[12]], block[s[13]]);
            g(&mut v, 3, 4, 9, 14, block[s[14]], block[s[15]]);
        }
        for i in 0..8 {
            h[i] ^= v[i] ^ v[i + 8];
        }
    }

    fn block_words(bytes: &[u8], offset: usize, length: usize) -> [u64; 16] {
        let mut m = [0u64; 16];
        for i in 0..length {
            m[i >> 3] |= (bytes[offset + i] as u64) << (8 * (i & 7));
        }
        m
    }

    /// BLAKE2b digest bytes, `digest_size` in 1..=64 (the brain pins 8).
    pub fn blake2b(data: &[u8], digest_size: usize) -> Vec<u8> {
        let mut h = IV;
        // parameter block: digest_length in byte 0, fanout=1 depth=1
        h[0] ^= 0x0101_0000 ^ (digest_size as u64 & 0xff);
        let total = data.len();
        if total == 0 {
            compress(&mut h, &[0u64; 16], 0, true);
        } else {
            let mut off = 0usize;
            while off < total {
                let is_last = off + 128 >= total;
                let end = if is_last { total } else { off + 128 };
                compress(
                    &mut h,
                    &block_words(data, off, end - off),
                    end as u64,
                    is_last,
                );
                off = end;
            }
        }
        let mut out = vec![0u8; digest_size];
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = ((h[i >> 3] >> (8 * (i & 7))) & 0xff) as u8;
        }
        out
    }

    /// blake2b digest as an unsigned BIG-ENDIAN integer (the shape
    /// `int.from_bytes(hashlib.blake2b(...).digest(), "big")` gives). Only
    /// digest sizes <= 8 are supported (u64 exact); the brain pins 8.
    pub fn blake2b_int(data: &[u8], digest_size: usize) -> u64 {
        let mut v: u64 = 0;
        for b in blake2b(data, digest_size) {
            v = (v << 8) | b as u64;
        }
        v
    }
}

// ---------------------------------------------------------------------------
// Inlined from oracle `src/intel/brain/nlp.js` — tokenisation + shingling.
// ---------------------------------------------------------------------------
mod nlp {
    use std::collections::HashSet;

    /// Raw words of the text (lowercased `[a-z0-9]+` runs; every other
    /// character is a separator, exactly like the oracle regex).
    pub fn words(text: &str) -> Vec<String> {
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

    /// Word k-shingles as an insertion-ordered dedup set (JS `Set`
    /// semantics). Text shorter than k words yields the WHOLE text as ONE
    /// shingle; empty text yields an empty set.
    pub fn shingles(text: &str, k: usize) -> Vec<String> {
        let w = words(text);
        if w.len() < k {
            return if w.is_empty() {
                Vec::new()
            } else {
                vec![w.join(" ")]
            };
        }
        let mut out: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for i in 0..=(w.len() - k) {
            let sh = w[i..i + k].join(" ");
            if seen.insert(sh.clone()) {
                out.push(sh);
            }
        }
        out
    }
}

/// MinHash signature of one text, or `None` when it has no shingles
/// (oracle `signature`; `num_perm` defaults to 64 at the JS call sites).
pub fn signature(text: &str, num_perm: usize) -> Option<Vec<u32>> {
    let sh = nlp::shingles(text, 3);
    if sh.is_empty() {
        return None;
    }
    let perms = _perms(num_perm, 42);
    let hashes: Vec<u64> = sh
        .iter()
        .map(|s| blake2b::blake2b_int(s.as_bytes(), 8))
        .collect();
    Some(
        perms
            .iter()
            .map(|(a, b)| {
                // min over (a*h + b) mod P — u128 intermediate keeps the
                // 61-bit modular product exact
                let mut m = u64::MAX;
                for h in &hashes {
                    let v = ((*a as u128 * *h as u128 + *b as u128) % _P as u128) as u64;
                    if v < m {
                        m = v;
                    }
                }
                (m & _MAX) as u32
            })
            .collect(),
    )
}

/// Jaccard estimated from signature agreement (oracle `jaccard`). Length
/// mismatches count as disagreements (the JS compares against `undefined`);
/// `0/0` is NaN, mirroring JS division.
pub fn jaccard(sig_a: &[u32], sig_b: &[u32]) -> f64 {
    let mut same = 0usize;
    let n = sig_a.len();
    for (i, a) in sig_a.iter().enumerate() {
        if sig_b.get(i) == Some(a) {
            same += 1;
        }
    }
    same as f64 / n as f64
}

/// LSH near-duplicate finder (oracle `nearDuplicates`). `docs` maps doc ids
/// to texts; returns `[[id_a, id_b, est_jaccard], ...]` sorted by score desc
/// (stable, so lexicographic pair order survives ties). Docs without
/// shingles are skipped; the candidate pair order is deterministic
/// (lexicographic), independent of the docs map's iteration order.
pub fn near_duplicates(
    docs: &BTreeMap<String, String>,
    threshold: f64,
    bands: usize,
) -> Vec<(String, String, f64)> {
    // insertion-ordered sig list (JS Map) + O(1) lookup
    let mut sig_order: Vec<String> = Vec::new();
    let mut sigs: HashMap<String, Vec<u32>> = HashMap::new();
    for (k, text) in docs {
        if let Some(s) = signature(text, 64) {
            sig_order.push(k.clone());
            sigs.insert(k.clone(), s);
        }
    }
    if sig_order.is_empty() {
        return Vec::new();
    }
    let first_len = sigs.get(&sig_order[0]).map_or(0, Vec::len) as f64;
    let rows = (first_len / bands as f64).floor() as usize;
    let mut buckets: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for doc_id in &sig_order {
        let Some(sig) = sigs.get(doc_id) else {
            continue;
        };
        for b in 0..bands {
            let key = format!(
                "{}\u{0}{}",
                b,
                sig[b * rows..(b + 1) * rows]
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join("\u{1}")
            );
            buckets.entry(key).or_default().push(doc_id.clone());
        }
    }
    // deterministic candidate order (pairs sorted lexicographically)
    let mut candidates: BTreeSet<String> = BTreeSet::new();
    for ids in buckets.values() {
        for i in 0..ids.len() {
            for j in (i + 1)..ids.len() {
                let mut pair = [ids[i].as_str(), ids[j].as_str()];
                pair.sort_unstable(); // JS default sort: lexicographic
                candidates.insert(format!("{}\u{0}{}", pair[0], pair[1]));
            }
        }
    }
    let mut out: Vec<(String, String, f64)> = Vec::new();
    for pair in &candidates {
        // BTreeSet iteration == [...candidates].sort()
        let Some((a, b)) = pair.split_once('\u{0}') else {
            continue;
        };
        let (Some(sa), Some(sb)) = (sigs.get(a), sigs.get(b)) else {
            continue;
        };
        let score = jaccard(sa, sb);
        if score >= threshold {
            out.push((a.to_string(), b.to_string(), py_round(score, 3)));
        }
    }
    // stable: keeps the pair order on ties
    out.sort_by(|x, y| y.2.partial_cmp(&x.2).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// JSON value for a jaccard/near-duplicate score the way `JSON.stringify`
/// would emit it (NaN -> null, integral floats as bare integers).
pub fn score_to_json(x: f64) -> serde_json::Value {
    jnum(x)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perms_match_cpython_seeded_sequence() {
        let perms = _perms(2, 42);
        assert_eq!(
            perms[0],
            (256_711_980_439_665_054, 1_709_899_044_117_486_289)
        );
        assert_eq!(perms[1], (564_682_175_042_572_888, 321_752_549_611_034_334));
        // raw getrandbits(61) draws under seed 42 (oracle fixture values;
        // with this seed no rejection occurs, so a = draw1 + 1 and
        // b = draw2, matching _perms(2, 42) above exactly)
        let mut rng = Mt19937::new(42);
        assert_eq!(rng.getrandbits(61), 256_711_980_439_665_053);
        assert_eq!(rng.getrandbits(61), 1_709_899_044_117_486_289);
        assert_eq!(rng.getrandbits(61), 564_682_175_042_572_887);
    }

    #[test]
    fn blake2b_matches_python_vectors() {
        // oracle doc comment vectors (verified with python3):
        // blake2b(b'abc', 8) = d8bb14d833d59559, blake2b(b'', 8) = e4a6a0577479b2b4
        assert_eq!(blake2b::blake2b_int(b"abc", 8), 0xd8bb_14d8_33d5_9559);
        assert_eq!(blake2b::blake2b_int(b"", 8), 0xe4a6_a057_7479_b2b4);
    }

    #[test]
    fn empty_text_has_no_signature() {
        assert_eq!(signature("", 64), None);
        assert_eq!(signature("   .,;!", 64), None);
    }

    #[test]
    fn identical_texts_have_identical_signatures_and_jaccard_one() {
        let a = signature("the quick brown fox jumps over the lazy dog", 64)
            .unwrap_or_else(|| panic!("sig"));
        let b = signature("the quick brown fox jumps over the lazy dog", 64)
            .unwrap_or_else(|| panic!("sig"));
        assert_eq!(a, b);
        assert_eq!(jaccard(&a, &b), 1.0);
    }

    #[test]
    fn jaccard_length_mismatch_counts_disagreements() {
        assert_eq!(jaccard(&[1, 2, 3], &[1, 2, 3, 4]), 1.0);
        assert_eq!(jaccard(&[1, 2, 3, 9], &[1, 2, 3]), 0.75);
        assert!(jaccard(&[], &[]).is_nan());
    }

    #[test]
    fn near_duplicates_skips_shingleless_docs() {
        let mut docs = BTreeMap::new();
        docs.insert("empty".to_string(), String::new());
        assert!(near_duplicates(&docs, 0.8, 16).is_empty());
    }
}
