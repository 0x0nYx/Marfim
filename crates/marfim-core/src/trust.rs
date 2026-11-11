//! Trust levels per directory; untrusted dirs run with reduced powers.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Trust {
    Untrusted,
    Workspace,
    Trusted,
}

impl Trust {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "untrusted" => Some(Trust::Untrusted),
            "workspace" => Some(Trust::Workspace),
            "trusted" => Some(Trust::Trusted),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct TrustStore {
    dirs: BTreeMap<String, Trust>,
}

impl TrustStore {
    pub fn set(&mut self, dir: &str, level: Trust) {
        self.dirs
            .insert(dir.trim_end_matches('/').to_string(), level);
    }

    pub fn get(&self, dir: &str) -> Trust {
        let d = dir.trim_end_matches('/');
        let mut best = Trust::Untrusted;
        let mut best_len = 0usize;
        for (k, v) in &self.dirs {
            if (d == k || d.starts_with(&format!("{k}/"))) && k.len() >= best_len {
                best_len = k.len();
                best = *v;
            }
        }
        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longest_prefix_wins() {
        let mut ts = TrustStore::default();
        ts.set("/home/u/work", Trust::Workspace);
        ts.set("/home/u/work/secret", Trust::Untrusted);
        assert_eq!(ts.get("/home/u/work/src/a.rs"), Trust::Workspace);
        assert_eq!(ts.get("/home/u/work/secret/k"), Trust::Untrusted);
        assert_eq!(ts.get("/elsewhere"), Trust::Untrusted);
    }
}
