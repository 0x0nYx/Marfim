//! W12 evidence pipeline (the REA layer, distilled from the REA
//! contracts — clean-room): three ledgers that turn "the agent says it
//! is done" into "the evidence says it is done":
//!
//! - **Evidence records**: every tool result can become a canonical
//!   [`EvidenceRecord`] with confidence/coverage and a digest — the
//!   retained view names the record, the record names the source.
//! - **Obligation ledger**: reconstruction/verification checklists as a
//!   definition-of-done gate — a refactor is NOT done while an
//!   obligation is open (`gate()` refuses, naming what is missing).
//! - **Unknowns ledger**: explicit `record_unknown` /
//!   `verify_unknown_resolution` — an investigation may not conclude
//!   with a silent unknown; each one is tracked to resolution or it
//!   blocks the gate too.
//!
//! All three are pure state (no threads, no wall clock in decisions —
//! sequence numbers order everything), so the session loop, a tool, and
//! the gateway can share one gate.

use std::collections::BTreeMap;

/// Digest helper: FNV-1a 64, hex — a fingerprint for dedupe/display, not
/// a security primitive (the same doctrine as the hashline tags).
pub fn digest(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    format!("{h:016x}")
}

/// A canonical evidence object (REA contracts).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceRecord {
    pub id: u64,
    pub tool: String,
    pub args_digest: String,
    pub output_digest: String,
    /// How much of the question this evidence answers: `Full` beats
    /// `Partial` beats `Indirect`.
    pub confidence: Confidence,
    /// What the evidence covers, as a short selector (e.g. "src/lib.rs",
    /// "cargo test", "git diff").
    pub coverage: String,
    /// The turn the evidence was produced in (sequence, not wall clock).
    pub turn: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Confidence {
    Indirect,
    Partial,
    Full,
}

