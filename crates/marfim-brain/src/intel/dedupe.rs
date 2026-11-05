//! Port of oracle `src/intel/dedupe.js` — duplicate detection with a
//! three-stage funnel + TTL quarantine.
//!
//! Duplicate keys (three-stage funnel): stage 1 size bucket (min_size
//! filter, symlinks excluded, unreadable entries honestly skipped) ->
//! stage 2 sha256 of the first 4096 bytes keyed (size, partial) -> stage 3
//! full-file sha256 decides. Groups are {size, wasted: size*(n-1), hash,
//! paths} sorted by (-wasted, hash); members sorted.
//!
//! PORT NOTES / NARROWINGS (differential-testable core kept, disk layer
//! virtualized):
//! - The oracle walks the real filesystem with `lstatSync`/`readdirSync`
//!   and hashes file contents. This port runs the IDENTICAL funnel over a
//!   caller-supplied virtual filesystem ([`Vfs`]: path -> file/dir/symlink
//!   with contents). Fixture replay passes the same relative paths and
//!   byte contents the oracle ran on, so results match group for group.
//!   `resolve()` has no cwd in this model; roots are used as virtual keys
//!   verbatim (fixture roots are canonical relative paths).
//! - `survivalScores` reads `statSync(p).atimeMs`; atime is machine state,
//!   so the fixtures record the ACTUAL atimeMs (pinned via utimesSync) and
//!   the port takes the atime table as input. The score formula, the
//!   4-decimal Python half-even rounding and the 0.0-for-unreadable
//!   contract are ported verbatim.
//! - `quarantine`/`restore` are file-backed (renameSync + manifest.json
//!   journal) in the oracle. The port implements them over an in-memory
//!   [`QuarantineStore`] (BTreeMap nodes + per-dir manifest vec) with the
//!   same journal semantics (entries sorted by (quarantined_at, file) when
//!   observed); the oracle's DISK persistence is a Phase-4-later
//!   integration and is intentionally not here.
//! - Error messages: 'not a regular file' and 'no manifest' are oracle
//!   constants and are byte-exact. OS-rename error strings (exc.code:
//!   exc.message) are platform text and are NOT exercised by fixtures; the
//!   in-memory restore only errors when the original path is occupied by a
//!   directory (same keep-entry semantics).
//! - The funnel's stage iteration order differs (BTreeMap vs JS Map
//!   insertion), but the output is provably order-independent: members are
//!   sorted, groups are totally sorted by (-wasted, hash), and hashes are
//!   unique per group.

use std::collections::BTreeMap;

use crate::intel::dp::sha256_hex;

/// sha256 of the first `PARTIAL_BYTES` bytes of a file (stage 2 key).
pub const PARTIAL_BYTES: usize = 4096;
/// Oracle DAY_SECONDS — the TTL/survival time unit.
pub const DAY_SECONDS: f64 = 86400.0;

/// A node of the virtual filesystem.
#[derive(Debug, Clone, PartialEq)]
pub enum FsNode {
    /// Regular file with its byte content.
    File(Vec<u8>),
    /// Directory.
    Dir,
    /// Symbolic link (excluded by the funnel like the oracle's lstat).
    Symlink,
}

/// Virtual filesystem: path -> node (paths are the fixture's canonical
/// relative keys, e.g. "root/a.txt").
pub type Vfs = BTreeMap<String, FsNode>;

/// One duplicate group ({size, wasted, hash, paths} in the oracle).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct DupGroup {
    pub size: u64,
    pub wasted: u64,
    pub hash: String,
    pub paths: Vec<String>,
}

/// sha256 of the first `PARTIAL_BYTES` bytes (oracle partialHash).
fn partial_hash(content: &[u8]) -> String {
    let end = content.len().min(PARTIAL_BYTES);
    sha256_hex(&content[..end])
}

