//! W10.1 (§S, ECC ecc.memory.v1): the portable memory vault — an
//! import/export format that moves memories ACROSS harnesses. The vault
//! is UNTRUSTED DATA by doctrine: an imported document is size-capped,
//! field-validated, control-character stripped, and never executed —
//! its entries may only enter the local memory store through the same
//! `remember` path (RELAX_WORDS promotion intact). Unknown format
//! versions fail closed; malformed entries fail closed with the entry
//! index in the error.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The only version this build accepts (exact string).
pub const VAULT_VERSION: &str = "ecc.memory.v1";

/// Import caps: an oversized vault is refused, not truncated (a silent
/// truncation would drop memory the user believed was kept).
pub const MAX_ENTRIES: usize = 256;
pub const MAX_ENTRY_CHARS: usize = 2000;
pub const MAX_DOC_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VaultEntry {
    pub id: String,
    pub content: String,
    /// fact | preference | procedure (unknown kinds fail closed)
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VaultDoc {
    pub version: String,
    pub entries: Vec<VaultEntry>,
}

fn strip_control(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect()
}

/// Parse an imported vault document. Fails closed on: non-object JSON,
/// wrong/missing version, oversized docs/entries, unknown kinds, empty
/// ids, oversized entry counts. Content is control-stripped (untrusted
/// data hygiene), never otherwise rewritten.
pub fn import(json: &str) -> Result<VaultDoc, String> {
    if json.len() > MAX_DOC_BYTES {
        return Err(format!(
            "vault document exceeds {} bytes ({}); import refused",
            MAX_DOC_BYTES,
            json.len()
        ));
    }
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("vault is not valid JSON: {e}"))?;
    let version = v
        .get("version")
        .and_then(|x| x.as_str())
        .ok_or("vault document missing `version`")?;
    if version != VAULT_VERSION {
        return Err(format!(
            "unsupported vault version {version:?} (this build reads {VAULT_VERSION:?} only)"
        ));
    }
    let entries = v
        .get("entries")
        .and_then(|x| x.as_array())
        .ok_or("vault document missing `entries` array")?;
    if entries.len() > MAX_ENTRIES {
        return Err(format!(
            "vault holds {} entries; the cap is {MAX_ENTRIES} — import refused",
            entries.len()
        ));
    }
    let mut out = Vec::with_capacity(entries.len());
    for (n, e) in entries.iter().enumerate() {
        let id = e
            .get("id")
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("entry {n}: missing or empty `id`"))?;
        let content = e
            .get("content")
            .and_then(|x| x.as_str())
            .ok_or_else(|| format!("entry {n} ({id}): missing `content`"))?;
        if content.chars().count() > MAX_ENTRY_CHARS {
            return Err(format!(
                "entry {n} ({id}): {} chars exceeds the {MAX_ENTRY_CHARS}-char cap",
                content.chars().count()
            ));
        }
        let kind = e
            .get("type")
            .and_then(|x| x.as_str())
            .ok_or_else(|| format!("entry {n} ({id}): missing `type`"))?;
        if !matches!(kind, "fact" | "preference" | "procedure") {
            return Err(format!("entry {n} ({id}): unknown type {kind:?}"));
        }
        let tags = e
            .get("tags")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|t| t.as_str())
                    .map(|t| strip_control(t).trim().to_string())
                    .filter(|t| !t.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        out.push(VaultEntry {
            id: strip_control(id).trim().to_string(),
            content: strip_control(content).trim().to_string(),
            kind: kind.to_string(),
            tags,
        });
    }
    Ok(VaultDoc {
        version: version.to_string(),
        entries: out,
    })
}

/// Export the local memory store into the portable format. IDs are
/// `local:<n>` in insertion order; kind is `fact` unless the entry is a
/// safety constraint (`procedure` is the closest portable kind for a
/// hard rule — the mapping is documented, not invented).
pub fn export(entries: &[crate::memory::MemoryEntry]) -> VaultDoc {
    VaultDoc {
        version: VAULT_VERSION.into(),
        entries: entries
            .iter()
            .enumerate()
            .map(|(n, m)| VaultEntry {
                id: format!("local:{}", n + 1),
                content: m.text.clone(),
                kind: if m.safety_constraint {
                    "procedure"
                } else {
                    "fact"
                }
                .into(),
                tags: vec![],
            })
            .collect(),
    }
}

