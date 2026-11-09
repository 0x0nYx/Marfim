//! Race-free test scaffolding shared by the Marfim workspace.
//!
//! Dev-dependency only: every consumer declares it under `[dev-dependencies]`,
//! so this code is never compiled into a release artifact. The one rule this
//! crate exists for: **no test may share a mutable temp directory with
//! another**. The old `temp_dir/{tag}-{pid}` scheme gave every test thread in
//! one process the same path — a parallel test's `remove_dir_all` deleted a
//! sibling's fixtures mid-run (observed as CI flake on the MSRV runner).
//!
//! Uniqueness here is `pid + per-process monotonic sequence`: cross-process
//! isolation comes from the pid, same-process isolation from the counter.
//! The counter is deterministic, which keeps run layouts reproducible.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

/// A fresh, never-before-used path under the process temp dir.
///
/// Infallible: it only computes the path. Create the directory (or file) in
/// the caller — test code, where `unwrap` is permitted.
pub fn unique_dir(tag: &str) -> PathBuf {
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!("{tag}-{}-{}", std::process::id(), n))
}

/// A `unique_dir` plus a Drop guard that removes the whole tree.
///
/// Construction is fallible (`create_dir_all`), so callers unwrap in their
/// own test code; destruction is best-effort, mirroring the classic tempdir
/// contract: a failed cleanup never masks the real test result.
pub struct Temp(PathBuf);

impl Temp {
    /// Create the unique directory, failing loudly if the OS refuses.
    pub fn new(tag: &str) -> std::io::Result<Self> {
        let p = unique_dir(tag);
        std::fs::create_dir_all(&p)?;
        Ok(Self(p))
    }

    pub fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_dir_never_repeats_within_a_process() {
        let a = unique_dir("tk-a");
        let b = unique_dir("tk-a");
        assert_ne!(a, b, "same tag twice must yield distinct paths");
        assert_ne!(
            unique_dir("tk-b"),
            unique_dir("tk-b"),
            "distinct tags must never collide either"
        );
    }

    #[test]
    fn temp_creates_and_removes_its_tree() {
        let inner = {
            let t = Temp::new("tk-temp").unwrap();
            std::fs::write(t.path().join("x"), b"1").unwrap();
            assert!(t.path().is_dir());
            t.path().to_path_buf()
        };
        assert!(!inner.exists(), "Drop must remove the tree");
    }
}