/// Walk one root (file or directory tree, symlinks excluded) to regular
/// files, depth-first with per-directory name order (BTreeMap key order ==
/// sorted names, same as the oracle's `readdirSync().sort()`). A root with
/// no node but with existing descendants is walked as a directory (the
/// oracle's filesystem materializes every ancestor; virtual callers may
/// only record leaves).
fn walk_files(vfs: &Vfs, root: &str, out: &mut Vec<String>) {
    let node = vfs.get(root);
    match node {
        Some(FsNode::Symlink) => return,
        Some(FsNode::File(_)) => {
            out.push(root.to_string());
            return;
        }
        _ => {} // Dir, or missing-but-with-descendants (see below)
    }
    let prefix = format!("{root}/");
    let children: Vec<String> = vfs
        .keys()
        .filter(|p| p.starts_with(&prefix) && !p[prefix.len()..].contains('/'))
        .cloned()
        .collect();
    for child in children {
        walk_files(vfs, &child, out);
    }
}

/// Duplicate groups under `roots`. Stage 1 buckets by size, stage 2 by
/// (size, sha256 of the first 4096 bytes), stage 3 full sha256 decides.
/// Returns groups sorted by wasted desc, hash asc — identical input,
/// identical output.
pub fn scan_duplicates(vfs: &Vfs, roots: &[&str], min_size: u64) -> Vec<DupGroup> {
    // stage 1: size bucket
    let mut by_size: BTreeMap<u64, Vec<String>> = BTreeMap::new();
    for root in roots {
        let mut files: Vec<String> = Vec::new();
        walk_files(vfs, root, &mut files);
        for path in files {
            let Some(FsNode::File(content)) = vfs.get(&path) else {
                continue; // symlinks/dirs are excluded by the walk
            };
            let size = content.len() as u64;
            if size >= min_size {
                by_size.entry(size).or_default().push(path);
            }
        }
    }
    // stage 2: partial hash of the first 4096 bytes
    let mut by_partial: BTreeMap<(u64, String), Vec<String>> = BTreeMap::new();
    for (size, paths) in &by_size {
        if paths.len() < 2 {
            continue;
        }
        for p in paths {
            let Some(FsNode::File(content)) = vfs.get(p) else {
                continue;
            };
            let head = partial_hash(content);
            by_partial.entry((*size, head)).or_default().push(p.clone());
        }
    }
    // stage 3: full hash decides. The oracle visits partial keys by
    // (-size, hash); BTreeMap order differs, but every observable is fixed
    // by the final (-wasted, hash) sort, so the visit order is irrelevant.
    let mut groups: Vec<DupGroup> = Vec::new();
    let keys: Vec<(u64, String)> = by_partial.keys().cloned().collect();
    for key in &keys {
        let paths = &by_partial[key];
        if paths.len() < 2 {
            continue;
        }
        let size = key.0;
        let mut by_full: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for p in paths {
            let Some(FsNode::File(content)) = vfs.get(p) else {
                continue;
            };
            let digest = sha256_hex(content);
            by_full.entry(digest).or_default().push(p.clone());
        }
        for (digest, members) in by_full {
            if members.len() > 1 {
                let mut sorted = members;
                sorted.sort();
                groups.push(DupGroup {
                    size,
                    wasted: size * (sorted.len() as u64 - 1),
                    hash: digest,
                    paths: sorted,
                });
            }
        }
    }
    // oracle: wasted desc, hash asc
    groups.sort_by(|a, b| b.wasted.cmp(&a.wasted).then_with(|| a.hash.cmp(&b.hash)));
    groups
}

/// Last-access-based survival score in [0,1]: 1.0 for entries accessed
/// within a day, decaying by half every 30 days after that. Honest about
/// atime-unreliability: the caller's report should carry that this is a
/// PRIOR, not a verdict. Unreadable (None) => 0.0.
///
/// `entries` pairs each path with the atime in milliseconds (the oracle's
/// `statSync(p).atimeMs`) or None when the stat would throw.
pub fn survival_scores(entries: &[(String, Option<f64>)], now: f64) -> BTreeMap<String, f64> {
    let mut scores = BTreeMap::new();
    for (p, atime_ms) in entries {
        let score = match atime_ms {
            None => 0.0,
            Some(at) => {
                let age_days = ((now - at / 1000.0) / DAY_SECONDS).max(0.0);
                if age_days <= 1.0 {
                    1.0
                } else {
                    py_round_local(0.5f64.powf((age_days - 1.0) / 30.0), 4)
                }
            }
        };
        scores.insert(p.clone(), score);
    }
    scores
}

