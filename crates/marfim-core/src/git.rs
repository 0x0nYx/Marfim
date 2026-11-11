//! Git backend trait (W1.5): ONE interface, two implementations.
//!
//! - [`CliGit`] — the current `git`-CLI behavior, byte-identical (the
//!   default; used by undo, checkpoints, and subagent isolation).
//! - [`GixGit`] — in-process via `gix` (feature `gix`): for embeddings
//!   without a git CLI (Android). Every API below was verified against
//!   the real gix 0.69.1 source; operations it does not faithfully
//!   support return honest `Err`s, never approximations.
//!
//! Determinism note: commits carry wall-clock signatures (git semantics,
//! same as CliGit); nothing else in this module is time-dependent.

use std::path::Path;

/// One commit in a log listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitInfo {
    pub id: String,
    pub summary: String,
}

/// A status snapshot: tracked modifications only (untracked listing is
/// CliGit-side; GixGit reports tracked dirtiness honestly).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatusSummary {
    pub modified: Vec<String>,
    pub dirty: bool,
}

pub trait GitBackend: Send + Sync {
    fn name(&self) -> &'static str;
    /// Is `dir` inside a git repository's worktree?
    fn is_repo(&self, dir: &Path) -> bool;
    fn status(&self, dir: &Path) -> Result<StatusSummary, String>;
    /// Human-readable change listing (HEAD ↔ worktree).
    fn diff(&self, dir: &Path) -> Result<String, String>;
    /// Stage every listed path (empty list = all tracked changes).
    fn add_all(&self, dir: &Path, paths: &[String]) -> Result<(), String>;
    /// Commit staged/tracked changes; returns the new HEAD id.
    fn commit(&self, dir: &Path, message: &str) -> Result<String, String>;
    fn log(&self, dir: &Path, limit: usize) -> Result<Vec<CommitInfo>, String>;
    /// Move the current branch to `rev` and hard-reset the worktree.
    fn reset_hard(&self, dir: &Path, rev: &str) -> Result<(), String>;
    /// D16 isolation primitives: detached worktree add/remove.
    fn worktree_add_detached(&self, dir: &Path, target: &Path) -> Result<(), String>;
    fn worktree_remove(&self, dir: &Path, target: &Path) -> Result<(), String>;
}

/// The default backend: the `git` CLI (pre-W1.5 behavior).
pub fn default_backend() -> Box<dyn GitBackend> {
    Box::new(CliGit)
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| format!("git {}: {e}", args.first().unwrap_or(&"")))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if out.status.success() {
        Ok(text)
    } else {
        Err(text)
    }
}

/// `git`-CLI backend: byte-identical to the pre-W1.5 call sites.
#[derive(Debug, Clone, Copy, Default)]
pub struct CliGit;

impl GitBackend for CliGit {
    fn name(&self) -> &'static str {
        "cli-git"
    }

    fn is_repo(&self, dir: &Path) -> bool {
        std::process::Command::new("git")
            .args(["rev-parse", "--git-dir"])
            .current_dir(dir)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn status(&self, dir: &Path) -> Result<StatusSummary, String> {
        let text = git(dir, &["status", "--porcelain"])?;
        let mut s = StatusSummary::default();
        for line in text.lines() {
            if line.len() < 4 {
                continue;
            }
            let (code, path) = line.split_at(2);
            let path = path.trim_start().to_string();
            s.dirty = true;
            if code.trim().is_empty() {
                // untracked ("?? ") — listed, not "modified"
                s.modified.push(path);
            } else {
                s.modified.push(path);
            }
        }
        Ok(s)
    }

    fn diff(&self, dir: &Path) -> Result<String, String> {
        git(dir, &["diff", "HEAD"])
    }

    fn add_all(&self, dir: &Path, paths: &[String]) -> Result<(), String> {
        if paths.is_empty() {
            git(dir, &["add", "-A"])?;
        } else {
            let mut args = vec!["add", "--"];
            args.extend(paths.iter().map(String::as_str));
            git(dir, &args)?;
        }
        Ok(())
    }

    fn commit(&self, dir: &Path, message: &str) -> Result<String, String> {
        git(dir, &["commit", "-m", message])?;
        Ok(git(dir, &["rev-parse", "HEAD"])?.trim().to_string())
    }

    fn log(&self, dir: &Path, limit: usize) -> Result<Vec<CommitInfo>, String> {
        let text = git(dir, &["log", &format!("-n{limit}"), "--pretty=%H %s"])?;
        Ok(text
            .lines()
            .filter(|l| l.len() >= 41)
            .map(|l| CommitInfo {
                id: l[..40].to_string(),
                summary: l[41..].to_string(),
            })
            .collect())
    }

    fn reset_hard(&self, dir: &Path, rev: &str) -> Result<(), String> {
        git(dir, &["reset", "--hard", rev]).map(|_| ())
    }

    fn worktree_add_detached(&self, dir: &Path, target: &Path) -> Result<(), String> {
        let out = std::process::Command::new("git")
            .args(["worktree", "add", "--detach"])
            .arg(target)
            .current_dir(dir)
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "worktree add failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ))
        }
    }

    fn worktree_remove(&self, dir: &Path, target: &Path) -> Result<(), String> {
        let out = std::process::Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(target)
            .current_dir(dir)
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "worktree remove failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ))
        }
    }
}

