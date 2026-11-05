//! Router: role → ordered model chain with failover (INV-05). Every role
//! (primary/small/architect/review) is consumed through the router; the
//! real model id lands in the usage ledger; unknown pricing fails closed
//! for budgeted runs.

use crate::providers::{CostBook, Provider};
use crate::{CoreError, Result};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    Primary,
    Small,
    Architect,
    Review,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Primary => "primary",
            Role::Small => "small",
            Role::Architect => "architect",
            Role::Review => "review",
        }
    }
}

#[derive(Clone)]
pub struct ModelEntry {
    pub model: String,
    pub provider: Arc<dyn Provider>,
    /// Cost book carries pricing; `None` here means "pricing unknown".
    pub priced: bool,
}

impl std::fmt::Debug for ModelEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelEntry")
            .field("model", &self.model)
            .field("priced", &self.priced)
            .finish()
    }
}

pub struct Router {
    chains: std::collections::BTreeMap<Role, Vec<ModelEntry>>,
    pub costs: CostBook,
    /// When true, unpriced models fail closed (budget enforcement on).
    pub budgeted: bool,
}

impl Router {
    pub fn new(costs: CostBook, budgeted: bool) -> Self {
        Self {
            chains: Default::default(),
            costs,
            budgeted,
        }
    }

    pub fn set_chain(&mut self, role: Role, entries: Vec<ModelEntry>) {
        self.chains.insert(role, entries);
    }

    pub fn chain(&self, role: Role) -> Vec<ModelEntry> {
        self.chains.get(&role).cloned().unwrap_or_default()
    }

    pub fn roles(&self) -> Vec<Role> {
        self.chains.keys().copied().collect()
    }

    /// Resolve a role to its first usable model, walking the failover
    /// chain. `probe` is called per candidate to decide usability.
    pub fn resolve<F>(&self, role: Role, mut probe: F) -> Result<(ModelEntry, Vec<String>)>
    where
        F: FnMut(&ModelEntry) -> bool,
    {
        let chain = self.chain(role);
        if chain.is_empty() {
            return Err(CoreError::Provider(format!(
                "no models configured for role `{}`",
                role.as_str()
            )));
        }
        let mut tried = Vec::new();
        for entry in &chain {
            if self.budgeted && !entry.priced {
                tried.push(format!("{}: unpriced (fail closed)", entry.model));
                continue;
            }
            if probe(entry) {
                return Ok((
                    ModelEntry {
                        model: entry.model.clone(),
                        provider: entry.provider.clone(),
                        priced: entry.priced,
                    },
                    tried,
                ));
            }
            tried.push(format!("{}:unavailable", entry.model));
        }
        Err(CoreError::Provider(format!(
            "failover exhausted for role `{}`: {}",
            role.as_str(),
            if tried.is_empty() {
                "all candidates fail closed (unpriced under budget)".into()
            } else {
                tried.join(", ")
            }
        )))
    }

    /// Why-this-model explanation surfaced to the user (ALIVE §display).
    pub fn explain(&self, role: Role, chosen: &str, tried: &[String]) -> String {
        if tried.is_empty() {
            format!("{}: `{}` selected (first in chain)", role.as_str(), chosen)
        } else {
            format!(
                "{}: `{}` selected after failover — tried: {}",
                role.as_str(),
                chosen,
                tried.join(", ")
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{MockProvider, Usage};

    fn entry(model: &str, priced: bool) -> ModelEntry {
        ModelEntry {
            model: model.into(),
            provider: Arc::new(MockProvider::new(vec![])),
            priced,
        }
    }

    #[test]
    fn failover_skips_unavailable() {
        let mut r = Router::new(CostBook::default(), true);
        r.set_chain(
            Role::Primary,
            vec![entry("down-model", true), entry("up-model", true)],
        );
        let (m, tried) = r
            .resolve(Role::Primary, |e| e.model != "down-model")
            .unwrap();
        assert_eq!(m.model, "up-model");
        assert_eq!(tried, vec!["down-model:unavailable".to_string()]);
        assert!(r
            .explain(Role::Primary, &m.model, &tried)
            .contains("failover"));
    }

    #[test]
    fn unpriced_fails_closed_when_budgeted() {
        let mut r = Router::new(CostBook::default(), true);
        r.set_chain(Role::Small, vec![entry("mystery-model", false)]);
        let err = r.resolve(Role::Small, |_| true).unwrap_err();
        assert!(format!("{err}").contains("fail closed"), "{err}");
    }

    #[test]
    fn unpriced_allowed_when_not_budgeted() {
        let mut r = Router::new(CostBook::default(), false);
        r.set_chain(Role::Small, vec![entry("mystery-model", false)]);
        let (m, _) = r.resolve(Role::Small, |_| true).unwrap();
        assert_eq!(m.model, "mystery-model");
    }

    #[test]
    fn empty_chain_is_error() {
        let r = Router::new(CostBook::default(), false);
        assert!(r.resolve(Role::Review, |_| true).is_err());
    }

    #[test]
    fn cost_book_integrates_with_usage() {
        let cb = CostBook::default();
        let u = Usage {
            input_tokens: 10_000,
            output_tokens: 2_000,
            ..Default::default()
        };
        let c = cb.cost_usd("gpt-5-mini", &u).unwrap();
        assert!((c - (10_000.0 * 0.25 + 2_000.0 * 2.0) / 1e6).abs() < 1e-12);
    }
}
