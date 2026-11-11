//! W1.5: GitBackend differential battery — CliGit (default) is the
//! reference; GixGit (feature `gix`) must agree on every observable.

#![forbid(unsafe_code)]

use marfim_core::git::{CliGit, GitBackend};
use marfim_testkit::Temp;

fn init_repo(dir: &std::path::Path) {
    use std::process::Command;
    for (args, msg) in [
        (vec!["init"], "init"),
        (vec!["config", "user.email", "t@marfim.dev"], "cfg email"),
        (vec!["config", "user.name", "marfim-test"], "cfg name"),
    ] {
        let out = Command::new("git")
            .args(&args)
            .current_dir(dir)
            .output()
            .unwrap_or_else(|e| panic!("git {msg}: {e}"));
        assert!(
            out.status.success(),
            "git {msg} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

fn write(dir: &std::path::Path, rel: &str, content: &str) {
    let p = dir.join(rel);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap_or_else(|e| panic!("mkdir: {e}"));
    }
    std::fs::write(p, content).unwrap_or_else(|e| panic!("write {rel}: {e}"));
}

fn commit_all(dir: &std::path::Path, msg: &str) -> String {
    CliGit
        .add_all(dir, &[])
        .unwrap_or_else(|e| panic!("add: {e}"));
    CliGit
        .commit(dir, msg)
        .unwrap_or_else(|e| panic!("commit: {e}"))
}

#[test]
fn cli_git_full_cycle() {
    let tmp = Temp::new("git-backends").unwrap_or_else(|e| panic!("tmp: {e}"));
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap_or_else(|e| panic!("mkdir: {e}"));
    init_repo(&repo);
    assert!(CliGit.is_repo(&repo));
    write(&repo, "a.txt", "one\n");
    write(&repo, "sub/b.txt", "two\n");
    let id1 = commit_all(&repo, "first");

    let st = CliGit
        .status(&repo)
        .unwrap_or_else(|e| panic!("status: {e}"));
    assert!(!st.dirty, "clean after commit: {st:?}");

    write(&repo, "a.txt", "changed\n");
    let st = CliGit
        .status(&repo)
        .unwrap_or_else(|e| panic!("status: {e}"));
    assert!(st.dirty);
    assert!(st.modified.contains(&"a.txt".to_string()), "{st:?}");
    let diff = CliGit.diff(&repo).unwrap_or_else(|e| panic!("diff: {e}"));
    assert!(diff.contains("a.txt"), "{diff}");

    let id2 = commit_all(&repo, "second");
    let log = CliGit.log(&repo, 10).unwrap_or_else(|e| panic!("log: {e}"));
    assert_eq!(log.len(), 2);
    assert_eq!(log[0].id, id2);
    assert_eq!(log[1].id, id1);
    assert_eq!(log[1].summary, "first");

    // reset_hard back to the first commit
    CliGit
        .reset_hard(&repo, &id1)
        .unwrap_or_else(|e| panic!("reset: {e}"));
    let content = std::fs::read_to_string(repo.join("a.txt")).unwrap_or_default();
    assert_eq!(content, "one\n");
    let st = CliGit
        .status(&repo)
        .unwrap_or_else(|e| panic!("status: {e}"));
    assert!(!st.dirty, "clean after reset: {st:?}");
    let log = CliGit.log(&repo, 10).unwrap_or_else(|e| panic!("log: {e}"));
    assert_eq!(log.len(), 1);
}

#[test]
fn cli_git_worktree_add_remove_roundtrip() {
    let tmp = Temp::new("git-backends").unwrap_or_else(|e| panic!("tmp: {e}"));
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap_or_else(|e| panic!("mkdir: {e}"));
    init_repo(&repo);
    write(&repo, "a.txt", "one\n");
    commit_all(&repo, "first");
    let wt = tmp.path().join("wt");
    CliGit
        .worktree_add_detached(&repo, &wt)
        .unwrap_or_else(|e| panic!("worktree add: {e}"));
    assert!(wt.join("a.txt").is_file(), "worktree has the checkout");
    CliGit
        .worktree_remove(&repo, &wt)
        .unwrap_or_else(|e| panic!("worktree remove: {e}"));
    assert!(!wt.exists(), "worktree gone after remove");
}

#[test]
fn cli_git_non_repo_is_honest() {
    let tmp = Temp::new("git-backends").unwrap_or_else(|e| panic!("tmp: {e}"));
    assert!(!CliGit.is_repo(tmp.path()));
    assert!(CliGit.status(tmp.path()).is_err());
}

#[cfg(feature = "gix")]
mod gix_differential {
    use super::*;
    use marfim_core::git::GixGit;

    #[test]
    fn gix_agrees_with_cli_on_status_log_and_produces_valid_commits() {
        let tmp = Temp::new("git-backends").unwrap_or_else(|e| panic!("tmp: {e}"));
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap_or_else(|e| panic!("mkdir: {e}"));
        init_repo(&repo);
        write(&repo, "a.txt", "one\n");
        write(&repo, "sub/b.txt", "two\n");
        let id1 = commit_all(&repo, "first");
        assert!(GixGit.is_repo(&repo));

        // status agrees when clean
        let st_cli = CliGit
            .status(&repo)
            .unwrap_or_else(|e| panic!("status: {e}"));
        let st_gix = GixGit
            .status(&repo)
            .unwrap_or_else(|e| panic!("gix status: {e}"));
        assert!(!st_cli.dirty);
        assert!(!st_gix.dirty, "gix sees clean: {st_gix:?}");

        // log agrees on ids
        let log_cli = CliGit.log(&repo, 5).unwrap_or_else(|e| panic!("log: {e}"));
        let log_gix = GixGit
            .log(&repo, 5)
            .unwrap_or_else(|e| panic!("gix log: {e}"));
        assert_eq!(log_cli.len(), log_gix.len(), "{log_cli:?} vs {log_gix:?}");
        assert_eq!(log_cli[0].id, log_gix[0].id);

        // a GixGit commit is a REAL commit: cli sees it, tree intact
        write(&repo, "a.txt", "changed-by-gix\n");
        let st = GixGit
            .status(&repo)
            .unwrap_or_else(|e| panic!("gix status: {e}"));
        assert!(st.dirty, "{st:?}");
        let diff = GixGit
            .diff(&repo)
            .unwrap_or_else(|e| panic!("gix diff: {e}"));
        assert!(diff.contains("M a.txt"), "{diff}");
        let id2 = GixGit
            .commit(&repo, "gix commit")
            .unwrap_or_else(|e| panic!("gix commit: {e}"));
        assert_ne!(id1, id2);
        let st = CliGit
            .status(&repo)
            .unwrap_or_else(|e| panic!("status: {e}"));
        assert!(!st.dirty, "cli sees clean after gix commit: {st:?}");
        let content = std::fs::read_to_string(repo.join("a.txt")).unwrap_or_default();
        assert_eq!(content, "changed-by-gix\n");
        let log_cli = CliGit.log(&repo, 5).unwrap_or_else(|e| panic!("log: {e}"));
        assert_eq!(log_cli[0].id, id2);
        assert_eq!(log_cli[0].summary, "gix commit");

        // gix reset_hard restores the first commit; cli agrees
        GixGit
            .reset_hard(&repo, &id1)
            .unwrap_or_else(|e| panic!("gix reset: {e}"));
        let content = std::fs::read_to_string(repo.join("a.txt")).unwrap_or_default();
        assert_eq!(content, "one\n", "gix checkout restored the worktree");
        let st = CliGit
            .status(&repo)
            .unwrap_or_else(|e| panic!("status: {e}"));
        assert!(!st.dirty, "cli sees clean after gix reset: {st:?}");
        let log = CliGit.log(&repo, 5).unwrap_or_else(|e| panic!("log: {e}"));
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].id, id1);
    }
}