/// Python round() semantics for the one 4-decimal site (half-to-even),
/// ported verbatim from the oracle's pyRoundLocal float implementation.
fn py_round_local(x: f64, digits: i32) -> f64 {
    let scale = 10f64.powi(digits);
    let scaled = x * scale;
    let floor = scaled.floor();
    let diff = scaled - floor;
    // half-to-even: round up on > 0.5 or on an exact 0.5 tie with an odd floor
    let round_up = diff > 0.5 || (diff == 0.5 && floor % 2.0 != 0.0);
    let q = if round_up { floor + 1.0 } else { floor };
    q / scale
}

/// One manifest journal entry ({original, quarantined_at, purge_after,
/// file} in the oracle).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ManifestEntry {
    pub original: String,
    pub quarantined_at: f64,
    pub purge_after: f64,
    pub file: String,
}

/// A quarantine/restore error row ({path?, error} in the oracle; the
/// no-manifest error carries no path).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct QuarantineError {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub error: String,
}

/// quarantine() outcome ({moved, errors, manifest} — the oracle returns the
/// manifest PATH; this in-memory store returns the final sorted journal
/// entries instead, which is what the manifest file would contain).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct QuarantineOutcome {
    pub moved: Vec<String>,
    pub errors: Vec<QuarantineError>,
    pub manifest: Vec<ManifestEntry>,
}

/// restore() outcome ({restored, errors} plus the final journal).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct RestoreOutcome {
    pub restored: Vec<String>,
    pub errors: Vec<QuarantineError>,
    pub manifest: Vec<ManifestEntry>,
}

/// In-memory TTL quarantine store — the virtual-disk twin of the oracle's
/// renameSync + manifest.json journal. The oracle's disk persistence is a
/// Phase-4-later integration; this state lives only in the process.
#[derive(Debug, Clone, Default)]
pub struct QuarantineStore {
    /// The whole virtual filesystem (originals + quarantine dirs).
    pub fs: Vfs,
    /// qdir -> journal entries, in journal order (unsorted, like the
    /// in-memory manifest before the oracle's write-time sort).
    manifests: BTreeMap<String, Vec<ManifestEntry>>,
}

fn basename(p: &str) -> String {
    // node path.basename: the text after the final '/'
    p.rsplit('/').next().unwrap_or(p).to_string()
}

fn manifest_sorted(entries: &[ManifestEntry]) -> Vec<ManifestEntry> {
    // oracle writeManifest: sort by (quarantined_at, file)
    let mut out = entries.to_vec();
    out.sort_by(|a, b| {
        a.quarantined_at
            .partial_cmp(&b.quarantined_at)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.file.cmp(&b.file))
    });
    out
}

impl QuarantineStore {
    pub fn new(fs: Vfs, manifests: BTreeMap<String, Vec<ManifestEntry>>) -> Self {
        Self { fs, manifests }
    }

    /// The final on-disk-equivalent manifest for a quarantine dir (the
    /// oracle sorts entries when writing manifest.json).
    pub fn manifest(&self, qdir: &str) -> Vec<ManifestEntry> {
        manifest_sorted(self.manifests.get(qdir).map(Vec::as_slice).unwrap_or(&[]))
    }

