//! W10.1 (§S + ECC continuous-learning-v2): instincts — atomic,
//! confidence-scored behaviors injected at session start. An instinct is
//! ONE sentence of learned behavior. Injection: confidence ≥
//! [`INJECT_THRESHOLD`], rendered as numbered lines, byte-identical
//! empty when none (the prompt contract). Reinforcement raises
//! confidence toward 1.0; contradictions decay it; an instinct whose
//! confidence sinks under [`RETIRE_THRESHOLD`] is auto-retired
//! (recorded, never deleted — the audit trail stays). Evolution: an
//! instinct with enough evidence ([`PROMOTE_HITS`] hits and confidence
//! ≥ [`PROMOTE_CONFIDENCE`]) yields a skill proposal — the WRITE is
//! always user-approved (propose-approve, the idle_propose doctrine);
//! the store only reports the candidate.
//!
//! Determinism: store order is insertion order; renders are
//! deterministic given the same store state.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// An instinct at or above this confidence is injected at session start.
pub const INJECT_THRESHOLD: f64 = 0.6;
/// Below this confidence an instinct is retired automatically.
pub const RETIRE_THRESHOLD: f64 = 0.2;
/// Hits required before promotion to a skill candidate.
pub const PROMOTE_HITS: u32 = 5;
/// Confidence required (with the hits) before promotion.
pub const PROMOTE_CONFIDENCE: f64 = 0.8;
/// Reinforcement delta per confirming event.
pub const REINFORCE_DELTA: f64 = 0.1;
/// Decay delta per contradicting event.
pub const DECAY_DELTA: f64 = 0.15;
/// Bounded store: no more than this many instincts (drop-oldest on add).
pub const MAX_INSTINCTS: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Instinct {
    pub id: String,
    /// ONE atomic behavior sentence. Never a multi-clause rule.
    pub text: String,
    pub confidence: f64,
    pub hits: u32,
    pub misses: u32,
    /// creation time as unix ms (injected; never generated here)
    pub created_ms: u64,
    /// where it came from (e.g. "review-fork", "user", "instinct:<id>")
    pub source: String,
    /// auto-retired or explicitly dropped instincts stay recorded
    pub retired: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InstinctStore {
    pub instincts: Vec<Instinct>,
    next_id: u64,
}

/// Promotion candidate produced by [`InstinctStore::promote_candidate`].
#[derive(Debug, Clone, PartialEq)]
pub struct SkillCandidate {
    pub instinct_id: String,
    pub skill_name: String,
    pub description: String,
    pub body: String,
}

impl InstinctStore {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> crate::Result<()> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Record a new instinct. Text is trimmed; empty text is refused;
    /// an EXACT duplicate text is refused (idempotent re-adds are a
    /// capture bug, not a reinforcement). Store cap is drop-oldest over
    /// non-retired entries.
    pub fn add(
        &mut self,
        text: &str,
        confidence: f64,
        created_ms: u64,
        source: &str,
    ) -> Option<String> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        if self
            .instincts
            .iter()
            .any(|i| i.text == text && i.retired.is_none())
        {
            return None;
        }
        self.next_id += 1;
        let id = format!("i{}", self.next_id);
        self.instincts.push(Instinct {
            id: id.clone(),
            text: text.to_string(),
            confidence: confidence.clamp(0.0, 1.0),
            hits: 0,
            misses: 0,
            created_ms,
            source: source.to_string(),
            retired: None,
        });
        self.enforce_cap();
        Some(id)
    }

    fn enforce_cap(&mut self) {
        while self.active_len() > MAX_INSTINCTS {
            // retire the OLDEST active instinct (insertion order)
            if let Some(idx) = self.instincts.iter().position(|i| i.retired.is_none()) {
                self.instincts[idx].retired = Some("cap-drop-oldest".into());
            } else {
                break;
            }
        }
    }

    fn active_len(&self) -> usize {
        self.instincts
            .iter()
            .filter(|i| i.retired.is_none())
            .count()
    }

    /// Reinforce: +[`REINFORCE_DELTA`], capped at 1.0, hits += 1.
    /// Unknown ids are a no-op (the caller decided the id locally).
    pub fn reinforce(&mut self, id: &str) -> bool {
        match self
            .instincts
            .iter_mut()
            .find(|i| i.id == id && i.retired.is_none())
        {
            Some(i) => {
                i.confidence = (i.confidence + REINFORCE_DELTA).min(1.0);
                i.hits += 1;
                true
            }
            None => false,
        }
    }

    /// Decay: −[`DECAY_DELTA`], floored at 0.0, misses += 1; an instinct
    /// sinking under [`RETIRE_THRESHOLD`] is auto-retired with the exact
    /// reason (audit trail, never deletion).
    pub fn decay(&mut self, id: &str) -> bool {
        match self
            .instincts
            .iter_mut()
            .find(|i| i.id == id && i.retired.is_none())
        {
            Some(i) => {
                i.confidence = (i.confidence - DECAY_DELTA).max(0.0);
                i.misses += 1;
                if i.confidence < RETIRE_THRESHOLD {
                    i.retired = Some("auto: confidence under threshold".into());
                }
                true
            }
            None => false,
        }
    }

    /// Explicit user drop. The instinct stays recorded with the reason.
    pub fn retire(&mut self, id: &str, reason: &str) -> bool {
        match self
            .instincts
            .iter_mut()
            .find(|i| i.id == id && i.retired.is_none())
        {
            Some(i) => {
                i.retired = Some(if reason.trim().is_empty() {
                    "user drop".into()
                } else {
                    format!("user drop: {}", reason.trim())
                });
                true
            }
            None => false,
        }
    }

    /// The session-start injection surface: active instincts with
    /// confidence ≥ [`INJECT_THRESHOLD`], deterministic order (insertion),
    /// `None` when empty — callers keep the base prompt byte-identical.
    pub fn render_active(&self) -> Option<String> {
        let active: Vec<&Instinct> = self
            .instincts
            .iter()
            .filter(|i| i.retired.is_none() && i.confidence >= INJECT_THRESHOLD)
            .collect();
        if active.is_empty() {
            return None;
        }
        let body = active
            .iter()
            .map(|i| format!("{}. {}", i.id, i.text))
            .collect::<Vec<_>>()
            .join("\n");
        Some(format!(
            "Learned behaviors (instincts — follow unless the user overrides):\n{body}"
        ))
    }

    /// Promotion: an instinct with ≥[`PROMOTE_HITS`] hits and confidence
    /// ≥[`PROMOTE_CONFIDENCE`] yields a skill candidate. The candidate is
    /// a PROPOSAL — writing it is the caller's user-approved step. The
    /// instinct stays (a promoted instinct is retired only when the
    /// skill lands, via [`InstinctStore::drop`] with the skill name).
    pub fn promote_candidate(&self) -> Option<SkillCandidate> {
        let i = self.instincts.iter().find(|i| {
            i.retired.is_none() && i.hits >= PROMOTE_HITS && i.confidence >= PROMOTE_CONFIDENCE
        })?;
        let name: String = i
            .text
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect::<String>()
            .split('-')
            .filter(|s| !s.is_empty())
            .take(4)
            .collect::<Vec<_>>()
            .join("-");
        let name = if name.is_empty() {
            "instinct".into()
        } else {
            name
        };
        let body = format!(
            "---\nname: {name}\ndescription: {}\n---\nPromoted from instinct {} ({} hits, confidence {:.2}).\n\n{}",
            i.text, i.id, i.hits, i.confidence, i.text
        );
        Some(SkillCandidate {
            instinct_id: i.id.clone(),
            skill_name: name,
            description: i.text.clone(),
            body,
        })
    }
}