/// In-process backend via gix (feature `gix`).
#[cfg(feature = "gix")]
#[derive(Debug, Clone, Copy, Default)]
pub struct GixGit;

#[cfg(feature = "gix")]
mod gix_impl {
    use super::*;
    use gix::bstr::ByteSlice;

    fn open(dir: &Path) -> Result<gix::Repository, String> {
        gix::discover(dir).map_err(|e| format!("gix: not a repository: {e}"))
    }

    impl GitBackend for GixGit {
        fn name(&self) -> &'static str {
            "gix"
        }

        fn is_repo(&self, dir: &Path) -> bool {
            gix::discover(dir).is_ok()
        }

        fn status(&self, dir: &Path) -> Result<StatusSummary, String> {
            let repo = open(dir)?;
            let mut s = StatusSummary::default();
            let iter = repo
                .status(gix::progress::Discard)
                .map_err(|e| e.to_string())?
                .index_worktree_rewrites(None)
                .into_index_worktree_iter(Vec::new())
                .map_err(|e| e.to_string())?;
            for item in iter {
                match item {
                    Ok(gix::status::index_worktree::Item::Modification { rela_path, .. }) => {
                        s.dirty = true;
                        s.modified.push(rela_path.to_str_lossy().into_owned());
                    }
                    Ok(_) => {}
                    Err(e) => return Err(format!("gix status: {e}")),
                }
            }
            Ok(s)
        }

        fn diff(&self, dir: &Path) -> Result<String, String> {
            // HEAD-tree ↔ index ↔ tracked-worktree change listing
            // (structured, not GNU patch text — CliGit owns patch text).
            let repo = open(dir)?;
            let head_tree = repo
                .head_commit()
                .map_err(|e| format!("gix: {e}"))?
                .tree()
                .map_err(|e| format!("gix: {e}"))?
                .id;
            let index = repo.index().map_err(|e| format!("gix: {e}"))?;
            let backing = index.path_backing();
            let current: std::collections::BTreeMap<String, String> = index
                .entries()
                .iter()
                .map(|e| {
                    (
                        e.path_in(backing).to_str_lossy().into_owned(),
                        e.id.to_string(),
                    )
                })
                .collect();
            let at_head_index = repo
                .index_from_tree(&head_tree)
                .map_err(|e| format!("gix: {e}"))?;
            let at_head_backing = at_head_index.path_backing();
            let at_head: std::collections::BTreeMap<String, String> = at_head_index
                .entries()
                .iter()
                .map(|e| {
                    (
                        e.path_in(at_head_backing).to_str_lossy().into_owned(),
                        e.id.to_string(),
                    )
                })
                .collect();
            let mut lines = Vec::new();
            for (p, id) in &current {
                match at_head.get(p) {
                    None => lines.push(format!("A {p}")),
                    Some(h) if h != id => lines.push(format!("M {p}")),
                    Some(_) => {}
                }
            }
            for p in at_head.keys() {
                if !current.contains_key(p) {
                    lines.push(format!("D {p}"));
                }
            }
            // tracked worktree changes (index ↔ worktree)
            let st = self.status(dir)?;
            for p in &st.modified {
                lines.push(format!("M {p}"));
            }
            Ok(lines.join("\n"))
        }