    /// TTL quarantine: move files into the quarantine dir with a journal
    /// entry recording origin + expiry; restore() undoes it; a purge after
    /// the TTL is a SEPARATE explicit action. Journaled by construction.
    pub fn quarantine(
        &mut self,
        paths: &[&str],
        qdir: &str,
        now: f64,
        ttl_days: f64,
    ) -> QuarantineOutcome {
        // mkdirSync(qdir, {recursive: true})
        self.fs.entry(qdir.to_string()).or_insert(FsNode::Dir);
        let mut moved: Vec<String> = Vec::new();
        let mut errors: Vec<QuarantineError> = Vec::new();
        for p in paths {
            let src = p.to_string();
            let is_regular = matches!(self.fs.get(&src), Some(FsNode::File(_)));
            if !is_regular {
                errors.push(QuarantineError {
                    path: Some(src),
                    error: "not a regular file".to_string(),
                });
                continue;
            }
            let dest = format!("{}/{}-{}", qdir, js_trunc(now * 1000.0), basename(&src));
            // renameSync(src, dest) — POSIX rename moves the node
            let node = self.fs.remove(&src);
            self.fs
                .insert(dest.clone(), node.unwrap_or(FsNode::File(Vec::new())));
            self.manifests
                .entry(qdir.to_string())
                .or_default()
                .push(ManifestEntry {
                    original: src,
                    quarantined_at: now,
                    purge_after: now + ttl_days * DAY_SECONDS,
                    file: basename(&dest),
                });
            moved.push(dest);
        }
        QuarantineOutcome {
            moved,
            errors,
            manifest: self.manifest(qdir),
        }
    }

    /// Undo every quarantine entry whose TTL has expired: entries with
    /// purge_after <= now are moved back to their original path; unexpired
    /// entries stay quarantined. Purging expired files from the quarantine
    /// dir is a separate explicit action and is never done here. An entry
    /// whose quarantined file no longer exists still reports the original
    /// as restored (verbatim oracle behavior).
    pub fn restore(&mut self, qdir: &str, now: f64) -> RestoreOutcome {
        let Some(entries) = self.manifests.get(qdir).cloned() else {
            return RestoreOutcome {
                restored: Vec::new(),
                errors: vec![QuarantineError {
                    path: None,
                    error: "no manifest".to_string(),
                }],
                manifest: Vec::new(),
            };
        };
        let mut restored: Vec<String> = Vec::new();
        let mut errors: Vec<QuarantineError> = Vec::new();
        let mut keep: Vec<ManifestEntry> = Vec::new();
        for entry in entries {
            if entry.purge_after > now {
                keep.push(entry); // TTL not yet reached: leave quarantined
                continue;
            }
            let dest = format!("{qdir}/{}", entry.file);
            let src = entry.original.clone();
            // renameSync(dest, src): fails only when the target is an
            // existing directory (a file target is replaced, POSIX semantics)
            let target_is_dir = matches!(self.fs.get(&src), Some(FsNode::Dir));
            if target_is_dir {
                errors.push(QuarantineError {
                    path: Some(src),
                    error: "EISDIR: illegal operation on a directory, rename".to_string(),
                });
                keep.push(entry);
                continue;
            }
            if self.fs.contains_key(&dest) {
                let node = self.fs.remove(&dest);
                self.fs
                    .insert(src.clone(), node.unwrap_or(FsNode::File(Vec::new())));
            }
            restored.push(src);
        }
        self.manifests.insert(qdir.to_string(), keep);
        RestoreOutcome {
            restored,
            errors,
            manifest: self.manifest(qdir),
        }
    }
}

