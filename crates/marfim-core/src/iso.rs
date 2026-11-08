//! W5.8 CoW session-isolation ladder (distilled: OMP `pi-iso`, clean-room).
//!
//! When a subagent needs an isolated tree and the project is NOT a git
//! repo (no worktree available), the copy ladder tries the cheapest
//! kernel-backed duplication first and degrades honestly:
//!
//! 1. **Reflink** — `cp --reflink=always` (APFS clonefile on macOS,
//!    FICLONE on btrfs/XFS/ZFS). The ioctl rides userspace `cp`, so this
//!    crate stays `forbid(unsafe_code)`. Probed with a small temp file.
//! 2. **Bounded rcopy** — the plain recursive copy (with the W1.5 file
//!    cap; a runaway copy is worse than a refusal).
//!
//! `git worktree` remains the repo path and is NOT part of this ladder
//! (it needs the GitBackend, which the caller already owns).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsoBackend {
    /// Kernel CoW reflink (clonefile/FICLONE) via `cp --reflink=always`.
    Reflink,
    /// Bounded recursive copy (last rung, always available).
    Rcopy,
}

impl IsoBackend {
    pub fn name(&self) -> &'static str {
        match self {
            IsoBackend::Reflink => "reflink",
            IsoBackend::Rcopy => "rcopy",
        }
    }
}

/// Can `cp --reflink=always` duplicate on this filesystem? Probed on a
/// throwaway temp file (never the workspace): availability is a property
/// of the FS pair, not of the data.
fn reflink_available() -> bool {
    let dir = std::env::temp_dir().join(format!(
        "marfim-iso-probe-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let a = dir.join("a");
    let b = dir.join("b");
    let ok = std::fs::write(&a, b"marfim-iso-probe").is_ok()
        && std::process::Command::new("cp")
            .arg("--reflink=always")
            .arg(&a)
            .arg(&b)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
    let _ = std::fs::remove_dir_all(&dir);
    ok
}

/// Copy `src` → `dst` through the ladder. Returns the backend that ran
/// and the file count (the caller surfaces both — honesty about what
/// actually happened). The file cap lives in the rcopy rung; reflink is
/// O(1) per file at the kernel level and skipped from that accounting.
pub fn isolate_tree(
    src: &std::path::Path,
    dst: &std::path::Path,
) -> Result<(IsoBackend, usize), String> {
    if !src.is_dir() {
        return Err(format!(
            "isolation source is not a directory: {}",
            src.display()
        ));
    }
    std::fs::create_dir_all(dst).map_err(|e| format!("mkdir {dst:?}: {e}"))?;
    if reflink_available() {
        let out = std::process::Command::new("cp")
            .arg("-a")
            .arg("--reflink=always")
            .arg(src)
            .arg(dst)
            .output()
            .map_err(|e| format!("cp spawn: {e}"))?;
        if out.status.success() {
            return Ok((IsoBackend::Reflink, 0));
        }
        // fs said yes for the probe but no for the tree (e.g. mixed
        // mounts) — fall through to rcopy, honestly.
    }
    let n = copy_bounded(src, dst)?;
    Ok((IsoBackend::Rcopy, n))
}

/// Bounded recursive copy (`.git` skipped, 50k file cap) — extracted
/// from the subagent module so the ladder owns the last rung.
pub fn copy_bounded(src: &std::path::Path, dst: &std::path::Path) -> Result<usize, String> {
    const FILE_CAP: usize = 50_000;
    fn walk(src: &std::path::Path, dst: &std::path::Path, count: &mut usize) -> Result<(), String> {
        for entry in
            std::fs::read_dir(src).map_err(|e| format!("copy read {}: {e}", src.display()))?
        {
            let entry = entry.map_err(|e| format!("copy entry: {e}"))?;
            let name = entry.file_name();
            if name == ".git" {
                continue; // history stays behind; the snapshot is the tree
            }
            let from = entry.path();
            let to = dst.join(&name);
            if from.is_dir() {
                std::fs::create_dir_all(&to).map_err(|e| format!("mkdir {to:?}: {e}"))?;
                walk(&from, &to, count)?;
            } else {
                *count += 1;
                if *count > FILE_CAP {
                    return Err(format!(
                        "copy isolation refused: workspace exceeds {FILE_CAP} files"
                    ));
                }
                std::fs::copy(&from, &to)
                    .map_err(|e| format!("copy {} → {}: {e}", from.display(), to.display()))?;
            }
        }
        Ok(())
    }
    let mut count = 0;
    std::fs::create_dir_all(dst).map_err(|e| format!("mkdir {dst:?}: {e}"))?;
    walk(src, dst, &mut count)?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ladder_copies_a_tree_and_names_the_backend() {
        let src = marfim_testkit::Temp::new("marfim-iso-src").unwrap();
        std::fs::write(src.path().join("a.txt"), "one").unwrap();
        std::fs::create_dir_all(src.path().join("sub")).unwrap();
        std::fs::write(src.path().join("sub/b.txt"), "two").unwrap();
        std::fs::create_dir_all(src.path().join(".git")).unwrap();
        std::fs::write(src.path().join(".git/config"), "stays behind").unwrap();

        let dst = marfim_testkit::unique_dir("marfim-iso-dst");
        let (backend, n) = isolate_tree(src.path(), &dst).expect("ladder runs");
        assert!(
            backend == IsoBackend::Reflink || backend == IsoBackend::Rcopy,
            "honest backend: {backend:?}"
        );
        assert_eq!(std::fs::read_to_string(dst.join("a.txt")).unwrap(), "one");
        assert_eq!(
            std::fs::read_to_string(dst.join("sub/b.txt")).unwrap(),
            "two"
        );
        assert!(!dst.join(".git").exists(), ".git skipped on the rcopy rung");
        if backend == IsoBackend::Rcopy {
            assert_eq!(n, 2, "rcopy counts files");
        }
        let _ = std::fs::remove_dir_all(&dst);
    }

    #[test]
    fn non_directory_source_refuses() {
        let dst = marfim_testkit::unique_dir("marfim-iso-bad");
        let err = isolate_tree(std::path::Path::new("/definitely/not/here"), &dst).unwrap_err();
        assert!(err.contains("not a directory"), "{err}");
    }
}
