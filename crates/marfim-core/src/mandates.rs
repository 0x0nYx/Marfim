//! W10.2 (§G, dsh-superagent policy doctrine): mandates — versioned,
//! revocable authorization objects for autonomous work. The gate is
//! DETERMINISTIC (tool/path/scope/expiry/budget); an LLM assessment is
//! advisory ONLY — it is recorded on the decision but can NEVER flip it
//! (test-asserted). Revocation is monotonic: a revoked mandate never
//! re-authorizes, and amendments create a NEW version (the old one stays
//! recorded; the ledger never rewrites history).

use serde::{Deserialize, Serialize};

/// One authorization object. `version` starts at 1 and increments on
/// every amendment; superseded versions stay in the book.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Mandate {
    pub id: String,
    pub version: u64,
    /// who acts under it (e.g. "worker:PROJ-12")
    pub grantee: String,
    pub purpose: String,
    /// tool names allowed under this mandate (exact match)
    pub tools: Vec<String>,
    /// path prefixes allowed (canonical slash form, exact prefix)
    pub paths: Vec<String>,
    /// max actions; None = unbounded within expiry
    pub max_actions: Option<u32>,
    /// expiry as unix ms; None = no expiry
    pub expires_ms: Option<u64>,
    pub revoked: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActionRequest {
    pub tool: String,
    /// canonical path of the resource ("" for pathless actions)
    pub path: String,
    pub now_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum GateDecision {
    Allow {
        mandate_id: String,
        version: u64,
        /// remaining actions under the budget (None = unbounded)
        remaining: Option<u32>,
    },
    Deny {
        mandate_id: String,
        version: u64,
        reason: String,
    },
}

/// Advisory LLM assessment: recorded on the outcome, never decides.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Assessment {
    pub recommended: bool,
    pub note: String,
}

/// The outcome of a gated action attempt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GatedOutcome {
    pub decision: GateDecision,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assessment: Option<Assessment>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MandateBook {
    /// ALL versions of every mandate, in issue order.
    pub mandates: Vec<Mandate>,
    /// actions consumed per mandate id (budget accounting)
    pub consumed: std::collections::BTreeMap<String, u32>,
    next_version: u64,
}

impl MandateBook {
    /// Issue mandate v1.
    #[allow(clippy::too_many_arguments)]
    pub fn issue(
        &mut self,
        id: &str,
        grantee: &str,
        purpose: &str,
        tools: Vec<String>,
        paths: Vec<String>,
        max_actions: Option<u32>,
        expires_ms: Option<u64>,
    ) -> Mandate {
        self.next_version += 1;
        let m = Mandate {
            id: id.to_string(),
            version: 1,
            grantee: grantee.to_string(),
            purpose: purpose.to_string(),
            tools,
            paths,
            max_actions,
            expires_ms,
            revoked: false,
        };
        self.mandates.push(m.clone());
        m
    }

    /// Amend: the CURRENT version must not be revoked; the amendment
    /// lands as a NEW version (history preserved).
    pub fn amend(
        &mut self,
        id: &str,
        tools: Vec<String>,
        paths: Vec<String>,
        max_actions: Option<u32>,
        expires_ms: Option<u64>,
    ) -> Option<Mandate> {
        let current = self
            .mandates
            .iter()
            .filter(|m| m.id == id)
            .max_by_key(|m| m.version)?;
        if current.revoked {
            return None;
        }
        self.next_version += 1;
        let m = Mandate {
            id: id.to_string(),
            version: current.version + 1,
            grantee: current.grantee.clone(),
            purpose: current.purpose.clone(),
            tools,
            paths,
            max_actions,
            expires_ms,
            revoked: false,
        };
        self.mandates.push(m.clone());
        Some(m)
    }

    /// Revoke: monotonic — every current and FUTURE amendment attempt
    /// on a revoked id is refused.
    pub fn revoke(&mut self, id: &str, now_ms: u64) -> Option<u64> {
        let current = self
            .mandates
            .iter()
            .filter(|m| m.id == id)
            .max_by_key(|m| m.version)?;
        if current.revoked {
            return None;
        }
        let v = current.version;
        self.next_version += 1;
        self.mandates.push(Mandate {
            id: id.to_string(),
            version: v + 1,
            grantee: current.grantee.clone(),
            purpose: current.purpose.clone(),
            tools: vec![],
            paths: vec![],
            max_actions: Some(0),
            expires_ms: Some(now_ms),
            revoked: true,
        });
        Some(v + 1)
    }

