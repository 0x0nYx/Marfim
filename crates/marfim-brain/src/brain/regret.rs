//! Port of oracle `src/intel/brain/regret.js` — the periodic
//! regret-vs-best-fixed audit for every Beta bandit.
//!
//! The baseline is the BEST-FIXED-ARM-IN-HINDSIGHT: the arm with the best
//! observed mean reward, multiplied across the whole horizon. Two input
//! shapes, same audit: aggregate Beta posteriors (`audit_from_arms`) or an
//! explicit per-round draw log (`audit_from_draws`). Honesty accounting is
//! part of the contract: an untouched bandit is an abstention, not a zero;
//! fewer than two played arms has no policy comparison; the regret figure
//! inherits the best-fixed ESTIMATE caveat and is never described as a bound.
//!
//! NARROWINGS (per the P4-B differential method):
//! - `pyRound` is the shared inlined copy at
//!   [`crate::brain::pooling::py_round`].
//! - `audit_from_draws` takes `(arm, reward)` pairs with the reward already
//!   normalised to `f64`: the oracle counts `reward === 1 || reward === true`
//!   as a win and everything else (0, 0.5, `"1"`, false, null) as a loss —
//!   callers map JSON booleans to 1.0/0.0 and leave other scalars alone, then
//!   `reward == 1.0` decides the win exactly like the strict equalities.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use crate::brain::pooling::{jnum, py_round};

/// The flat prior mass (oracle `FLAT_PRIOR`).
pub const FLAT_PRIOR: f64 = 1.0;

/// Oracle `METHOD` — verbatim.
pub const METHOD: &str = "regret vs best-fixed-arm-in-hindsight; the best arm's \
observed mean is assumed to hold over the whole horizon — an estimate, not a bound";

/// One arm's evidence row (oracle `per_arm` entries).
#[derive(Debug, Clone)]
pub struct ArmStat {
    pub arm: String,
    pub plays: f64,
    pub reward_sum: f64,
    /// `None` exactly when `plays == 0` (oracle `null`).
    pub mean_reward: Option<f64>,
}

impl ArmStat {
    fn to_json(&self) -> Value {
        json!({
            "arm": self.arm,
            "plays": jnum(self.plays),
            "reward_sum": jnum(self.reward_sum),
            "mean_reward": match self.mean_reward {
                Some(m) => jnum(m),
                None => Value::Null,
            },
        })
    }
}

/// The audit report. Key presence mirrors the oracle exactly: `abstained`
/// carries no best-fixed/regret fields; `single-arm` carries
/// `best_fixed_arm` + `estimated_regret`; `compared` adds
/// `best_fixed_estimate`.
#[derive(Debug, Clone)]
pub struct RegretAudit {
    pub per_arm: Vec<ArmStat>,
    pub horizon: f64,
    pub cumulative_reward: f64,
    pub method: &'static str,
    pub status: &'static str,
    pub note: String,
    pub best_fixed_arm: Option<String>,
    pub best_fixed_estimate: Option<f64>,
    pub estimated_regret: Option<f64>,
}

impl RegretAudit {
    pub fn to_json(&self) -> Value {
        let mut obj = Map::new();
        obj.insert(
            "per_arm".into(),
            json!(self
                .per_arm
                .iter()
                .map(ArmStat::to_json)
                .collect::<Vec<_>>()),
        );
        obj.insert("horizon".into(), jnum(self.horizon));
        obj.insert("cumulative_reward".into(), jnum(self.cumulative_reward));
        obj.insert("method".into(), json!(self.method));
        obj.insert("status".into(), json!(self.status));
        match self.status {
            "abstained" => {}
            "single-arm" => {
                obj.insert(
                    "best_fixed_arm".into(),
                    match &self.best_fixed_arm {
                        Some(a) => json!(a),
                        None => Value::Null,
                    },
                );
                obj.insert(
                    "estimated_regret".into(),
                    match self.estimated_regret {
                        Some(r) => jnum(r),
                        None => Value::Null,
                    },
                );
            }
            _ => {
                obj.insert(
                    "best_fixed_arm".into(),
                    match &self.best_fixed_arm {
                        Some(a) => json!(a),
                        None => Value::Null,
                    },
                );
                obj.insert(
                    "best_fixed_estimate".into(),
                    match self.best_fixed_estimate {
                        Some(e) => jnum(e),
                        None => Value::Null,
                    },
                );
                obj.insert(
                    "estimated_regret".into(),
                    match self.estimated_regret {
                        Some(r) => jnum(r),
                        None => Value::Null,
                    },
                );
            }
        }
        obj.insert("note".into(), json!(self.note));
        Value::Object(obj)
    }
}