impl Confidence {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "full" => Some(Self::Full),
            "partial" => Some(Self::Partial),
            "indirect" => Some(Self::Indirect),
            _ => None,
        }
    }
    pub fn name(&self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Partial => "partial",
            Self::Indirect => "indirect",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Obligation {
    pub id: u64,
    pub kind: ObligationKind,
    pub description: String,
    pub satisfied_by: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObligationKind {
    /// The behavior/structure must be reconstructed and shown.
    Reconstruct,
    /// A check must run and pass (the definition-of-done half).
    Verify,
}

impl ObligationKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "reconstruct" => Some(Self::Reconstruct),
            "verify" => Some(Self::Verify),
            _ => None,
        }
    }
    pub fn name(&self) -> &'static str {
        match self {
            Self::Reconstruct => "reconstruct",
            Self::Verify => "verify",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unknown {
    pub id: u64,
    pub question: String,
    pub resolved_by: Option<u64>,
}

#[derive(Debug, Default)]
pub struct ReaLedger {
    next: u64,
    evidence: BTreeMap<u64, EvidenceRecord>,
    obligations: BTreeMap<u64, Obligation>,
    unknowns: BTreeMap<u64, Unknown>,
}

impl ReaLedger {
    pub fn new() -> Self {
        Self::default()
    }

    fn bump(&mut self) -> u64 {
        self.next += 1;
        self.next
    }

    // -- evidence ---------------------------------------------------------

    /// Every tool result becomes a canonical record (the caller passes
    /// confidence/coverage; the digests are derived here so the record
    /// cannot lie about its inputs).
    pub fn record(
        &mut self,
        tool: &str,
        args: &str,
        output: &str,
        confidence: Confidence,
        coverage: &str,
        turn: u64,
    ) -> u64 {
        let id = self.bump();
        self.evidence.insert(
            id,
            EvidenceRecord {
                id,
                tool: tool.to_string(),
                args_digest: digest(args),
                output_digest: digest(output),
                confidence,
                coverage: coverage.to_string(),
                turn,
            },
        );
        // Bounded: the newest 512 records stay; older ones age out (the
        // session journal remains the durable record — this is the hot view).
        while self.evidence.len() > 512 {
            let oldest = *self.evidence.keys().next().unwrap_or(&0);
            self.evidence.remove(&oldest);
        }
        id
    }

    /// The retained-view pager: records newest-last (id order), bounded.
    pub fn page(&self, offset: usize, limit: usize) -> Vec<&EvidenceRecord> {
        self.evidence.values().skip(offset).take(limit).collect()
    }

    pub fn evidence(&self, id: u64) -> Option<&EvidenceRecord> {
        self.evidence.get(&id)
    }

    // -- obligations ------------------------------------------------------

    pub fn obligate(&mut self, kind: ObligationKind, description: &str) -> u64 {
        let id = self.bump();
        self.obligations.insert(
            id,
            Obligation {
                id,
                kind,
                description: description.to_string(),
                satisfied_by: None,
            },
        );
        id
    }

    /// Satisfy an obligation with an evidence record. `Verify`
    /// obligations demand at least Partial confidence; `Reconstruct`
    /// demands a coverage reference — a weaker claim is REFUSED (the
    /// gate would be theater otherwise).
    pub fn satisfy(&mut self, obligation_id: u64, evidence_id: u64) -> Result<(), String> {
        let Some(e) = self.evidence.get(&evidence_id) else {
            return Err(format!("no evidence record {evidence_id}"));
        };
        let Some(o) = self.obligations.get_mut(&obligation_id) else {
            return Err(format!("no obligation {obligation_id}"));
        };
        if o.satisfied_by.is_some() {
            return Err(format!("obligation {obligation_id} is already satisfied"));
        }
        match o.kind {
            ObligationKind::Verify if e.confidence == Confidence::Indirect => {
                return Err(format!(
                    "evidence {evidence_id} is indirect — verify obligations need at least partial confidence"
                ));
            }
            ObligationKind::Reconstruct if e.confidence == Confidence::Indirect => {
                return Err(format!(
                    "evidence {evidence_id} is indirect — reconstruct obligations need the thing shown, not a mention"
                ));
            }
            ObligationKind::Reconstruct if e.coverage.is_empty() => {
                return Err("reconstruct obligations need a coverage reference".into());
            }
            _ => {}
        }
        o.satisfied_by = Some(evidence_id);
        Ok(())
    }

    // -- unknowns ---------------------------------------------------------

    pub fn record_unknown(&mut self, question: &str) -> u64 {
        let id = self.bump();
        self.unknowns.insert(
            id,
            Unknown {
                id,
                question: question.to_string(),
                resolved_by: None,
            },
        );
        id
    }

    pub fn verify_unknown_resolution(
        &mut self,
        unknown_id: u64,
        evidence_id: u64,
    ) -> Result<(), String> {
        if !self.evidence.contains_key(&evidence_id) {
            return Err(format!("no evidence record {evidence_id}"));
        }
        let Some(u) = self.unknowns.get_mut(&unknown_id) else {
            return Err(format!("no unknown {unknown_id}"));
        };
        if u.resolved_by.is_some() {
            return Err(format!("unknown {unknown_id} is already resolved"));
        }
        u.resolved_by = Some(evidence_id);
        Ok(())
    }

    // -- the gate ---------------------------------------------------------

    /// The definition-of-done gate: `Ok(())` only when every obligation
    /// is satisfied and every unknown is resolved. The Err lists EACH
    /// blocker (the agent gets an actionable checklist, not a shrug).
    pub fn gate(&self) -> Result<(), Vec<String>> {
        let mut blockers = Vec::new();
        for o in self.obligations.values() {
            if o.satisfied_by.is_none() {
                blockers.push(format!(
                    "obligation {} ({}) open: {}",
                    o.id,
                    o.kind.name(),
                    o.description
                ));
            }
        }
        for u in self.unknowns.values() {
            if u.resolved_by.is_none() {
                blockers.push(format!("unknown {} open: {}", u.id, u.question));
            }
        }
        if blockers.is_empty() {
            Ok(())
        } else {
            Err(blockers)
        }
    }

    pub fn open_count(&self) -> usize {
        self.obligations
            .values()
            .filter(|o| o.satisfied_by.is_none())
            .count()
            + self
                .unknowns
                .values()
                .filter(|u| u.resolved_by.is_none())
                .count()
    }

    pub fn evidence_count(&self) -> usize {
        self.evidence.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_records_derive_their_own_digests() {
        let mut l = ReaLedger::new();
        let id = l.record(
            "Read",
            r#"{"path":"src/lib.rs"}"#,
            "contents…",
            Confidence::Full,
            "src/lib.rs",
            3,
        );
        let e = l.evidence(id).unwrap();
        assert_eq!(e.args_digest, digest(r#"{"path":"src/lib.rs"}"#));
        assert_eq!(e.output_digest, digest("contents…"));
        assert_eq!(e.tool, "Read");
        assert_eq!(e.turn, 3);
        // pager: newest-last, bounded
        for i in 0..5 {
            l.record("LS", &format!("{i}"), "out", Confidence::Partial, ".", 1);
        }
        assert_eq!(l.page(0, 3).len(), 3);
        assert_eq!(l.page(4, 10).len(), 2);
    }

    #[test]
    fn obligations_gate_the_definition_of_done() {
        let mut l = ReaLedger::new();
        let ev = l.record(
            "Bash",
            "cargo test",
            "ok",
            Confidence::Full,
            "cargo test",
            1,
        );
        let v = l.obligate(ObligationKind::Verify, "tests pass");
        let r = l.obligate(ObligationKind::Reconstruct, "the loop rewrite is shown");
        assert!(l.gate().is_err(), "open gate refuses");
        l.satisfy(v, ev).unwrap();
        // verify needs ≥ partial — a FULL record satisfies it
        let weak = l.record(
            "WebSearch",
            "q",
            "a blog said so",
            Confidence::Indirect,
            "blog",
            2,
        );
        assert!(
            l.satisfy(r, weak).is_err(),
            "indirect evidence cannot satisfy reconstruct-with-empty-coverage"
        );
        let strong = l.record(
            "Read",
            "diff",
            "the new loop",
            Confidence::Partial,
            "src/loop_.rs",
            3,
        );
        l.satisfy(r, strong).unwrap();
        assert!(l.gate().is_ok(), "everything satisfied");
        // double-satisfaction refuses
        let v2 = l.obligate(ObligationKind::Verify, "again");
        l.satisfy(v2, ev).unwrap();
        assert!(l.satisfy(v2, ev).is_err());
    }

    #[test]
    fn unknowns_track_to_resolution_or_block() {
        let mut l = ReaLedger::new();
        let u = l.record_unknown("does the CI runner have landlock?");
        assert_eq!(l.open_count(), 1);
        let ev = l.record(
            "Bash",
            "uname -a",
            "5.15 …",
            Confidence::Partial,
            "kernel",
            1,
        );
        l.verify_unknown_resolution(u, ev).unwrap();
        assert_eq!(l.open_count(), 0);
        assert!(l.gate().is_ok());
        assert!(
            l.verify_unknown_resolution(u, ev).is_err(),
            "double-resolve refuses"
        );
    }

    #[test]
    fn the_gate_names_every_blocker() {
        let mut l = ReaLedger::new();
        l.obligate(ObligationKind::Verify, "tests");
        l.record_unknown("weather");
        let err = l.gate().unwrap_err();
        assert_eq!(err.len(), 2);
        assert!(err.iter().any(|b| b.contains("tests")));
        assert!(err.iter().any(|b| b.contains("weather")));
    }

    #[test]
    fn digest_is_stable_and_content_sensitive() {
        assert_eq!(digest("a"), digest("a"));
        assert_ne!(digest("a"), digest("b"));
        assert_eq!(digest("x").len(), 16);
    }
}
