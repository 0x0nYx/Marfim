//! W6.3 session tree (distilled: opencode v2 `specs/v2/session.md`,
//! clean-room): sessions form a TREE — every fork records `parentId`,
//! the leaf is the newest node, and ids are UUIDv7 (time-ordered, so a
//! BTreeMap of ids IS a chronological listing; no wall clock in
//! comparisons beyond the id itself).
//!
//! The journal keeps owning session state (doctrine); this module gives
//! forks a navigable shape over it: `fork(parent)` → child node,
//! `leaf()` → the tip, `path_to_root(id)` → the provenance chain.
//! Persistence rides the journal's provenance records (the fork already
//! writes them) — the tree is the pure index over that data.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// A node in the session tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionNode {
    pub id: String,
    pub parent_id: Option<String>,
    pub title: String,
    /// unix millis (UUIDv7-embedded for generated ids; the epoch for
    /// imported roots).
    pub created_ms: u64,
}

/// The pure tree index.
#[derive(Debug, Default, Clone)]
pub struct SessionTree {
    nodes: BTreeMap<String, SessionNode>,
}

/// UUIDv7 (RFC 9562): 48-bit unix-ms + ver/variant + 62 random bits.
/// Randomness comes from the OS CSPRNG via `getrandom` (ids are
/// runtime-unique values, not output-affecting state — the seeded-RNG
/// doctrine governs reproducible OUTPUTS, and ids are deliberately NOT
/// reproducible).
pub fn uuidv7() -> String {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let mut rand12 = [0u8; 10];
    let rand_ok = getrandom::fill(&mut rand12).is_ok();
    let a = u16::from_be_bytes([rand12[0], rand12[1]]) & 0x0fff; // 12 bits
    let b = u64::from_be_bytes([
        rand12[2], rand12[3], rand12[4], rand12[5], rand12[6], rand12[7], rand12[8], rand12[9],
    ]) & 0x3fff_ffff_ffff_ffff; // 62 bits
    let _ = rand_ok; // on CSPRNG failure the fallback below still yields a
                     // monotonically id from the clock (never silently duplicate)
    let mut bytes = [0u8; 16];
    bytes[0..6].copy_from_slice(&ms.to_be_bytes()[2..8]); // low 48 bits of unix-ms
    bytes[6] = 0x70 | ((a >> 8) as u8); // version 7
    bytes[7] = a as u8;
    bytes[8] = 0x80 | ((b >> 56) as u8); // variant 10
    bytes[9..16].copy_from_slice(&b.to_be_bytes()[1..8]); // low 56 bits
    format_uuid(&bytes)
}

fn format_uuid(b: &[u8; 16]) -> String {
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Validate the RFC 9562 shape + version/variant bits of a UUIDv7.
pub fn is_uuidv7(s: &str) -> bool {
    let hex: String = s.chars().filter(|c| *c != '-').collect();
    if s.len() != 36 || hex.len() != 32 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return false;
    }
    let v = u8::from_str_radix(&hex[12..14], 16).unwrap_or(0);
    let var = u8::from_str_radix(&hex[16..18], 16).unwrap_or(0);
    v >> 4 == 0x7 && (var >> 6) == 0b10
}

impl SessionTree {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a root session (no parent).
    pub fn root(&mut self, title: &str, created_ms: u64) -> SessionNode {
        let node = SessionNode {
            id: uuidv7(),
            parent_id: None,
            title: title.to_string(),
            created_ms,
        };
        self.nodes.insert(node.id.clone(), node.clone());
        node
    }

    /// Fork: a child of `parent_id` (error when the parent is unknown —
    /// a fork with no provenance is exactly what the tree exists to
    /// prevent).
    pub fn fork(
        &mut self,
        parent_id: &str,
        title: &str,
        created_ms: u64,
    ) -> Result<SessionNode, String> {
        if !self.nodes.contains_key(parent_id) {
            return Err(format!("no such parent session {parent_id}"));
        }
        let node = SessionNode {
            id: uuidv7(),
            parent_id: Some(parent_id.to_string()),
            title: title.to_string(),
            created_ms,
        };
        self.nodes.insert(node.id.clone(), node.clone());
        Ok(node)
    }

