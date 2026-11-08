//! W12 REA tool: the evidence pipeline the model drives.
//!
//! One tool, seven actions over [`marfim_core::rea::ReaLedger`]:
//! - `record` — turn a tool result into a canonical evidence record;
//! - `obligate` — add a reconstruct/verify obligation (definition-of-done
//!   checklist);
//! - `satisfy` — satisfy an obligation WITH an evidence record;
//! - `unknown` — record an explicit unknown;
//! - `resolve` — resolve an unknown with an evidence record;
//! - `gate` — the definition-of-done gate: refuses while anything is
//!   open, naming every blocker;
//! - `page` — the retained view over the newest records.
//!
//! Additionally the tool host auto-records EVERY dispatched tool result
//! (success = Full confidence, error = Indirect) so the ledger is never
//! empty and never self-serving — the evidence exists whether or not
//! the model chooses to cite it.

use crate::registry::{ToolCtx, ToolOutput};
use marfim_core::rea::{Confidence, ObligationKind};
use serde_json::Value;

pub fn rea(ctx: &ToolCtx, args: &Value) -> ToolOutput {
    let Some(action) = args["action"].as_str() else {
        return ToolOutput::err("missing `action`");
    };
    let mut l = ctx.rea.lock().unwrap_or_else(|p| p.into_inner());
    match action {
        "record" => {
            let Some(tool) = args["tool"].as_str() else {
                return ToolOutput::err("missing `tool`");
            };
            let confidence = args["confidence"]
                .as_str()
                .and_then(Confidence::parse)
                .unwrap_or(Confidence::Partial);
            let id = l.record(
                tool,
                args["args"].as_str().unwrap_or(""),
                args["output"].as_str().unwrap_or(""),
                confidence,
                args["coverage"].as_str().unwrap_or(""),
                args["turn"].as_u64().unwrap_or(0),
            );
            ToolOutput::ok(format!(
                "evidence record {id} ({tool}, confidence: {})",
                confidence.name()
            ))
        }
        "obligate" => {
            let Some(kind) = args["kind"].as_str().and_then(ObligationKind::parse) else {
                return ToolOutput::err("missing `kind` (reconstruct|verify)");
            };
            let Some(desc) = args["description"].as_str() else {
                return ToolOutput::err("missing `description`");
            };
            let id = l.obligate(kind, desc);
            ToolOutput::ok(format!("obligation {id} ({}) open: {desc}", kind.name()))
        }
        "satisfy" => {
            let (Some(o), Some(e)) = (args["obligation_id"].as_u64(), args["evidence_id"].as_u64())
            else {
                return ToolOutput::err("missing `obligation_id`/`evidence_id`");
            };
            match l.satisfy(o, e) {
                Ok(()) => ToolOutput::ok(format!("obligation {o} satisfied by evidence {e}")),
                Err(err) => ToolOutput::err(err),
            }
        }
        "unknown" => {
            let Some(q) = args["question"].as_str() else {
                return ToolOutput::err("missing `question`");
            };
            let id = l.record_unknown(q);
            ToolOutput::ok(format!("unknown {id} recorded: {q}"))
        }
        "resolve" => {
            let (Some(u), Some(e)) = (args["unknown_id"].as_u64(), args["evidence_id"].as_u64())
            else {
                return ToolOutput::err("missing `unknown_id`/`evidence_id`");
            };
            match l.verify_unknown_resolution(u, e) {
                Ok(()) => ToolOutput::ok(format!("unknown {u} resolved by evidence {e}")),
                Err(err) => ToolOutput::err(err),
            }
        }
        "gate" => match l.gate() {
            Ok(()) => ToolOutput::ok(format!(
                "definition-of-done gate PASSES ({} evidence records, 0 open items)",
                l.evidence_count()
            )),
            Err(blockers) => ToolOutput::err(format!(
                "definition-of-done gate BLOCKED ({} open):\n{}",
                blockers.len(),
                blockers
                    .iter()
                    .map(|b| format!("- {b}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            )),
        },
        "page" => {
            let offset = args["offset"].as_u64().unwrap_or(0) as usize;
            let limit = args["limit"].as_u64().unwrap_or(20).min(100) as usize;
            let page = l.page(offset, limit);
            if page.is_empty() {
                return ToolOutput::ok("no evidence records in this page");
            }
            let out: Vec<String> = page
                .iter()
                .map(|e| {
                    format!(
                        "e{} — {} (confidence: {}, coverage: {}, turn {})",
                        e.id,
                        e.tool,
                        e.confidence.name(),
                        e.coverage,
                        e.turn
                    )
                })
                .collect();
            ToolOutput::ok(out.join("\n"))
        }
        other => ToolOutput::err(format!(
            "unknown action `{other}` (expected record|obligate|satisfy|unknown|resolve|gate|page)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::ToolCtx;

    fn ctx() -> ToolCtx {
        ToolCtx::default()
    }

    #[test]
    fn the_full_gate_workflow() {
        let c = ctx();
        // evidence from a real record
        let out = rea(
            &c,
            &serde_json::json!({
                "action": "record",
                "tool": "Bash",
                "args": "cargo test",
                "output": "test result: ok",
                "confidence": "full",
                "coverage": "cargo test",
                "turn": 2,
            }),
        );
        assert!(!out.is_error, "{}", out.output);
        let ev_id: u64 = out
            .output
            .split_whitespace()
            .nth(2)
            .and_then(|s| s.parse().ok())
            .unwrap();
        // obligations + unknowns
        let o = rea(
            &c,
            &serde_json::json!({"action":"obligate","kind":"verify","description":"tests pass"}),
        );
        let ob_id: u64 = o
            .output
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap();
        let u = rea(
            &c,
            &serde_json::json!({"action":"unknown","question":"is fs gated?"}),
        );
        let un_id: u64 = u
            .output
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap();
        // gate blocked, naming both
        let gate = rea(&c, &serde_json::json!({"action":"gate"}));
        assert!(gate.is_error);
        assert!(gate.output.contains("tests pass") && gate.output.contains("is fs gated?"));
        // satisfy + resolve → gate passes
        assert!(
            !rea(
                &c,
                &serde_json::json!({"action":"satisfy","obligation_id":ob_id,"evidence_id":ev_id})
            )
            .is_error
        );
        assert!(
            !rea(
                &c,
                &serde_json::json!({"action":"resolve","unknown_id":un_id,"evidence_id":ev_id})
            )
            .is_error
        );
        let gate = rea(&c, &serde_json::json!({"action":"gate"}));
        assert!(!gate.is_error, "{}", gate.output);
        assert!(gate.output.contains("PASSES"));
    }

    #[test]
    fn bad_confidence_and_actions_fail_honestly() {
        let c = ctx();
        let rec = rea(
            &c,
            &serde_json::json!({"action":"record","tool":"T","confidence":"vibes"}),
        );
        assert!(!rec.is_error, "unknown confidence falls back to partial");
        assert!(rea(&c, &serde_json::json!({"action":"obligate","kind":"vibes"})).is_error);
        assert!(
            rea(
                &c,
                &serde_json::json!({"action":"satisfy","obligation_id":9,"evidence_id":9})
            )
            .is_error
        );
        assert!(rea(&c, &serde_json::json!({"action":"explode"})).is_error);
        assert!(rea(&c, &serde_json::json!({})).is_error);
    }

    #[test]
    fn paging_is_bounded() {
        let c = ctx();
        for i in 0..7 {
            let _ = rea(
                &c,
                &serde_json::json!({"action":"record","tool":"T","args":i.to_string(),"output":"o"}),
            );
        }
        let p = rea(
            &c,
            &serde_json::json!({"action":"page","offset":5,"limit":3}),
        );
        assert_eq!(p.output.lines().count(), 2, "5..7 = two records");
    }
}
