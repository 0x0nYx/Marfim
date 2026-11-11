//! Port of oracle `src/intel/conservative.js` — the conservative bandit
//! (Wu, Shariff, Lattimore & Szepesvári, ICML 2016) and its Hoeffding radii.
//!
//! Differential-port notes:
//! - `py_round` is inlined from oracle `src/intel/util/pymath.js::pyRound`
//!   (CPython banker's rounding over the EXACT binary double; reimplemented
//!   over u128). Oracle call sites use digit count 4, inside the helper's
//!   exact envelope.
//! - `js_to_fixed` reimplements ECMAScript `Number.prototype.toFixed(4)`
//!   exactly (round-half-up on the double's exact binary value, ties to the
//!   larger integer) for the reachable magnitude envelope; it is used ONLY
//!   in the gate-refusal reason string.
//! - `Math.log` (in the Hoeffding radius) is transcendental: <=1-ulp
//!   tolerance applies (Math.sqrt is IEEE-exact). Validated by the committed
//!   fixtures (`fixtures/intel/conservative.jsonl`).
//! - Arm names iterate in sorted order everywhere output is affected
//!   (`Object.keys(...).sort()`), so a BTreeMap keeps output deterministic.
//! - Narrowing: `choose()` with a `thompson_sample` that is not among the
//!   arms makes the oracle crash (undefined `.alpha` TypeError); the port
//!   falls back deterministically to "no evidence" (floor closed).

use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

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

/// JS number -> string for template interpolation (String(x)): shortest
/// round-trip decimal, integral floats without a trailing ".0". Matches
/// Rust's `{}` Display for every magnitude the oracle interpolates here
/// (>= 1e-6 and < 1e21).
fn js_num(x: f64) -> String {
    format!("{x}")
}

/// ECMAScript `Number.prototype.toFixed(f)` — the spec negates negative x
/// up front (sign always prepended), then picks the integer n minimizing
/// |n/10^f - x| over the double's EXACT value, ties resolved to the LARGER
/// n (half-up on the magnitude), and inserts a decimal point f digits from
/// the right. Exact for |x| < 2^70 with a fractional binary exponent <= 127;
/// outside that envelope (unreachable at this module's call sites, whose
/// bounds are O(1)) it falls back to Rust's `{:.f}` formatting.
fn js_to_fixed(x: f64, f: u32) -> String {
    if !x.is_finite() {
        return if x.is_nan() {
            "NaN".to_string()
        } else if x > 0.0 {
            "Infinity".to_string()
        } else {
            "-Infinity".to_string()
        };
    }
    let neg = x < 0.0; // -0.0 is NOT negative (x < 0 is false), matching the spec
    let ax = x.abs();
    let bits = ax.to_bits();
    let biased = ((bits >> 52) & 0x7ff) as i64;
    let raw = bits & 0x000f_ffff_ffff_ffff;
    let (mant, e): (u128, i64) = if biased == 0 {
        (u128::from(raw), -1074)
    } else {
        (u128::from(raw | 0x0010_0000_0000_0000), biased - 1075)
    };
    if f > 12 || !(-127..0).contains(&e) {
        // |x| >= 2^52 (integral, no rounding needed) or far below the
        // reachable envelope: Rust's correctly-rounded formatting applies
        // (ax, not x: -0.0 must format without the sign, like JS)
        let shown = if neg { x } else { ax };
        return format!("{:.*}", f as usize, shown);
    }
    // scaled = ax * 10^f = mant * 10^f / 2^(-e), computed exactly
    let num = mant * 10u128.pow(f);
    let den = 1u128 << (-e);
    let mut q = num / den;
    let r = num % den;
    if r * 2 >= den {
        // tie -> the LARGER integer n (half-up on the magnitude)
        q += 1;
    }
    let sign = if neg { "-" } else { "" };
    if f == 0 {
        return format!("{sign}{q}");
    }
    let s = q.to_string();
    let padded = format!("{s:0>width$}", width = f as usize + 1);
    let cut = padded.len() - f as usize;
    format!("{sign}{}.{}", &padded[..cut], &padded[cut..])
}

/// The Hoeffding radius core (n > 0, delta in (0, 1) assumed — the public
/// wrapper validates). Math.log is transcendental: <=1-ulp tolerance.
fn hoeffding_radius_raw(n: i64, delta: f64) -> f64 {
    ((2.0 / delta).ln() / (2.0 * n as f64)).sqrt()
}