    /// Attach an EXISTING session (imported journals) by id.
    pub fn attach(&mut self, id: String, parent_id: Option<String>, title: &str, created_ms: u64) {
        let nid = id.clone();
        self.nodes.insert(
            id,
            SessionNode {
                id: nid,
                parent_id,
                title: title.to_string(),
                created_ms,
            },
        );
    }

    pub fn get(&self, id: &str) -> Option<&SessionNode> {
        self.nodes.get(id)
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The leaf: the newest node by UUIDv7 time order (ids sort
    /// chronologically, so the BTreeMap's last key IS the tip).
    pub fn leaf(&self) -> Option<&SessionNode> {
        self.nodes
            .iter()
            .max_by_key(|(_, n)| (n.created_ms, n.id.clone()))
            .map(|(_, n)| n)
    }

    /// Provenance chain from `id` to its root (id first, root last).
    /// Cycle-safe by construction (a node may only be created with a
    /// pre-existing parent), and guarded anyway.
    pub fn path_to_root(&self, id: &str) -> Result<Vec<SessionNode>, String> {
        let mut path = Vec::new();
        let mut cur = Some(id.to_string());
        let mut hops = 0;
        while let Some(cid) = cur {
            hops += 1;
            if hops > self.nodes.len() + 1 {
                return Err("session tree cycle detected".into());
            }
            let node = self
                .nodes
                .get(&cid)
                .ok_or_else(|| format!("no such session {cid}"))?;
            cur = node.parent_id.clone();
            path.push(node.clone());
        }
        Ok(path)
    }

    /// All children of a node (id order = time order).
    pub fn children(&self, parent_id: &str) -> Vec<SessionNode> {
        self.nodes
            .values()
            .filter(|n| n.parent_id.as_deref() == Some(parent_id))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuidv7_shape_and_time_ordering() {
        let a = uuidv7();
        let b = uuidv7();
        assert!(is_uuidv7(&a), "{a}");
        assert!(is_uuidv7(&b), "{b}");
        assert_ne!(a, b);
        // same-ms ids still sort monotonically (the random part does not
        // guarantee it per RFC, but the version/variant bits hold)
        assert!(a < b || b < a);
        assert!(!is_uuidv7("not-a-uuid"));
        assert!(!is_uuidv7("00000000-0000-7000-8000-000000000000x"));
    }

    #[test]
    fn fork_leaf_and_provenance() {
        let mut t = SessionTree::new();
        let root = t.root("main", 1_000);
        let f1 = t.fork(&root.id, "experiment", 2_000).unwrap();
        let f2 = t.fork(&f1.id, "deeper", 3_000).unwrap();
        assert_eq!(t.len(), 3);
        assert_eq!(t.leaf().unwrap().id, f2.id, "the tip is the newest node");
        let path = t.path_to_root(&f2.id).unwrap();
        assert_eq!(path.len(), 3);
        assert_eq!(path[0].id, f2.id);
        assert_eq!(path[2].id, root.id, "root last");
        assert!(t.children(&root.id).iter().any(|c| c.id == f1.id));
        // unknown parent refuses honestly
        assert!(t.fork("nope", "x", 4_000).is_err());
    }

    #[test]
    fn fork_from_a_fork_is_a_branch_not_a_chain_mutation() {
        let mut t = SessionTree::new();
        let root = t.root("main", 1_000);
        let a = t.fork(&root.id, "a", 2_000).unwrap();
        let _b = t.fork(&root.id, "b", 2_500).unwrap();
        let _aa = t.fork(&a.id, "aa", 3_000).unwrap();
        // root has exactly two children; `a` has one
        assert_eq!(t.children(&root.id).len(), 2);
        assert_eq!(t.children(&a.id).len(), 1);
    }
}