/// JS Math.trunc for the quarantine filename prefix (toward zero).
fn js_trunc(x: f64) -> i64 {
    x.trunc() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(bytes: &[u8]) -> FsNode {
        FsNode::File(bytes.to_vec())
    }

    #[test]
    fn funnel_groups_identical_files_and_splits_prefix_twins() {
        let mut vfs: Vfs = BTreeMap::new();
        vfs.insert("root/a.txt".into(), file(b"hello world"));
        vfs.insert("root/b.txt".into(), file(b"hello world"));
        vfs.insert("root/c.txt".into(), file(b"other stuff"));
        let groups = scan_duplicates(&vfs, &["root"], 1);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].size, 11);
        assert_eq!(groups[0].wasted, 11);
        assert_eq!(groups[0].paths, vec!["root/a.txt", "root/b.txt"]);
        // same first 4096 bytes, different tails -> no group
        let mut big1: Vec<u8> = vec![7u8; 4096 + 64];
        let mut big2: Vec<u8> = vec![7u8; 4096 + 64];
        big1[4096] = 1;
        big2[4096] = 2;
        let mut vfs2: Vfs = BTreeMap::new();
        vfs2.insert("root/x1.bin".into(), file(&big1));
        vfs2.insert("root/x2.bin".into(), file(&big2));
        assert!(scan_duplicates(&vfs2, &["root"], 1).is_empty());
        // identical large files DO group, wasted = size*(n-1)
        let mut vfs3: Vfs = BTreeMap::new();
        vfs3.insert("root/x1.bin".into(), file(&big1));
        vfs3.insert("root/x2.bin".into(), file(&big1));
        let g3 = scan_duplicates(&vfs3, &["root"], 1);
        assert_eq!(g3.len(), 1);
        assert_eq!(g3[0].wasted, big1.len() as u64);
    }

    #[test]
    fn symlinks_and_min_size_are_excluded() {
        let mut vfs: Vfs = BTreeMap::new();
        vfs.insert("root/real.txt".into(), file(b"data"));
        vfs.insert("root/link.txt".into(), FsNode::Symlink);
        vfs.insert("root/small.txt".into(), file(b"tiny"));
        vfs.insert("root/dir".into(), FsNode::Dir);
        assert!(scan_duplicates(&vfs, &["root"], 1).is_empty());
        assert!(scan_duplicates(&vfs, &["root"], 100).is_empty());
        assert!(scan_duplicates(&vfs, &["missing"], 1).is_empty());
    }

    #[test]
    #[allow(clippy::approx_constant)] // the oracle's pinned 4-decimal outputs
    fn survival_decay_and_rounding() {
        let now = 1700000000.0;
        let day = DAY_SECONDS;
        let entries = vec![
            ("fresh".to_string(), Some((now - 3600.0) * 1000.0)),
            ("day1".to_string(), Some((now - day) * 1000.0)),
            ("m31".to_string(), Some((now - 31.0 * day) * 1000.0)),
            ("m16".to_string(), Some((now - 16.0 * day) * 1000.0)),
            ("gone".to_string(), None),
        ];
        let scores = survival_scores(&entries, now);
        assert_eq!(scores["fresh"], 1.0);
        assert_eq!(scores["day1"], 1.0);
        assert_eq!(scores["m31"], 0.5);
        assert_eq!(scores["m16"], 0.7071);
        assert_eq!(scores["gone"], 0.0);
    }

    #[test]
    fn quarantine_and_restore_roundtrip_in_memory() {
        let mut vfs: Vfs = BTreeMap::new();
        vfs.insert("src/a.txt".into(), file(b"A"));
        vfs.insert("src/b.txt".into(), file(b"B"));
        vfs.insert("src/sub".into(), FsNode::Dir);
        let mut store = QuarantineStore::new(vfs, BTreeMap::new());
        let now = 1700000000.0;
        let out = store.quarantine(
            &["src/a.txt", "src/b.txt", "src/sub", "nope"],
            "q",
            now,
            30.0,
        );
        assert_eq!(
            out.moved,
            vec!["q/1700000000000-a.txt", "q/1700000000000-b.txt"]
        );
        assert_eq!(out.errors.len(), 2);
        assert!(out.errors.iter().all(|e| e.error == "not a regular file"));
        assert_eq!(out.manifest.len(), 2);
        assert_eq!(out.manifest[0].purge_after, now + 30.0 * DAY_SECONDS);
        // expired entry comes back; unexpired stays
        let back = store.restore("q", now + 31.0 * DAY_SECONDS);
        assert_eq!(back.restored, vec!["src/a.txt", "src/b.txt"]);
        assert_eq!(back.manifest.len(), 0);
        assert!(matches!(store.fs.get("src/a.txt"), Some(FsNode::File(_))));
        // missing manifest
        let none = store.restore("q2", now);
        assert_eq!(none.restored, Vec::<String>::new());
        assert_eq!(none.errors[0].error, "no manifest");
    }

    #[test]
    #[allow(clippy::approx_constant)] // the oracle's pinned 4-decimal outputs
    fn py_round_local_half_even() {
        assert_eq!(py_round_local(0.7071067811865476, 4), 0.7071);
        assert_eq!(py_round_local(0.5, 0), 0.0);
        assert_eq!(py_round_local(1.5, 0), 2.0);
        assert_eq!(py_round_local(2.5, 0), 2.0);
    }
}