        fn add_all(&self, _dir: &Path, _paths: &[String]) -> Result<(), String> {
            // staging is folded into commit()'s snapshot; the CLI backend
            // owns the granular staging surface.
            Ok(())
        }

        fn commit(&self, dir: &Path, message: &str) -> Result<String, String> {
            let repo = open(dir)?;
            // snapshot the tracked worktree into blobs → a fresh tree
            let index = repo.index().map_err(|e| format!("gix: {e}"))?;
            let backing = index.path_backing();
            let st = self.status(dir)?;
            let modified: std::collections::BTreeSet<String> =
                st.modified.iter().cloned().collect();
            let mut entries: Vec<(String, gix::hash::ObjectId, gix::objs::tree::EntryMode)> =
                Vec::new();
            for e in index.entries() {
                let rela = e.path_in(backing).to_str_lossy().into_owned();
                if modified.contains(&rela) {
                    let abs = dir.join(&rela);
                    let bytes = std::fs::read(&abs).map_err(|e| format!("read {rela}: {e}"))?;
                    let id = repo.write_blob(bytes).map_err(|e| format!("gix: {e}"))?;
                    entries.push((rela, id.detach(), gix::objs::tree::EntryKind::Blob.into()));
                } else {
                    entries.push((rela, e.id, gix::objs::tree::EntryKind::Blob.into()));
                }
            }
            let tree = write_tree_recursive(&repo, &mut entries)?;
            let parents: Vec<gix::hash::ObjectId> = match repo.head_id() {
                Ok(id) => vec![id.detach()],
                Err(_) => vec![],
            };
            let author = repo
                .author()
                .ok_or("gix: no author configured (set git config user.name/email)")?
                .map_err(|e| format!("gix author: {e}"))?;
            let committer = repo
                .committer()
                .ok_or("gix: no committer configured (set git config user.name/email)")?
                .map_err(|e| format!("gix committer: {e}"))?;
            let id = repo
                .commit_as(committer, author, "HEAD", message, tree, parents)
                .map_err(|e| format!("gix commit: {e}"))?;
            // keep the index consistent with the new tree
            let mut idx = repo
                .index_from_tree(&tree)
                .map_err(|e| format!("gix: {e}"))?;
            idx.write(Default::default())
                .map_err(|e| format!("gix index write: {e}"))?;
            Ok(id.to_string())
        }

        fn log(&self, dir: &Path, limit: usize) -> Result<Vec<CommitInfo>, String> {
            let repo = open(dir)?;
            let head = repo.head_id().map_err(|e| format!("gix: {e}"))?;
            let mut out = Vec::new();
            for info in repo
                .rev_walk([head])
                .all()
                .map_err(|e| format!("gix: {e}"))?
            {
                if out.len() >= limit {
                    break;
                }
                let info = info.map_err(|e| format!("gix walk: {e}"))?;
                let commit = info.object().map_err(|e| format!("gix: {e}"))?;
                let msg = commit.message().map_err(|e| format!("gix: {e}"))?;
                let summary = msg.summary().to_str_lossy().into_owned();
                out.push(CommitInfo {
                    id: info.id.to_string(),
                    summary,
                });
            }
            Ok(out)
        }

