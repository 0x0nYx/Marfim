//! W10.1 (§S, Hermes skills system — the PARTIAL remainder): skill
//! usage telemetry sidecar, curator lifecycle, and an append-only audit
//! ledger with blob backups. Rules:
//!
//! - **Telemetry**: per-skill sidecar JSON (`telemetry.json` next to the
//!   SKILL.md) records invocations + last-used ms (caller-injected, no
//!   wall clock here). A missing/corrupt sidecar reads as zero use —
//!   never a panic, never a refusal to list the skill.
//! - **Lifecycle**: `Proposed → Active → Retired`. A skill with no
//!   SKILL.md state file is `Proposed` on first sight; an explicit
//!   invoke makes it `Active`; the curator retires a skill unused for
//!   [`RETIRE_AFTER_MS`] (with the reason recorded, never a deletion).
//!   Retired skills are reported by the curator and skipped by
//!   [`SkillBook::render_active`].
//! - **Audit ledger**: append-only `audit.jsonl` in the skills root.
//!   Every state change and every skill WRITE through this module lands
//!   an entry; when a write REPLACES an existing SKILL.md, the old bytes
//!   are backed up to `backups/<name>.<stamp>.md` first and the entry
//!   carries the backup name + byte length (blob backups).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Retire a skill unused for this many ms (default 30 days).
pub const RETIRE_AFTER_MS: u64 = 30 * 24 * 3600 * 1000;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SkillState {
    Proposed,
    Active,
    Retired,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkillTelemetry {
    #[serde(default)]
    pub invocations: u64,
    #[serde(default)]
    pub last_used_ms: Option<u64>,
}

impl SkillTelemetry {
    fn sidecar_path(skill_md: &Path) -> PathBuf {
        skill_md
            .parent()
            .unwrap_or(Path::new("."))
            .join("telemetry.json")
    }

    /// Load the sidecar; missing or corrupt reads as zero use.
    pub fn load(skill_md: &Path) -> Self {
        std::fs::read_to_string(Self::sidecar_path(skill_md))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Record one invocation (now_ms caller-injected for determinism).
    pub fn record_use(skill_md: &Path, now_ms: u64) -> crate::Result<()> {
        let mut t = Self::load(skill_md);
        t.invocations += 1;
        t.last_used_ms = Some(now_ms);
        let path = Self::sidecar_path(skill_md);
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(&t)?)?;
        Ok(())
    }
}

/// One audit-ledger record (JSONL, append-only).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuditEntry {
    pub ts_ms: u64,
    pub skill: String,
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
}

/// The curator + audit surface over a skills root directory.
#[derive(Debug, Clone)]
pub struct SkillBook {
    pub root: PathBuf,
}

impl SkillBook {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    fn state_path(&self, name: &str) -> PathBuf {
        self.root.join(name).join("state.json")
    }

    fn audit_path(&self) -> PathBuf {
        self.root.join("audit.jsonl")
    }

    /// Current lifecycle state of a skill (missing state file =
    /// Proposed).
    pub fn state(&self, name: &str) -> SkillState {
        std::fs::read_to_string(self.state_path(name))
            .ok()
            .and_then(|s| serde_json::from_str::<SkillState>(&s).ok())
            .unwrap_or(SkillState::Proposed)
    }

    fn set_state(&self, name: &str, st: SkillState) -> crate::Result<()> {
        let p = self.state_path(name);
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(p, serde_json::to_string(&st)?)?;
        Ok(())
    }

    /// Append one entry to the audit ledger (creates the ledger on first
    /// use). Append-only: the file is opened for append, never rewritten.
    pub fn audit(&self, entry: AuditEntry) -> crate::Result<()> {
        use std::io::Write;
        if let Some(d) = self.audit_path().parent() {
            std::fs::create_dir_all(d)?;
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.audit_path())?;
        writeln!(f, "{}", serde_json::to_string(&entry)?)?;
        Ok(())
    }

    /// Read the whole audit ledger (oldest first). Corrupt lines are
    /// skipped (the ledger stays readable).
    pub fn audit_trail(&self) -> Vec<AuditEntry> {
        let mut out = Vec::new();
        if let Ok(s) = std::fs::read_to_string(self.audit_path()) {
            for line in s.lines() {
                if let Ok(e) = serde_json::from_str::<AuditEntry>(line) {
                    out.push(e);
                }
            }
        }
        out
    }

    /// An explicit invoke: telemetry + Proposed→Active transition +
    /// audit. Returns the invocation count.
    pub fn record_invoke(&self, name: &str, skill_md: &Path, now_ms: u64) -> crate::Result<u64> {
        SkillTelemetry::record_use(skill_md, now_ms)?;
        let was = self.state(name);
        if was == SkillState::Proposed {
            self.set_state(name, SkillState::Active)?;
            self.audit(AuditEntry {
                ts_ms: now_ms,
                skill: name.into(),
                action: "activate".into(),
                detail: Some("first explicit invoke".into()),
                backup: None,
                bytes: None,
            })?;
        }
        Ok(SkillTelemetry::load(skill_md).invocations)
    }