/// The Hoeffding radius sqrt(ln(2/delta)/(2n)); refuses at n = 0 ("no
/// evidence" is the honest answer, not a small radius) and outside-delta.
pub fn hoeffding_radius(n: i64, delta: f64) -> Result<f64, String> {
    if n <= 0 {
        return Err(
            "radius is undefined at n = 0 — the honest answer is 'no evidence', not a small radius"
                .to_string(),
        );
    }
    if !(0.0 < delta && delta < 1.0) {
        return Err("delta must be inside (0, 1)".to_string());
    }
    Ok(hoeffding_radius_raw(n, delta))
}

/// Conservative bandit — a SAFE BASELINE arm plus the paper's gate: a
/// non-baseline arm is explored only when its UCB clears the safe arm's LCB
/// minus the budget epsilon. Sits ON TOP of the Beta posteriors (n =
/// alpha + beta - prior_mass); when the gate refuses, the SAFE arm plays.
#[derive(Debug, Clone)]
pub struct ConservativeBandit {
    arms: BTreeMap<String, [f64; 2]>, // name -> [alpha, beta]
    safe_arm: String,
    epsilon: f64,
    delta: f64,
    prior_mass: f64,
}

impl ConservativeBandit {
    /// `new ConservativeBandit({arms, safeArm, epsilon, delta, priorMass})`
    /// where `arms` is the JSON object {name: {alpha, beta}}. Errors carry
    /// the oracle's exact exception messages.
    pub fn new(
        arms: &Value,
        safe_arm: &str,
        epsilon: f64,
        delta: f64,
        prior_mass: f64,
    ) -> Result<Self, String> {
        let mut parsed: BTreeMap<String, [f64; 2]> = BTreeMap::new();
        if let Some(map) = arms.as_object() {
            for (name, post) in map {
                let alpha = post
                    .get("alpha")
                    .and_then(Value::as_f64)
                    .unwrap_or(f64::NAN);
                let beta = post.get("beta").and_then(Value::as_f64).unwrap_or(f64::NAN);
                parsed.insert(name.clone(), [alpha, beta]);
            }
        }
        if !parsed.contains_key(safe_arm) {
            let names: Vec<String> = parsed.keys().cloned().collect();
            return Err(format!(
                "safe arm {safe_arm:?} not among the arms [{}]",
                names.join(", ")
            ));
        }
        if epsilon < 0.0 {
            return Err(
                "epsilon (the safety budget) must be >= 0 — a negative budget is not a budget"
                    .to_string(),
            );
        }
        if !(0.0 < delta && delta < 1.0) {
            return Err("delta must be inside (0, 1)".to_string());
        }
        Ok(Self {
            arms: parsed,
            safe_arm: safe_arm.to_string(),
            epsilon,
            delta,
            prior_mass,
        })
    }

    fn mean_of(&self, name: &str) -> Option<f64> {
        self.arms.get(name).map(|p| p[0] / (p[0] + p[1]))
    }

    fn plays_of(&self, name: &str) -> i64 {
        // the posterior's own prior mass is not evidence: n = mass - prior
        self.arms
            .get(name)
            .map_or(0, |p| (p[0] + p[1] - self.prior_mass).trunc() as i64)
            .max(0)
    }

    fn lcb_of(&self, name: &str) -> Option<f64> {
        let n = self.plays_of(name);
        if n == 0 {
            return None; // no evidence: no lower bound is claimed
        }
        self.mean_of(name)
            .map(|m| m - hoeffding_radius_raw(n, self.delta))
    }

    fn ucb_of(&self, name: &str) -> Option<f64> {
        let n = self.plays_of(name);
        if n == 0 {
            return None;
        }
        self.mean_of(name)
            .map(|m| m + hoeffding_radius_raw(n, self.delta))
    }

