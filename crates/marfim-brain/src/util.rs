//! Determinism helpers shared by every ported module: f64 comparison at
//! a 1-ulp tolerance, a seeded xorshift RNG (fixture-replayable), and
//! stable JSON helpers.

/// Compare two f64s as <= 1 ulp apart (or both NaN, or exact equal).
/// JS numbers are f64; elementary ops (+,-,*,/,sqrt) round identically
/// in Rust, so ports are usually BIT-exact and this tolerance is the
/// documented maximum for transcendentals.
pub fn ulp_close(a: f64, b: f64) -> bool {
    if a == b {
        return true;
    }
    if a.is_nan() && b.is_nan() {
        return true;
    }
    let (a_bits, b_bits) = (a.to_bits(), b.to_bits());
    // map to an ordered integer space (monotone over the f64 total order)
    let sa = if a_bits >> 63 == 0 {
        a_bits as i64
    } else {
        !(a_bits as i64)
    };
    let sb = if b_bits >> 63 == 0 {
        b_bits as i64
    } else {
        !(b_bits as i64)
    };
    (sa - sb).abs() <= 1
}

/// Deterministic PRNG (xorshift64*): same seed => same sequence, on every
/// platform, forever. The oracle's `fixedRng` seeds behave identically.
#[derive(Debug, Clone)]
pub struct SeededRng {
    state: u64,
}

impl SeededRng {
    pub fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 { 0x9E3779B97F4A7C15 } else { seed },
        }
    }

    pub fn next_u64(&mut self) -> u64 {
        // xorshift64* (Vigna) — full period 2^64-1
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }

    /// [0,1) with 53 bits of entropy.
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform integer in [0, n) — rejection-free modulo (n small).
    pub fn next_below(&mut self, n: u64) -> u64 {
        self.next_u64() % n.max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeded_rng_is_reproducible() {
        let mut a = SeededRng::new(42);
        let mut b = SeededRng::new(42);
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        let mut c = SeededRng::new(43);
        assert_ne!(a.next_u64(), c.next_u64());
    }

    #[test]
    fn ulp_close_exact_and_one_ulp() {
        assert!(ulp_close(1.0, 1.0));
        assert!(ulp_close(1.0, 1.0 + f64::EPSILON));
        assert!(!ulp_close(1.0, 2.0));
        assert!(ulp_close(f64::NAN, f64::NAN));
        assert!(ulp_close(0.0, -0.0), "zero signs are equal values");
        // across the sign boundary
        assert!(!ulp_close(-0.0, f64::MIN_POSITIVE));
    }
}