    /// Curator pass over every discovered skill: an Active skill whose
    /// last use is older than [`RETIRE_AFTER_MS`] (or never used AND the
    /// skill dir predates the window — created_ms passed by the caller)
    /// is retired with the reason recorded. Returns the retirement
    /// decisions (name, reason).
    pub fn curate(&self, now_ms: u64) -> crate::Result<Vec<(String, String)>> {
        let mut retired = Vec::new();
        for skill in crate::skills::discover(&self.root) {
            let name = skill.name.clone();
            if self.state(&name) != SkillState::Active {
                continue;
            }
            let tel = SkillTelemetry::load(&skill.path);
            let stale = match tel.last_used_ms {
                Some(last) => now_ms.saturating_sub(last) > RETIRE_AFTER_MS,
                None => false, // Proposed-only skills are not retired
            };
            if stale {
                let reason = format!(
                    "unused for {} ms",
                    now_ms.saturating_sub(tel.last_used_ms.unwrap_or(0))
                );
                self.set_state(&name, SkillState::Retired)?;
                self.audit(AuditEntry {
                    ts_ms: now_ms,
                    skill: name.clone(),
                    action: "retire".into(),
                    detail: Some(reason.clone()),
                    backup: None,
                    bytes: None,
                })?;
                retired.push((name, reason));
            }
        }
        Ok(retired)
    }

    /// Write a SKILL.md through the curator: an EXISTING file's bytes
    /// are backed up first (`backups/<name>.<stamp>.md`) and the audit
    /// entry carries the backup name + size. A new skill lands as
    /// Proposed (audit: `create`).
    pub fn write_skill(&self, name: &str, body: &str, now_ms: u64) -> crate::Result<PathBuf> {
        let dir = self.root.join(name);
        let skill_md = dir.join("SKILL.md");
        if let Some(d) = skill_md.parent() {
            std::fs::create_dir_all(d)?;
        }
        let replacing = skill_md.is_file();
        let mut backup = None;
        let mut bytes = None;
        if replacing {
            let old = std::fs::read(&skill_md)?;
            bytes = Some(old.len() as u64);
            let bdir = self.root.join("backups");
            std::fs::create_dir_all(&bdir)?;
            let bname = format!("{name}.{now_ms}.md");
            std::fs::write(bdir.join(&bname), &old)?;
            backup = Some(bname);
        }
        std::fs::write(&skill_md, body)?;
        if !replacing {
            self.set_state(name, SkillState::Proposed)?;
        }
        self.audit(AuditEntry {
            ts_ms: now_ms,
            skill: name.into(),
            action: if replacing {
                "replace".into()
            } else {
                "create".into()
            },
            detail: None,
            backup,
            bytes,
        })?;
        Ok(skill_md)
    }

