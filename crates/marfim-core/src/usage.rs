//! Usage ledger: per-role, real-model accounting (INV-05).

use crate::providers::{CostBook, Usage};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UsageRecord {
    pub role: String,
    pub model: String,
    pub usage: Usage,
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Ledger {
    pub records: Vec<UsageRecord>,
}

impl Ledger {
    pub fn record(&mut self, costs: &CostBook, role: &str, model: &str, usage: &Usage) {
        let cost_usd = costs.cost_usd(model, usage);
        self.records.push(UsageRecord {
            role: role.into(),
            model: model.into(),
            usage: usage.clone(),
            cost_usd,
        });
    }

    pub fn total_cost(&self) -> Option<f64> {
        // fail closed: any unpriced model makes the total unknown
        let mut total = Some(0.0);
        for r in &self.records {
            match (total, r.cost_usd) {
                (Some(a), Some(c)) => total = Some(a + c),
                _ => return None,
            }
        }
        total
    }

    pub fn total_tokens(&self) -> u64 {
        self.records
            .iter()
            .map(|r| r.usage.input_tokens + r.usage.output_tokens)
            .sum()
    }

    pub fn by_role(&self, role: &str) -> Vec<&UsageRecord> {
        self.records.iter().filter(|r| r.role == role).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_real_model_per_role() {
        let cb = CostBook::default();
        let mut l = Ledger::default();
        l.record(
            &cb,
            "primary",
            "claude-sonnet-4-5",
            &Usage {
                input_tokens: 1000,
                output_tokens: 500,
                ..Default::default()
            },
        );
        l.record(
            &cb,
            "small",
            "claude-haiku-4-5",
            &Usage {
                input_tokens: 2000,
                output_tokens: 100,
                ..Default::default()
            },
        );
        assert_eq!(l.by_role("primary")[0].model, "claude-sonnet-4-5");
        assert_eq!(l.by_role("small")[0].model, "claude-haiku-4-5");
        assert!(l.total_cost().is_some());
    }

    #[test]
    fn unknown_pricing_makes_total_unknown_fail_closed() {
        let cb = CostBook::default();
        let mut l = Ledger::default();
        l.record(&cb, "primary", "mystery", &Usage::default());
        assert_eq!(l.total_cost(), None, "unpriced → total unknown");
    }
}