/// Default path under a project root: `<root>/.marfim/instincts.json`.
pub fn instincts_path(root: &Path) -> PathBuf {
    root.join(".marfim").join("instincts.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> InstinctStore {
        InstinctStore::default()
    }

    #[test]
    fn add_and_render_active() {
        let mut s = store();
        let id = s
            .add("Prefer rg over grep in this repo", 0.7, 1, "user")
            .unwrap();
        assert_eq!(id, "i1");
        let surface = s.render_active().unwrap();
        assert!(surface.starts_with("Learned behaviors"));
        assert!(surface.contains("1. Prefer rg over grep in this repo"));
    }

    #[test]
    fn empty_and_low_confidence_render_none() {
        let mut s = store();
        assert!(s.render_active().is_none());
        s.add("weak behavior", 0.5, 1, "user").unwrap();
        assert!(s.render_active().is_none(), "below INJECT_THRESHOLD");
    }

    #[test]
    fn duplicate_text_refused() {
        let mut s = store();
        assert!(s.add("behavior", 0.7, 1, "user").is_some());
        assert!(s.add("behavior", 0.7, 2, "user").is_none());
    }

    #[test]
    fn empty_text_refused() {
        let mut s = store();
        assert!(s.add("   ", 0.7, 1, "user").is_none());
    }

    #[test]
    fn reinforce_caps_at_one_and_counts_hits() {
        let mut s = store();
        let id = s.add("b", 0.95, 1, "user").unwrap();
        for _ in 0..5 {
            s.reinforce(&id);
        }
        let i = &s.instincts[0];
        assert_eq!(i.confidence, 1.0);
        assert_eq!(i.hits, 5);
    }

    #[test]
    fn decay_retires_under_threshold_and_keeps_record() {
        let mut s = store();
        let id = s.add("b", 0.3, 1, "user").unwrap();
        assert!(s.decay(&id));
        let i = &s.instincts[0];
        assert_eq!(i.misses, 1);
        assert_eq!(
            i.retired.as_deref(),
            Some("auto: confidence under threshold")
        );
        assert!(s.render_active().is_none());
        // retired id can no longer be reinforced
        assert!(!s.reinforce(&id));
    }

    #[test]
    fn unknown_ids_are_noops() {
        let mut s = store();
        assert!(!s.reinforce("nope"));
        assert!(!s.decay("nope"));
        assert!(!s.retire("nope", "x"));
    }

    #[test]
    fn promotion_requires_hits_and_confidence() {
        let mut s = store();
        let id = s
            .add("always run cargo fmt before committing", 0.85, 1, "user")
            .unwrap();
        assert!(s.promote_candidate().is_none(), "0 hits");
        for _ in 0..PROMOTE_HITS {
            s.reinforce(&id);
        }
        let c = s.promote_candidate().unwrap();
        assert_eq!(c.instinct_id, id);
        assert!(c.body.contains("name: always-run-cargo-fmt"));
        assert!(c.body.contains("---\n"));
    }

    #[test]
    fn store_cap_drops_oldest_active() {
        let mut s = store();
        for n in 0..(MAX_INSTINCTS + 1) {
            s.add(&format!("behavior {n}"), 0.9, n as u64, "user")
                .unwrap();
        }
        assert_eq!(s.active_len(), MAX_INSTINCTS);
        assert_eq!(s.instincts[0].retired.as_deref(), Some("cap-drop-oldest"));
        // the newest survives
        assert!(s
            .render_active()
            .unwrap()
            .contains(&format!("behavior {MAX_INSTINCTS}")));
    }

    #[test]
    fn save_load_round_trip() {
        let dir = marfim_testkit::unique_dir("instinct-store");
        let _ = std::fs::remove_dir_all(&dir);
        let path = instincts_path(&dir);
        let mut s = store();
        s.add("b1", 0.7, 5, "review-fork").unwrap();
        s.save(&path).unwrap();
        let s2 = InstinctStore::load(&path);
        assert_eq!(s2.instincts.len(), 1);
        assert_eq!(s2.instincts[0].source, "review-fork");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