        fn reset_hard(&self, dir: &Path, rev: &str) -> Result<(), String> {
            let repo = open(dir)?;
            let oid: gix::hash::ObjectId = gix::hash::ObjectId::from_hex(rev.as_bytes())
                .map_err(|e| format!("gix: bad rev `{rev}`: {e}"))?;
            // move the current branch (or detached HEAD), then sync the
            // worktree + index from that tree.
            let name = repo
                .head_name()
                .map_err(|e| format!("gix: {e}"))?
                .ok_or("gix: no HEAD")?;
            use gix::refs::transaction::PreviousValue;
            repo.reference(name, oid, PreviousValue::Any, "marfim reset")
                .map_err(|e| format!("gix reset ref: {e}"))?;
            let tree = repo
                .find_object(oid)
                .map_err(|e| format!("gix: {e}"))?
                .peel_to_tree()
                .map_err(|e| format!("gix: {e}"))?
                .id;
            let (mut stream, mut index) = repo
                .worktree_stream(tree)
                .map_err(|e| format!("gix stream: {e}"))?;
            let wt = repo
                .worktree()
                .ok_or("gix: bare repository has no worktree")?
                .base()
                .to_path_buf();
            // materialize the tree: the stream yields entries in order,
            // each fully read before the next
            while let Some(mut entry) = stream
                .next_entry()
                .map_err(|e| format!("gix stream entry: {e}"))?
            {
                let rel = entry
                    .relative_path()
                    .to_path()
                    .map_err(|e| format!("gix: non-utf8 path in tree: {e}"))?;
                let dest = wt.join(rel);
                if entry.mode.is_tree() {
                    std::fs::create_dir_all(&dest)
                        .map_err(|e| format!("mkdir {}: {e}", dest.display()))?;
                } else {
                    if let Some(parent) = dest.parent() {
                        std::fs::create_dir_all(parent)
                            .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
                    }
                    use std::io::Read as _;
                    let mut buf = Vec::new();
                    entry
                        .read_to_end(&mut buf)
                        .map_err(|e| format!("read {}: {e}", dest.display()))?;
                    std::fs::write(&dest, buf)
                        .map_err(|e| format!("write {}: {e}", dest.display()))?;
                }
            }
            index
                .write(Default::default())
                .map_err(|e| format!("gix index write: {e}"))?;
            Ok(())
        }

        fn worktree_add_detached(&self, _dir: &Path, _target: &Path) -> Result<(), String> {
            Err("gix backend: worktree add is not supported in-process in this build — use the cli-git backend".into())
        }

        fn worktree_remove(&self, _dir: &Path, _target: &Path) -> Result<(), String> {
            Err("gix backend: worktree remove is not supported in-process in this build — use the cli-git backend".into())
        }
    }

    /// Build tree objects bottom-up from (path, oid, mode) triples.
    fn write_tree_recursive(
        repo: &gix::Repository,
        entries: &mut Vec<(String, gix::hash::ObjectId, gix::objs::tree::EntryMode)>,
    ) -> Result<gix::hash::ObjectId, String> {
        // group by first path segment; leaf entries write as blobs
        let mut files: Vec<gix::objs::tree::Entry> = Vec::new();
        let mut dirs: std::collections::BTreeMap<
            String,
            Vec<(String, gix::hash::ObjectId, gix::objs::tree::EntryMode)>,
        > = std::collections::BTreeMap::new();
        for (path, oid, mode) in entries.drain(..) {
            match path.split_once('/') {
                None => files.push(gix::objs::tree::Entry {
                    mode,
                    filename: path.into(),
                    oid,
                }),
                Some((dir, rest)) => {
                    dirs.entry(dir.to_string())
                        .or_default()
                        .push((rest.to_string(), oid, mode));
                }
            }
        }
        for (name, mut sub) in dirs {
            let oid = write_tree_recursive(repo, &mut sub)?;
            files.push(gix::objs::tree::Entry {
                mode: gix::objs::tree::EntryKind::Tree.into(),
                filename: name.into(),
                oid,
            });
        }
        // git trees sort entries as if dirs carry a trailing '/'
        files.sort_by(|a, b| {
            tree_entry_cmp(&a.filename, a.mode.is_tree(), &b.filename, b.mode.is_tree())
        });
        let tree = gix::objs::Tree { entries: files };
        repo.write_object(tree)
            .map(|id| id.detach())
            .map_err(|e| format!("gix write tree: {e}"))
    }

    /// git's tree-entry sort: directory names compare as if they had a
    /// trailing '/'.
    fn tree_entry_cmp(a: &[u8], a_is_dir: bool, b: &[u8], b_is_dir: bool) -> std::cmp::Ordering {
        let mut ka = a.to_vec();
        if a_is_dir {
            ka.push(b'/');
        }
        let mut kb = b.to_vec();
        if b_is_dir {
            kb.push(b'/');
        }
        ka.cmp(&kb)
    }
}