    /// The current (highest-version) mandate for an id.
    pub fn current(&self, id: &str) -> Option<&Mandate> {
        self.mandates
            .iter()
            .filter(|m| m.id == id)
            .max_by_key(|m| m.version)
    }

    /// THE GATE: deterministic. Order of refusal checks is fixed and
    /// test-pinned: revoked → expired → tool scope → path scope →
    /// budget. An advisory assessment may be ATTACHED afterwards (see
    /// [`MandateBook::gate_with_assessment`]) but never flips a Deny.
    pub fn gate(&self, req: &ActionRequest) -> GateDecision {
        let id = id_of(req);
        // the tool name rides after the "id|" prefix (see [`action`])
        let tool = req
            .tool
            .split_once('|')
            .map(|(_, t)| t.to_string())
            .unwrap_or(req.tool.clone());
        let Some(m) = self.current(&id) else {
            return GateDecision::Deny {
                mandate_id: id,
                version: 0,
                reason: "no mandate with this id".into(),
            };
        };
        let deny = |reason: String| GateDecision::Deny {
            mandate_id: m.id.clone(),
            version: m.version,
            reason,
        };
        if m.revoked {
            return deny(format!("mandate {} v{} is revoked", m.id, m.version));
        }
        if let Some(exp) = m.expires_ms {
            if req.now_ms >= exp {
                return deny(format!("mandate expired at {exp}"));
            }
        }
        if !m.tools.iter().any(|t| t == &tool) {
            return deny(format!("tool {tool:?} is outside the mandate scope"));
        }
        if !m.paths.iter().any(|p| req.path.starts_with(p.as_str())) {
            return deny(format!("path {:?} is outside the mandate scope", req.path));
        }
        if let Some(max) = m.max_actions {
            let used = *self
                .consumed
                .get(&format!("{}:{}", m.id, m.version))
                .unwrap_or(&0);
            if used >= max {
                return deny(format!("mandate budget exhausted ({used}/{max} actions)"));
            }
        }
        GateDecision::Allow {
            mandate_id: m.id.clone(),
            version: m.version,
            remaining: m.max_actions.map(|max| {
                let used = *self
                    .consumed
                    .get(&format!("{}:{}", m.id, m.version))
                    .unwrap_or(&0);
                max - used
            }),
        }
    }

    /// Record one consumed action against the CURRENT version.
    pub fn record_action(&mut self, id: &str) {
        if let Some(m) = self.current(id) {
            let key = format!("{}:{}", m.id, m.version);
            *self.consumed.entry(key).or_insert(0) += 1;
        }
    }

    /// Gate + attach an advisory assessment. THE ASSESSMENT NEVER
    /// DECIDES: a Deny stays a Deny with the assessment recorded; an
    /// Allow stays an Allow even when the assessment recommends against
    /// (both directions test-asserted — the deterministic gate is the
    /// sole authority).
    pub fn gate_with_assessment(&self, req: &ActionRequest, a: Assessment) -> GatedOutcome {
        GatedOutcome {
            decision: self.gate(req),
            assessment: Some(a),
        }
    }
}

/// The mandate id rides the request (the gate looks it up).
fn id_of(req: &ActionRequest) -> String {
    req.tool
        .split_once('|')
        .map(|(id, _tool)| id.to_string())
        .unwrap_or_else(|| req.tool.clone())
}