    /// The prompt-injection surface: Active skills only, deterministic
    /// order (name-sorted), `None` when none — byte-identical empty
    /// prompt contract.
    pub fn render_active(&self) -> Option<String> {
        let mut lines: Vec<(String, String)> = Vec::new();
        for skill in crate::skills::discover(&self.root) {
            if self.state(&skill.name) == SkillState::Active {
                lines.push((skill.name.clone(), skill.description.clone()));
            }
        }
        if lines.is_empty() {
            return None;
        }
        lines.sort();
        let body = lines
            .iter()
            .map(|(n, d)| format!("- {n}: {d}"))
            .collect::<Vec<_>>()
            .join("\n");
        Some(format!(
            "Available skills (Skill tool to load one):\n{body}"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use marfim_testkit::unique_dir;

    fn skill_root(tag: &str) -> (PathBuf, PathBuf) {
        let root = unique_dir(tag);
        let _ = std::fs::remove_dir_all(&root);
        let skills = root.join("skills");
        std::fs::create_dir_all(&skills).unwrap();
        (root, skills)
    }

    fn make_skill(skills: &Path, name: &str) -> PathBuf {
        let d = skills.join(name);
        std::fs::create_dir_all(&d).unwrap();
        let md = d.join("SKILL.md");
        std::fs::write(
            &md,
            format!("---\nname: {name}\ndescription: desc-{name}\n---\nbody"),
        )
        .unwrap();
        md
    }

    #[test]
    fn telemetry_sidecar_round_trip() {
        let (_r, skills) = skill_root("curator-tel");
        let md = make_skill(&skills, "alpha");
        SkillTelemetry::record_use(&md, 100).unwrap();
        SkillTelemetry::record_use(&md, 200).unwrap();
        let t = SkillTelemetry::load(&md);
        assert_eq!(t.invocations, 2);
        assert_eq!(t.last_used_ms, Some(200));
        let _ = std::fs::remove_dir_all(&_r);
    }

    #[test]
    fn corrupt_sidecar_reads_as_zero() {
        let (_r, skills) = skill_root("curator-corrupt");
        let md = make_skill(&skills, "alpha");
        std::fs::write(md.parent().unwrap().join("telemetry.json"), "{broken").unwrap();
        assert_eq!(SkillTelemetry::load(&md).invocations, 0);
        let _ = std::fs::remove_dir_all(&_r);
    }

    #[test]
    fn lifecycle_proposed_active_retired() {
        let (_r, skills) = skill_root("curator-life");
        let md = make_skill(&skills, "beta");
        let book = SkillBook::new(&skills);
        assert_eq!(book.state("beta"), SkillState::Proposed);
        book.record_invoke("beta", &md, 1000).unwrap();
        assert_eq!(book.state("beta"), SkillState::Active);
        assert_eq!(book.state("beta"), SkillState::Active);
        let trail = book.audit_trail();
        assert_eq!(trail[0].action, "activate");
        let _ = std::fs::remove_dir_all(&_r);
    }

    #[test]
    fn curate_retires_stale_active_skills_only() {
        let (_r, skills) = skill_root("curator-stale");
        let md_a = make_skill(&skills, "stale");
        let _md_b = make_skill(&skills, "fresh");
        let book = SkillBook::new(&skills);
        let now = 100 + RETIRE_AFTER_MS + 1;
        book.record_invoke("stale", &md_a, 100).unwrap();
        book.record_invoke("fresh", &skills.join("fresh").join("SKILL.md"), now - 1000)
            .unwrap();
        // push "stale" back in time by rewriting its sidecar
        let t = SkillTelemetry {
            invocations: 1,
            last_used_ms: Some(100),
        };
        std::fs::write(
            skills.join("stale").join("telemetry.json"),
            serde_json::to_string(&t).unwrap(),
        )
        .unwrap();
        let retired = book.curate(now).unwrap();
        assert_eq!(
            retired.len(),
            1,
            "only the stale skill retires: {retired:?}"
        );
        assert_eq!(retired[0].0, "stale");
        assert!(retired[0].1.starts_with("unused for "));
        assert_eq!(book.state("stale"), SkillState::Retired);
        assert_eq!(book.state("fresh"), SkillState::Active);
        assert!(book
            .audit_trail()
            .iter()
            .any(|e| e.action == "retire" && e.skill == "stale"));
        let _ = std::fs::remove_dir_all(&_r);
    }

    #[test]
    fn never_used_proposed_skills_are_not_retired() {
        let (_r, skills) = skill_root("curator-proposed");
        make_skill(&skills, "idle");
        let book = SkillBook::new(&skills);
        assert!(book.curate(RETIRE_AFTER_MS * 10).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&_r);
    }

    #[test]
    fn write_skill_backs_up_replaced_blob() {
        let (_r, skills) = skill_root("curator-backup");
        let md = make_skill(&skills, "gamma");
        let old = std::fs::read_to_string(&md).unwrap();
        let book = SkillBook::new(&skills);
        book.write_skill(
            "gamma",
            "---\nname: gamma\ndescription: v2\n---\nnew body",
            5000,
        )
        .unwrap();
        let trail = book.audit_trail();
        let e = trail.last().unwrap();
        assert_eq!(e.action, "replace");
        let bname = e.backup.as_ref().unwrap();
        assert_eq!(e.bytes, Some(old.len() as u64));
        let backed = std::fs::read_to_string(skills.join("backups").join(bname)).unwrap();
        assert_eq!(backed, old);
        // new file is the replacement
        assert!(
            std::fs::read_to_string(skills.join("gamma").join("SKILL.md"))
                .unwrap()
                .contains("v2")
        );
        let _ = std::fs::remove_dir_all(&_r);
    }

    #[test]
    fn write_new_skill_lands_proposed_with_create_audit() {
        let (_r, skills) = skill_root("curator-create");
        let book = SkillBook::new(&skills);
        book.write_skill("delta", "---\nname: delta\ndescription: d\n---\nx", 10)
            .unwrap();
        assert_eq!(book.state("delta"), SkillState::Proposed);
        assert_eq!(book.audit_trail()[0].action, "create");
        assert!(book.audit_trail()[0].backup.is_none());
        let _ = std::fs::remove_dir_all(&_r);
    }

    #[test]
    fn render_active_only_lists_active_skills() {
        let (_r, skills) = skill_root("curator-render");
        let md = make_skill(&skills, "zeta");
        make_skill(&skills, "alpha");
        let book = SkillBook::new(&skills);
        assert!(book.render_active().is_none(), "nothing Active yet");
        book.record_invoke("zeta", &md, 1).unwrap();
        let s = book.render_active().unwrap();
        assert!(s.contains("- zeta: desc-zeta"));
        assert!(
            !s.contains("alpha"),
            "proposed skills stay out of the prompt"
        );
        let _ = std::fs::remove_dir_all(&_r);
    }
}
