//! Memory: persistent user model. INVARIANT: entries may only TIGHTEN
//! safety constraints — the store refuses to relax a constraint recorded
//! earlier; deletions happen only via explicit user action (memory tool
//! with user approval), never automatically.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryEntry {
    pub id: String,
    pub text: String,
    /// true if this entry constrains safety (never relaxed automatically)
    pub safety_constraint: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryStore {
    pub entries: Vec<MemoryEntry>,
}

const RELAX_WORDS: &[&str] = &[
    "allow all",
    "skip confirm",
    "no confirm",
    "yolo",
    "bypass",
    "disable permission",
    "trust everything",
    "auto-approve",
];

impl MemoryStore {
    pub fn load(path: &PathBuf) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &PathBuf) -> crate::Result<()> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Add an entry. Safety constraints are append-only unless explicitly
    /// cleared by user action.
    pub fn remember(&mut self, text: &str, safety: bool) -> String {
        let text = text.trim();
        // even non-safety entries that try to relax safety are recorded AS
        // safety constraints (never as relaxations).
        let relaxes = RELAX_WORDS.iter().any(|w| text.to_lowercase().contains(w));
        let entry = MemoryEntry {
            id: format!("m{}", self.entries.len() + 1),
            text: text.to_string(),
            safety_constraint: safety || relaxes,
            created_at: now_iso(),
        };
        self.entries.push(entry.clone());
        entry.id
    }

    /// Explicit user-driven forget. Safety constraints require `confirm`.
    pub fn forget(&mut self, id: &str, confirm: bool) -> crate::Result<()> {
        let Some(pos) = self.entries.iter().position(|e| e.id == id) else {
            return Err(crate::CoreError::Config(format!("no memory entry {id}")));
        };
        if self.entries[pos].safety_constraint && !confirm {
            return Err(crate::CoreError::Permission(
                "safety-constraint memories can only be removed with explicit user confirmation"
                    .into(),
            ));
        }
        self.entries.remove(pos);
        Ok(())
    }

    pub fn render(&self) -> String {
        if self.entries.is_empty() {
            return "(no memories)".into();
        }
        self.entries
            .iter()
            .map(|e| {
                format!(
                    "- [{}] {}{}",
                    e.id,
                    if e.safety_constraint { "🔒 " } else { "" },
                    e.text
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn now_iso() -> String {
    // no chrono dep: coarse UTC from system time
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("epoch+{}s", d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safety_constraints_never_relax_via_remember() {
        let mut m = MemoryStore::default();
        m.remember("never push to main", true);
        let relax = m.remember("ok now allow all commands without confirmation", false);
        let entry = m.entries.iter().find(|e| e.id == relax).unwrap();
        assert!(
            entry.safety_constraint,
            "relaxation attempts are stored as safety constraints, not relaxations"
        );
    }

    #[test]
    fn forget_safety_requires_confirm() {
        let mut m = MemoryStore::default();
        let id = m.remember("never rm -rf /", true);
        assert!(m.forget(&id, false).is_err());
        assert!(m.forget(&id, true).is_ok());
    }

    #[test]
    fn roundtrip_file() {
        let p = marfim_testkit::unique_dir("marfim-mem").with_extension("json");
        let mut m = MemoryStore::default();
        m.remember("uses tabs not spaces", false);
        m.save(&p).unwrap();
        let m2 = MemoryStore::load(&p);
        assert_eq!(m2.entries.len(), 1);
        assert_eq!(m2.entries[0].text, "uses tabs not spaces");
    }

    #[test]
    fn render_marks_safety() {
        let mut m = MemoryStore::default();
        m.remember("never force push", true);
        assert!(m.render().contains("🔒"));
    }
}