/// Helper: build an ActionRequest naming a mandate + tool (the id
/// convention the gateway uses: "mandate-id|ToolName").
pub fn action(id: &str, tool: &str, path: &str, now_ms: u64) -> ActionRequest {
    ActionRequest {
        tool: format!("{id}|{tool}"),
        path: path.to_string(),
        now_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_gate_allow_within_scope() {
        let mut b = MandateBook::default();
        b.issue(
            "m1",
            "worker:PROJ-1",
            "tidy tests",
            vec!["Read".into(), "Edit".into()],
            vec!["/repo/tests".into()],
            Some(10),
            Some(1000),
        );
        let d = b.gate(&action("m1", "Edit", "/repo/tests/x.rs", 500));
        match d {
            GateDecision::Allow {
                version, remaining, ..
            } => {
                assert_eq!(version, 1);
                assert_eq!(remaining, Some(10));
            }
            GateDecision::Deny { reason, .. } => panic!("denied: {reason}"),
        }
    }

    #[test]
    fn refusal_order_is_pinned() {
        let mut b = MandateBook::default();
        b.issue(
            "m1",
            "w",
            "p",
            vec!["Read".into()],
            vec!["/repo".into()],
            Some(1),
            Some(100),
        );
        let d = b.gate(&action("m1", "Read", "/repo/x", 100));
        assert!(matches!(&d, GateDecision::Deny { reason, .. } if reason.contains("expired")));
        // tool scope before path scope
        let d2 = b.gate(&action("m1", "Bash", "/elsewhere", 50));
        assert!(matches!(&d2, GateDecision::Deny { reason, .. } if reason.contains("tool")));
        let d3 = b.gate(&action("m1", "Read", "/elsewhere", 50));
        assert!(matches!(&d3, GateDecision::Deny { reason, .. } if reason.contains("path")));
        // budget: one action then exhausted
        assert!(matches!(
            b.gate(&action("m1", "Read", "/repo/a", 50)),
            GateDecision::Allow { .. }
        ));
        b.record_action("m1");
        let d4 = b.gate(&action("m1", "Read", "/repo/a", 50));
        assert!(
            matches!(&d4, GateDecision::Deny { reason, .. } if reason.contains("budget exhausted"))
        );
        // unknown id
        let d5 = b.gate(&action("nope", "Read", "/repo", 50));
        assert!(matches!(&d5, GateDecision::Deny { reason, .. } if reason.contains("no mandate")));
    }

    #[test]
    fn revocation_is_monotonic_and_blocks_amendments() {
        let mut b = MandateBook::default();
        b.issue(
            "m1",
            "w",
            "p",
            vec!["Read".into()],
            vec!["/repo".into()],
            None,
            None,
        );
        let v = b.revoke("m1", 100).unwrap();
        assert_eq!(v, 2);
        let d = b.gate(&action("m1", "Read", "/repo", 100));
        assert!(matches!(&d, GateDecision::Deny { reason, .. } if reason.contains("revoked")));
        assert!(b.amend("m1", vec![], vec![], None, None).is_none());
        assert!(b.revoke("m1", 100).is_none(), "double revoke refused");
        // history preserved: v1 + the revocation tombstone
        assert_eq!(b.mandates.iter().filter(|m| m.id == "m1").count(), 2);
    }

    #[test]
    fn amendment_creates_new_version_and_gate_uses_it() {
        let mut b = MandateBook::default();
        b.issue(
            "m1",
            "w",
            "p",
            vec!["Read".into()],
            vec!["/repo".into()],
            None,
            None,
        );
        let m2 = b
            .amend(
                "m1",
                vec!["Read".into(), "Edit".into()],
                vec!["/repo".into()],
                None,
                None,
            )
            .unwrap();
        assert_eq!(m2.version, 2);
        assert!(matches!(
            b.gate(&action("m1", "Edit", "/repo/x", 1)),
            GateDecision::Allow { version: 2, .. }
        ));
        // budget accounting is per-version
        b.record_action("m1");
        assert!(matches!(
            b.gate(&action("m1", "Edit", "/repo/x", 1)),
            GateDecision::Allow { .. }
        ));
    }

    #[test]
    fn assessment_never_flips_the_gate() {
        let mut b = MandateBook::default();
        b.issue(
            "m1",
            "w",
            "p",
            vec!["Read".into()],
            vec!["/repo".into()],
            None,
            None,
        );
        // deny + approving assessment stays a deny
        let out = b.gate_with_assessment(
            &action("m1", "Bash", "/repo", 1),
            Assessment {
                recommended: true,
                note: "looks fine".into(),
            },
        );
        assert!(matches!(out.decision, GateDecision::Deny { .. }));
        assert_eq!(out.assessment.unwrap().note, "looks fine");
        // allow + rejecting assessment stays an allow
        let out2 = b.gate_with_assessment(
            &action("m1", "Read", "/repo", 1),
            Assessment {
                recommended: false,
                note: "seems risky".into(),
            },
        );
        assert!(matches!(out2.decision, GateDecision::Allow { .. }));
        assert_eq!(out2.assessment.unwrap().note, "seems risky");
    }
}
