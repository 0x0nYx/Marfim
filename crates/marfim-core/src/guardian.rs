//! Guardian: rule engine over the event stream. Denials are capped
//! (`maxDenials`), actions: warn | block | kill-switch. Plus the
//! evidence ledger (append-only claim records) and the message board
//! (cross-agent notes).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GuardianRule {
    pub pattern: String,
    pub action: String, // "warn" | "block"
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct Guardian {
    pub rules: Vec<GuardianRule>,
    pub max_denials: u32,
    denials: std::cell::Cell<u32>,
}

impl Guardian {
    pub fn new(rules: Vec<GuardianRule>, max_denials: u32) -> Self {
        Self {
            rules,
            max_denials,
            denials: std::cell::Cell::new(0),
        }
    }

    /// Inspect a tool call; returns Some(reason) if blocked.
    pub fn inspect(&self, tool: &str, args_json: &str) -> Option<String> {
        for r in &self.rules {
            if let Ok(re) = regex::Regex::new(&r.pattern) {
                let hay = format!("{tool} {args_json}");
                if re.is_match(&hay) && r.action == "block" {
                    let n = self.denials.get() + 1;
                    self.denials.set(n);
                    return Some(format!(
                        "guardian: {} ({}/{})",
                        r.reason, n, self.max_denials
                    ));
                }
            }
        }
        None
    }

    pub fn denial_count(&self) -> u32 {
        self.denials.get()
    }

    /// Kill-switch: exceeded max denials → all further execution blocked.
    pub fn tripped(&self) -> bool {
        self.denials.get() >= self.max_denials
    }
}

/// ------------------------------------------------------ Evidence ledger
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Evidence {
    pub id: u64,
    pub claim: String,
    pub citation: String,
    pub source_tool: String,
    pub at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EvidenceLedger {
    pub records: Vec<Evidence>,
}

impl EvidenceLedger {
    pub fn add(&mut self, claim: &str, citation: &str, source_tool: &str) -> u64 {
        let id = self.records.len() as u64 + 1;
        self.records.push(Evidence {
            id,
            claim: claim.into(),
            citation: citation.into(),
            source_tool: source_tool.into(),
            at: String::new(),
        });
        id
    }

    pub fn verify_claim(&self, claim: &str) -> Option<&Evidence> {
        self.records.iter().find(|e| e.claim == claim)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guardian() -> Guardian {
        Guardian::new(
            vec![GuardianRule {
                pattern: r"(?i)rm\s+-rf\s+/".into(),
                action: "block".into(),
                reason: "destructive root delete".into(),
            }],
            3,
        )
    }

    #[test]
    fn guardian_blocks_matching_and_counts() {
        let g = guardian();
        assert!(g.inspect("Bash", r#"{"command":"rm -rf /"}"#).is_some());
        assert_eq!(g.denial_count(), 1);
        assert!(g.inspect("Read", "{}").is_none());
    }

    #[test]
    fn guardian_trips_kill_switch() {
        let g = guardian();
        for _ in 0..3 {
            let _ = g.inspect("Bash", r#"{"command":"rm -rf /"}"#);
        }
        assert!(g.tripped());
    }

    #[test]
    fn evidence_is_append_only_and_verifiable() {
        let mut e = EvidenceLedger::default();
        let id = e.add("uses rust 2021", "Cargo.toml:2", "Read");
        assert_eq!(id, 1);
        assert!(e.verify_claim("uses rust 2021").is_some());
        assert!(e.verify_claim("nonexistent").is_none());
    }
}