/// Shared tail: totals, best-fixed-in-hindsight estimate, regret (oracle
/// `audit`).
fn audit(per_arm: Vec<ArmStat>) -> RegretAudit {
    let horizon = per_arm.iter().map(|x| x.plays).sum::<f64>();
    let cumulative = per_arm.iter().map(|x| x.reward_sum).sum::<f64>();
    let played_idx: Vec<usize> = per_arm
        .iter()
        .enumerate()
        .filter(|(_, a)| a.plays > 0.0)
        .map(|(i, _)| i)
        .collect();
    let base = RegretAudit {
        per_arm,
        horizon,
        cumulative_reward: py_round(cumulative, 4),
        method: METHOD,
        status: "abstained",
        note: String::new(),
        best_fixed_arm: None,
        best_fixed_estimate: None,
        estimated_regret: None,
    };
    if horizon == 0.0 {
        return RegretAudit {
            status: "abstained",
            note: "no plays recorded — nothing to compare yet".to_string(),
            ..base
        };
    }
    if played_idx.len() < 2 {
        return RegretAudit {
            status: "single-arm",
            best_fixed_arm: played_idx.first().map(|&i| base.per_arm[i].arm.clone()),
            estimated_regret: Some(0.0),
            note: "only one arm was ever played — no policy \
                   comparison exists; regret is 0 by definition"
                .to_string(),
            ..base
        };
    }
    // max by (mean_reward, arm): strictly-better mean wins; ties go to the
    // LARGER arm name (the oracle's comparison, verbatim)
    let mut best = played_idx[0];
    for &i in &played_idx[1..] {
        let a = &base.per_arm[i];
        let b = &base.per_arm[best];
        let a_mean = a.mean_reward.unwrap_or(f64::NEG_INFINITY);
        let b_mean = b.mean_reward.unwrap_or(f64::NEG_INFINITY);
        if a_mean > b_mean || (a_mean == b_mean && a.arm > b.arm) {
            best = i;
        }
    }
    let best_mean = base.per_arm[best].mean_reward.unwrap_or(f64::NAN);
    let best_fixed_total = best_mean * horizon;
    let regret = best_fixed_total - cumulative;
    let best_arm = base.per_arm[best].arm.clone();
    RegretAudit {
        status: "compared",
        best_fixed_arm: Some(best_arm),
        best_fixed_estimate: Some(py_round(best_fixed_total, 4)),
        estimated_regret: Some(py_round(regret, 4)),
        note: format!(
            "positive estimated regret = the bandit trailed always-\
             -playing-{}; an estimate over unobserved rounds, printed for the \
             human — nothing acts on it",
            base.per_arm[best].arm
        ),
        ..base
    }
}