    /// Apply the gate to one decision. `thompson_sample`: the arm the
    /// underlying Thompson policy already picked (the caller samples; this
    /// module gates). Returns {arm, overridden, gated_sample, details}.
    pub fn choose(&self, thompson_sample: Option<&str>) -> Value {
        let safe_lcb = self.lcb_of(&self.safe_arm);
        let mut details = json!({
            "epsilon": self.epsilon,
            "delta": self.delta,
            "safe_arm": self.safe_arm,
            "safe_lcb": safe_lcb,
        });
        let insert_reason = |details: &mut Value, reason: &str| {
            if let Some(obj) = details.as_object_mut() {
                obj.insert("reason".to_string(), json!(reason));
            }
        };
        let Some(safe_lcb_val) = safe_lcb else {
            // the safe arm itself has no evidence: no floor exists yet —
            // play the safe arm and say so
            insert_reason(&mut details, "safe arm has no evidence — floor closed");
            return json!({
                "arm": self.safe_arm,
                "overridden": true,
                "gated_sample": thompson_sample,
                "details": details,
            });
        };
        match thompson_sample {
            None => json!({
                "arm": self.safe_arm,
                "overridden": false,
                "gated_sample": Value::Null,
                "details": details,
            }),
            Some(ts) if ts == self.safe_arm => json!({
                "arm": self.safe_arm,
                "overridden": false,
                "gated_sample": ts,
                "details": details,
            }),
            Some(ts) => {
                let ucb = self.ucb_of(ts);
                if let Some(obj) = details.as_object_mut() {
                    obj.insert("sample_ucb".to_string(), json!(ucb));
                }
                let Some(ucb_val) = ucb else {
                    // an unplayed arm: no evidence to clear the gate with —
                    // the honest reading is "not yet"
                    insert_reason(&mut details, "sampled arm has no evidence — floor closed");
                    return json!({
                        "arm": self.safe_arm,
                        "overridden": true,
                        "gated_sample": ts,
                        "details": details,
                    });
                };
                if ucb_val > safe_lcb_val - self.epsilon {
                    insert_reason(&mut details, "sample cleared the gate");
                    json!({
                        "arm": ts,
                        "overridden": false,
                        "gated_sample": ts,
                        "details": details,
                    })
                } else {
                    insert_reason(
                        &mut details,
                        &format!(
                            "UCB {} <= safe LCB {} - ε {} — floor closed",
                            js_to_fixed(ucb_val, 4),
                            js_to_fixed(safe_lcb_val, 4),
                            js_num(self.epsilon),
                        ),
                    );
                    json!({
                        "arm": self.safe_arm,
                        "overridden": true,
                        "gated_sample": ts,
                        "details": details,
                    })
                }
            }
        }
    }

    /// The guarantee, stated exactly as the paper gives it — with its scope.
    pub fn guarantee(&self) -> String {
        format!(
            "Wu et al. 2016, Theorem 1 (this implementation's form): with probability >= {}, the total regret against always playing {:?} is at most {} * T (+ constants) — exploration is no worse than the baseline by more than ε per round, in expectation. This bounds DISTANCE FROM THE BASELINE, not regret against the best arm.",
            js_num(1.0 - self.delta),
            self.safe_arm,
            js_num(self.epsilon),
        )
    }

    /// The audit card: per-arm evidence, bounds, and the gate state. An
    /// untouched arm reports n = 0 and unclaimed bounds — an abstention.
    pub fn report(&self) -> Value {
        let rows: Vec<Value> = self
            .arms
            .keys()
            .map(|name| {
                let n = self.plays_of(name);
                let mean = if n != 0 {
                    self.mean_of(name).map(|m| py_round(m, 4))
                } else {
                    None
                };
                let lcb = self.lcb_of(name).map(|v| py_round(v, 4));
                let ucb = self.ucb_of(name).map(|v| py_round(v, 4));
                json!({
                    "arm": name,
                    "plays": n,
                    "mean": mean,
                    "lcb": lcb,
                    "ucb": ucb,
                    "safe": name == &self.safe_arm,
                })
            })
            .collect();
        json!({
            "arms": rows,
            "epsilon": self.epsilon,
            "delta": self.delta,
            "guarantee": self.guarantee(),
        })
    }

    pub fn to_json(&self) -> Value {
        let mut arms = Map::new();
        for (name, post) in &self.arms {
            arms.insert(name.clone(), json!({"alpha": post[0], "beta": post[1]}));
        }
        json!({
            "arms": Value::Object(arms),
            "safe_arm": self.safe_arm,
            "epsilon": self.epsilon,
            "delta": self.delta,
            "prior_mass": self.prior_mass,
        })
    }