/// Serialize a vault document for writing (pretty, stable field order is
/// serde's declaration order — deterministic for the same doc).
pub fn to_json(doc: &VaultDoc) -> crate::Result<String> {
    Ok(serde_json::to_string_pretty(doc)?)
}

/// Default path under a project root: `<root>/.marfim/vault.json`.
pub fn vault_path(root: &Path) -> PathBuf {
    root.join(".marfim").join("vault.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> String {
        r#"{
  "version": "ecc.memory.v1",
  "entries": [
    {"id": "e1", "content": "User prefers tabs", "type": "preference", "tags": ["style"]},
    {"id": "e2", "content": "Deploy runs via scripts/deploy.sh", "type": "procedure"}
  ]
}"#
        .to_string()
    }

    #[test]
    fn imports_valid_doc() {
        let doc = import(&sample()).unwrap();
        assert_eq!(doc.version, "ecc.memory.v1");
        assert_eq!(doc.entries.len(), 2);
        assert_eq!(doc.entries[0].tags, vec!["style"]);
        assert_eq!(doc.entries[1].kind, "procedure");
    }

    #[test]
    fn wrong_version_fails_closed() {
        let bad = sample().replace("ecc.memory.v1", "ecc.memory.v9");
        let err = import(&bad).unwrap_err();
        assert!(err.contains("unsupported vault version"), "{err}");
    }

    #[test]
    fn missing_version_fails_closed() {
        let bad = r#"{"entries": []}"#;
        assert!(import(bad).unwrap_err().contains("missing `version`"));
    }

    #[test]
    fn invalid_json_fails_closed() {
        assert!(import("{nope").unwrap_err().contains("not valid JSON"));
    }

    #[test]
    fn unknown_kind_fails_closed_with_index() {
        let bad = sample().replace("\"procedure\"", "\"weird\"");
        let err = import(&bad).unwrap_err();
        assert!(err.contains("entry 1"), "{err}");
        assert!(err.contains("unknown type"), "{err}");
    }

    #[test]
    fn oversized_entry_refused() {
        let big = "x".repeat(MAX_ENTRY_CHARS + 1);
        let bad = format!(
            r#"{{"version": "{VAULT_VERSION}", "entries": [{{"id":"a","content":"{big}","type":"fact"}}]}}"#
        );
        assert!(import(&bad).unwrap_err().contains("cap"));
    }

    #[test]
    fn oversized_doc_refused() {
        let big = "x".repeat(MAX_DOC_BYTES + 1);
        assert!(import(&big).unwrap_err().contains("exceeds"));
    }

    #[test]
    fn too_many_entries_refused() {
        let mut entries = String::new();
        for n in 0..(MAX_ENTRIES + 1) {
            entries.push_str(&format!(r#"{{"id":"e{n}","content":"c","type":"fact"}},"#));
        }
        entries.pop();
        let bad = format!(r#"{{"version":"{VAULT_VERSION}","entries":[{entries}]}}"#);
        assert!(import(&bad).unwrap_err().contains("cap is"));
    }

    #[test]
    fn control_chars_stripped_from_content() {
        let doc_in = serde_json::json!({
            "version": VAULT_VERSION,
            "entries": [
                {"id": "a", "content": "ok\u{0000}now".to_string(), "type": "fact"}
            ]
        });
        let doc = import(&doc_in.to_string()).unwrap();
        assert_eq!(doc.entries[0].content, "oknow");
    }

    #[test]
    fn export_import_round_trip() {
        let mut m = crate::memory::MemoryStore::default();
        m.remember("likes rust", false);
        m.remember("never push to main", true);
        let doc = export(&m.entries);
        assert_eq!(doc.entries[1].kind, "procedure");
        let json = to_json(&doc).unwrap();
        let back = import(&json).unwrap();
        assert_eq!(back.entries.len(), 2);
        assert_eq!(back.entries[0].content, "likes rust");
        assert_eq!(back.entries[1].content, "never push to main");
    }

    #[test]
    fn empty_entries_accepted() {
        let doc = import(r#"{"version":"ecc.memory.v1","entries":[]}"#).unwrap();
        assert!(doc.entries.is_empty());
    }
}