/// The audit from aggregate Beta posteriors `{name: [alpha, beta]}`. Evidence
/// only: plays = (alpha - prior) + (beta - prior), reward sum = alpha - prior
/// (the flat prior's own mass is never counted). Arms are visited in
/// sorted-name order (BTreeMap iteration == oracle `Object.keys().sort()`).
pub fn audit_from_arms(arms: &BTreeMap<String, [f64; 2]>, prior: f64) -> RegretAudit {
    let mut per_arm: Vec<ArmStat> = Vec::new();
    for (name, pair) in arms {
        let (alpha, beta) = (pair[0], pair[1]);
        let s = (alpha - prior).max(0.0);
        let f = (beta - prior).max(0.0);
        let n = s + f;
        per_arm.push(ArmStat {
            arm: name.clone(),
            plays: n,
            reward_sum: s,
            mean_reward: if n != 0.0 {
                Some(py_round(s / n, 6))
            } else {
                None
            },
        });
    }
    audit(per_arm)
}

/// The audit from an explicit per-round draw log `[[arm, reward], ...]`.
/// Rewards arrive pre-normalised to f64 (see the module doc narrowing):
/// exactly `1.0` counts as a win, everything else a loss.
pub fn audit_from_draws(draws: &[(String, f64)]) -> RegretAudit {
    // per-arm accumulators; BTreeMap iteration is the sorted order the
    // oracle's `[...stats.keys()].sort()` produces
    let mut stats: BTreeMap<String, (f64, f64)> = BTreeMap::new(); // arm -> (plays, reward_sum)
    for (arm, reward) in draws {
        let row = stats.entry(arm.clone()).or_insert((0.0, 0.0));
        row.0 += 1.0;
        row.1 += if *reward == 1.0 { 1.0 } else { 0.0 };
    }
    let per_arm: Vec<ArmStat> = stats
        .iter()
        .map(|(name, (plays, reward_sum))| ArmStat {
            arm: name.clone(),
            plays: *plays,
            reward_sum: *reward_sum,
            mean_reward: if *plays != 0.0 {
                Some(py_round(reward_sum / plays, 6))
            } else {
                None
            },
        })
        .collect();
    audit(per_arm)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_draws_abstain() {
        let audit = audit_from_draws(&[]);
        assert_eq!(audit.status, "abstained");
        assert_eq!(audit.note, "no plays recorded — nothing to compare yet");
        assert!(audit.best_fixed_arm.is_none());
        assert!(audit.estimated_regret.is_none());
    }

    #[test]
    fn single_arm_has_zero_regret_by_definition() {
        let audit = audit_from_draws(&[("solo".into(), 1.0), ("solo".into(), 0.0)]);
        assert_eq!(audit.status, "single-arm");
        assert_eq!(audit.best_fixed_arm.as_deref(), Some("solo"));
        assert_eq!(audit.estimated_regret, Some(0.0));
    }

    #[test]
    fn ties_on_mean_go_to_larger_arm_name() {
        // both arms mean 0.5: the oracle keeps the LARGER name
        let mut arms = BTreeMap::new();
        arms.insert("zeta".to_string(), [3.0, 1.0]);
        arms.insert("alpha".to_string(), [2.0, 0.0]);
        let audit = audit_from_arms(&arms, 1.0);
        assert_eq!(audit.status, "compared");
        assert_eq!(audit.best_fixed_arm.as_deref(), Some("zeta"));
    }

    #[test]
    fn evidence_excludes_the_prior_mass() {
        let mut arms = BTreeMap::new();
        arms.insert("a".to_string(), [5.0, 3.0]);
        let audit = audit_from_arms(&arms, 1.0);
        assert_eq!(audit.horizon, 6.0);
        assert_eq!(audit.cumulative_reward, 4.0);
        let arm = &audit.per_arm[0];
        assert_eq!(arm.plays, 6.0);
        assert_eq!(arm.reward_sum, 4.0);
    }

    #[test]
    fn method_note_is_verbatim() {
        assert!(METHOD.starts_with("regret vs best-fixed-arm-in-hindsight;"));
        assert!(METHOD.ends_with("an estimate, not a bound"));
        let audit = audit_from_draws(&[]);
        assert_eq!(audit.method, METHOD);
    }
}