    /// The oracle's `static fromDict(d)` — revalidates through the
    /// constructor, so `Err` is possible exactly when the constructor errs.
    pub fn from_json(d: &Value) -> Result<Self, String> {
        let empty = json!({});
        let arms = d.get("arms").unwrap_or(&empty);
        let safe_arm = d
            .get("safe_arm")
            .and_then(Value::as_str)
            .unwrap_or("balanced");
        let epsilon = d.get("epsilon").and_then(Value::as_f64).unwrap_or(0.05);
        let delta = d.get("delta").and_then(Value::as_f64).unwrap_or(0.05);
        let prior_mass = d.get("prior_mass").and_then(Value::as_f64).unwrap_or(2.0);
        Self::new(arms, safe_arm, epsilon, delta, prior_mass)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::ulp_close;

    #[test]
    fn radius_refuses_no_evidence_and_bad_delta() {
        assert_eq!(
            hoeffding_radius(0, 0.05).unwrap_err(),
            "radius is undefined at n = 0 — the honest answer is 'no evidence', not a small radius"
        );
        assert_eq!(
            hoeffding_radius(10, 0.0).unwrap_err(),
            "delta must be inside (0, 1)"
        );
        assert!(ulp_close(
            hoeffding_radius(2, 0.05).unwrap_or_else(|e| panic!("{e}")),
            0.960_322_791_319_920_7
        ));
    }

    #[test]
    fn flat_arms_close_the_floor() {
        let arms =
            json!({"balanced": {"alpha": 2.0, "beta": 0.0}, "fast": {"alpha": 1.0, "beta": 1.0}});
        let b = ConservativeBandit::new(&arms, "balanced", 0.05, 0.05, 2.0)
            .unwrap_or_else(|e| panic!("{e}"));
        let choice = b.choose(Some("fast"));
        assert_eq!(choice["arm"], "balanced");
        assert_eq!(
            choice["details"]["reason"],
            "safe arm has no evidence — floor closed"
        );
    }

    #[test]
    fn unplayed_sampled_arm_is_gated() {
        let arms = json!({
            "balanced": {"alpha": 8.0, "beta": 2.0},
            "fast": {"alpha": 3.0, "beta": 3.0},
            "cheap": {"alpha": 0.5, "beta": 0.5},
        });
        let b = ConservativeBandit::new(&arms, "balanced", 0.05, 0.05, 2.0)
            .unwrap_or_else(|e| panic!("{e}"));
        let choice = b.choose(Some("cheap")); // 0.5+0.5-2 < 0 plays
        assert_eq!(choice["overridden"], true);
        assert_eq!(choice["details"]["sample_ucb"], Value::Null);
        assert_eq!(
            choice["details"]["reason"],
            "sampled arm has no evidence — floor closed"
        );
    }

    #[test]
    fn report_sorts_arms_and_audits() {
        let arms = json!({
            "balanced": {"alpha": 6.0, "beta": 2.0},
            "fast": {"alpha": 3.0, "beta": 3.0},
        });
        let b = ConservativeBandit::new(&arms, "fast", 0.05, 0.05, 2.0)
            .unwrap_or_else(|e| panic!("{e}"));
        let rep = b.report();
        let rows = rep["arms"].as_array().unwrap_or(&Vec::new()).clone();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["arm"], "balanced");
        assert_eq!(rows[1]["arm"], "fast");
        assert_eq!(rows[1]["safe"], true);
        assert!(rep["guarantee"]
            .as_str()
            .unwrap_or("")
            .contains("Theorem 1"));
    }

    #[test]
    fn ctor_errors_are_exact() {
        let arms = json!({"balanced": {"alpha": 4.0, "beta": 2.0}});
        assert_eq!(
            ConservativeBandit::new(&arms, "ghost", 0.05, 0.05, 2.0).unwrap_err(),
            "safe arm \"ghost\" not among the arms [balanced]"
        );
        assert_eq!(
            ConservativeBandit::new(&arms, "balanced", -1.0, 0.05, 2.0).unwrap_err(),
            "epsilon (the safety budget) must be >= 0 — a negative budget is not a budget"
        );
    }

    #[test]
    fn js_to_fixed_matches_ecmascript() {
        assert_eq!(js_to_fixed(0.468_726_046_076_560_77, 4), "0.4687");
        assert_eq!(js_to_fixed(0.827_617_312_272_670_5, 4), "0.8276");
        assert_eq!(js_to_fixed(0.0, 4), "0.0000");
        assert_eq!(js_to_fixed(-0.5, 0), "-1");
        assert_eq!(js_to_fixed(-0.3, 0), "-0");
        assert_eq!(js_to_fixed(0.5, 0), "1");
        assert_eq!(js_to_fixed(2.5, 0), "3");
        assert_eq!(js_to_fixed(-2.5, 0), "-3");
        assert_eq!(js_to_fixed(1.25, 1), "1.3");
        assert_eq!(js_to_fixed(1.005, 2), "1.00");
        assert_eq!(js_to_fixed(0.35, 1), "0.3");
        assert_eq!(js_to_fixed(-0.000_04, 4), "-0.0000");
        assert_eq!(js_to_fixed(-0.0, 4), "0.0000");
        assert_eq!(js_to_fixed(8.0, 4), "8.0000");
    }
}
