//! Sandbox: availability probing + subprocess construction (INV-09/10).
//! Env is always sanitized; process group is set so timeout kills the
//! whole tree; network isolation uses `unshare -n` when available
//! (honest degradation otherwise).

use std::process::Stdio;
use tokio::process::Command;

#[derive(Debug, Clone, Default)]
pub struct Sandbox {
    pub network: bool,
    pub writable_roots: Vec<String>,
    unshare_available: bool,
}

impl Sandbox {
    /// Probe sandbox features once at startup: `unshare` must not merely
    /// exist but actually succeed (containers often EPERM it).
    pub fn probe() -> Self {
        let unshare_available = std::process::Command::new("unshare")
            .arg("--net")
            .arg("--")
            .arg("/bin/true")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        Self {
            network: false,
            writable_roots: vec![],
            unshare_available,
        }
    }

    pub fn unshare_available(&self) -> bool {
        self.unshare_available
    }

    /// Build a Command with: sanitized env, process group 0, cwd,
    /// (when possible) network isolation, and (when writable roots are
    /// configured) filesystem containment via the `marfim sandbox-exec`
    /// self-wrapper. FAIL-CLOSED: when roots are configured but this
    /// platform cannot enforce them, `command()` returns an error string
    /// and the caller must refuse to spawn.
    pub fn command(&self, argv0: &str) -> Result<Command, String> {
        let fs_rooted = !self.writable_roots.is_empty();
        let mut cmd = if fs_rooted {
            // filesystem containment requested: exec through our own
            // sandbox-exec wrapper (applies Landlock on Linux / Seatbelt
            // profile on macOS, then execs the target — fail-closed)
            if !self.fs_sandbox_available() {
                return Err(format!(
                    "filesystem sandbox requested (writable_roots={:?}) but this platform cannot enforce it — refusing to run `{argv0}` unsandboxed",
                    self.writable_roots
                ));
            }
            let self_exe = std::env::current_exe()
                .map_err(|e| format!("cannot locate marfim for sandbox-exec: {e}"))?;
            let mut c = Command::new(self_exe);
            c.arg("sandbox-exec");
            for r in &self.writable_roots {
                c.arg("--root").arg(r);
            }
            c.arg("--").arg(argv0);
            c
        } else if self.unshare_available && !self.network {
            // network-isolated wrapper: unshare --net -- <argv0> ...
            let mut c = Command::new("unshare");
            c.arg("--net").arg("--").arg(argv0);
            c
        } else {
            Command::new(argv0)
        };
        cmd.env_clear();
        for (k, v) in crate::permissions::sanitize_env(std::env::vars().collect::<Vec<_>>()) {
            cmd.env(k, v);
        }
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        #[cfg(unix)]
        {
            // tokio re-exposes process_group on unix; children die as a group.
            cmd.process_group(0);
        }
        Ok(cmd)
    }

    /// Can this platform enforce filesystem containment? (Linux: Landlock
    /// kernel support; macOS: sandbox-exec exists.)
    pub fn fs_sandbox_available(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            crate::sandbox::landlock_supported()
        }
        #[cfg(target_os = "macos")]
        {
            std::process::Command::new("/usr/bin/sandbox-exec")
                .arg("-p")
                .arg("(version 1)")
                .arg("/usr/bin/true")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            false
        }
    }
}

/// Kill a process group: TERM then KILL after grace, via kill(2) on the
/// negative pgid. Portable across Linux and macOS, no `/bin/kill`
/// subprocess (whose `--<pgid>` argument form GNU kill rejects). Safe
/// API only: marfim-core forbids unsafe code, so libc is wrapped by nix.
pub async fn kill_process_group(pgid: i32) {
    #[cfg(unix)]
    {
        use nix::sys::signal::{kill, Signal};
        use nix::unistd::Pid;
        // negative pid targets the whole process group (POSIX kill(2))
        let _ = kill(Pid::from_raw(-pgid), Signal::SIGTERM);
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let _ = kill(Pid::from_raw(-pgid), Signal::SIGKILL);
    }
    #[cfg(not(unix))]
    {
        // No portable group signal on this platform; callers fall back to
        // kill_on_drop for the direct child.
        let _ = pgid;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_does_not_panic() {
        let s = Sandbox::probe();
        let _ = s.unshare_available();
    }

    /// REGRESSION (Phase 5): kill_process_group used to shell out to
    /// `/bin/kill -TERM --<pgid>`, which GNU kill parses as an unknown
    /// option — the kill silently failed and timed-out process groups
    /// LEAKED. The test must fail if the group survives the kill.
    #[tokio::test]
    async fn kill_process_group_actually_kills_the_group() {
        let s = Sandbox::probe();
        let mut cmd = s.command("/bin/sh").expect("probe has no fs roots");
        cmd.arg("-c").arg("sleep 30 & wait");
        let mut child = cmd.spawn().expect("spawn sh");
        let pid = child.id().expect("pid") as i32;

        kill_process_group(pid).await;

        // The direct child must be reaped within 5s even though `sleep 30`
        // was still running (with the bug, wait() blocks the full 30s).
        let reaped = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await;
        assert!(
            reaped.is_ok(),
            "process group {pid} survived kill_process_group (leaked child)"
        );

        // The negative pgid must no longer resolve: every group member is
        // gone. (We own the group, so failure can only be ESRCH, never EPERM.)
        #[cfg(unix)]
        {
            let probe = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(-pid),
                nix::sys::signal::Signal::SIGTERM,
            );
            assert!(
                matches!(probe, Err(nix::errno::Errno::ESRCH)),
                "process group {pid} still has live members: {probe:?}"
            );
        }
    }

    #[tokio::test]
    async fn command_has_clean_env() {
        std::env::set_var("MARFIM_TEST_SECRET", "x1");
        let s = Sandbox::probe();
        let out = s
            .command("/usr/bin/env")
            .expect("probe has no fs roots")
            .output()
            .await
            .expect("env runs");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            !text.contains("MARFIM_TEST_SECRET"),
            "secret leaked: {text}"
        );
        assert!(text.contains("PATH="), "PATH must pass through");
    }

    #[tokio::test]
    async fn fs_roots_without_enforcement_refuse_to_spawn() {
        let mut s = Sandbox::probe();
        s.writable_roots = vec!["/tmp".into()];
        if s.fs_sandbox_available() {
            return; // enforcement-capable platform: refusal not expected here
        }
        let Err(err) = s.command("/bin/echo") else {
            panic!("must refuse on this platform");
        };
        assert!(err.contains("refusing to run"), "{err}");
    }

    #[tokio::test]
    async fn process_group_survives_wrapper() {
        let s = Sandbox::probe();
        let mut cmd = s.command("/bin/echo").expect("probe has no fs roots");
        cmd.arg("ok");
        let out = cmd.output().await.unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "ok");
    }
}

// ============================================================ P5: fs sandbox

/// Symlink-aware containment (P5): resolve `path` (following symlinks for
/// every component that exists, lexically normalizing the non-existent
/// tail) and verify it stays inside one of `roots`. Rejects `..` escapes
/// and symlink targets pointing outside — a writable root with a symlink
/// to `/etc/passwd` is an ESCAPE, not a write.
pub fn path_within_roots(path: &std::path::Path, roots: &[std::path::PathBuf]) -> Containment {
    if roots.is_empty() {
        return Containment::Unrestricted;
    }
    let resolved = resolve_existing_prefix(path);
    for root in roots {
        let rc = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
        if resolved.starts_with(&rc) {
            return Containment::Within { resolved };
        }
    }
    Containment::Escapes { resolved }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Containment {
    /// No roots configured — no filesystem restriction requested.
    Unrestricted,
    /// Resolves inside a configured root; carries the fully resolved path.
    Within { resolved: std::path::PathBuf },
    /// Resolves OUTSIDE every root (via `..`, symlink or absolute path).
    Escapes { resolved: std::path::PathBuf },
}

/// Canonicalize the deepest existing ancestor, append the remaining
/// components verbatim, then lexically normalize `.` and `..`.
fn resolve_existing_prefix(path: &std::path::Path) -> std::path::PathBuf {
    let mut existing = path.to_path_buf();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    loop {
        match std::fs::canonicalize(&existing) {
            Ok(c) => {
                let mut resolved = c;
                for t in tail.iter().rev() {
                    resolved.push(t);
                }
                return normalize_lexical(&resolved);
            }
            Err(_) => match (existing.file_name(), existing.parent()) {
                (Some(name), Some(parent)) => {
                    tail.push(name.to_os_string());
                    existing = parent.to_path_buf();
                }
                _ => return normalize_lexical(path),
            },
        }
    }
}

/// Lexical normalization of `.` / `..` on an absolute path.
fn normalize_lexical(path: &std::path::Path) -> std::path::PathBuf {
    let mut out: std::path::PathBuf = std::path::PathBuf::new();
    for comp in path.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Seatbelt profile text (macOS): writes confined to the given roots,
/// reads and process execution broadly allowed. The GENERATOR is
/// cross-platform and unit-tested everywhere; applying it (via
/// `sandbox-exec -p`) is macOS-only.
pub fn seatbelt_profile(writable_roots: &[std::path::PathBuf]) -> String {
    let mut rules = String::new();
    for r in writable_roots {
        rules.push_str(&format!(
            "  (allow file-write* (subpath \"{}\"))\n",
            r.display()
        ));
    }
    format!(
        "(version 1)\n(deny default)\n\
         (allow file-read*)\n\
         (allow process-exec)\n\
         (allow process-fork)\n\
         (allow sysctl-read)\n\
         (allow mach-lookup)\n\
         (allow network*)\n\
{rules}  (deny file-write*)\n"
    )
}

#[cfg(target_os = "linux")]
mod landlock_imp {
    //! Landlock enforcement via the safe-API crate. Fail-closed: ABI v5 is
    //! a HARD requirement — an unsupported kernel returns Err and the
    //! caller must refuse to spawn the child rather than run it free.

    use landlock::{
        Access, AccessFs, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset, RulesetAttr,
        RulesetCreatedAttr, ABI,
    };
    use std::path::Path;

    fn build_ruleset() -> Result<landlock::RulesetCreated, landlock::RulesetError> {
        // HardRequirement: unsupported kernels must ERROR, never degrade
        // (fail-closed P5 contract). Access set = the full ABI v5 surface.
        Ruleset::default()
            .set_compatibility(CompatLevel::HardRequirement)
            .handle_access(AccessFs::from_all(ABI::V5))?
            .create()
    }

    /// True when this kernel supports Landlock ABI v5 (5.13+). Probed by
    /// building (not enforcing) a ruleset.
    pub fn supported() -> bool {
        build_ruleset().is_ok()
    }

    /// Apply: reads + execution everywhere, writes ONLY under `roots`.
    /// Restricts the CURRENT process; the caller then execs the child so
    /// the restriction is inherited.
    pub fn enforce_write_roots(roots: &[&Path]) -> Result<(), String> {
        if roots.is_empty() {
            return Err("no writable roots configured — refusing to sandbox to nowhere".into());
        }
        let created = build_ruleset().map_err(|e| format!("landlock unavailable: {e}"))?;
        let read_everywhere = AccessFs::from_read(ABI::V5);
        let all = AccessFs::from_all(ABI::V5);
        let root_fd = PathFd::new("/").map_err(|e| format!("landlock cannot open /: {e}"))?;
        let ruleset = created
            .add_rule(PathBeneath::new(root_fd, read_everywhere))
            .map_err(|e| format!("landlock read rule failed: {e}"))?;
        let mut current = ruleset;
        for r in roots {
            let fd =
                PathFd::new(r).map_err(|e| format!("landlock cannot open {}: {e}", r.display()))?;
            current = current
                .add_rule(PathBeneath::new(fd, all))
                .map_err(|e| format!("landlock write rule for {}: {e}", r.display()))?;
        }
        current
            .restrict_self()
            .map_err(|e| format!("landlock restrict_self failed: {e}"))?;
        Ok(())
    }
}

/// Landlock kernel support probe (Linux only; false elsewhere).
pub fn landlock_supported() -> bool {
    #[cfg(target_os = "linux")]
    {
        landlock_imp::supported()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// Enforce write-containment on the CURRENT process (Linux only).
/// Fail-closed contract: Err means "do not spawn the child".
pub fn enforce_write_roots(roots: &[&std::path::Path]) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        landlock_imp::enforce_write_roots(roots)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = roots;
        Err("filesystem sandbox not implemented on this platform".into())
    }
}

#[cfg(test)]
mod sandbox_policy_tests {
    use super::*;
    use std::path::Path;

    fn tmp_root(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("marfim-sandbox-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn no_roots_means_unrestricted() {
        assert_eq!(
            path_within_roots(Path::new("/etc/passwd"), &[]),
            Containment::Unrestricted
        );
    }

    #[test]
    fn simple_write_inside_root_is_within() {
        let root = tmp_root("within");
        let p = root.join("sub/file.txt");
        assert!(matches!(
            path_within_roots(&p, std::slice::from_ref(&root)),
            Containment::Within { .. }
        ));
    }

    #[test]
    fn dotdot_escape_is_caught_even_when_unresolvable() {
        let root = tmp_root("dotdot");
        let p = root.join("../../etc/passwd");
        assert!(matches!(
            path_within_roots(&p, &[root]),
            Containment::Escapes { .. }
        ));
    }

    #[test]
    fn symlink_to_outside_is_an_escape() {
        let root = tmp_root("symlink");
        std::fs::write(root.join("real.txt"), "x").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", root.join("evil")).unwrap();
        // the write target is the symlink INSIDE the root, but its
        // resolution lands outside → escape
        match path_within_roots(&root.join("evil"), &[root]) {
            Containment::Escapes { resolved } => {
                assert!(resolved.starts_with("/etc"), "{resolved:?}");
            }
            other => panic!("expected escape, got {other:?}"),
        }
    }

    #[test]
    fn seatbelt_profile_shape() {
        let p = seatbelt_profile(&["/work".into()]);
        assert!(p.contains("(version 1)"));
        assert!(p.contains("(deny default)"));
        assert!(p.contains("(allow file-write* (subpath \"/work\"))"));
        assert!(p.trim_end().ends_with("(deny file-write*)"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn landlock_refusal_is_fail_closed_on_unsupported_kernels() {
        // On kernels without Landlock (like this 5.10 CI box) enforce must
        // return Err — the caller then refuses to spawn, never runs free.
        let root = tmp_root("landlock");
        if !landlock_supported() {
            let err = enforce_write_roots(&[root.as_path()]);
            assert!(err.is_err(), "unsupported kernel must refuse, not no-op");
        } else {
            // enforcement-capable kernel: the real ruleset applies cleanly
            enforce_write_roots(&[root.as_path()]).expect("supported kernel enforces");
        }
    }
}

#[cfg(test)]
mod policy_battery {
    //! P5 regression battery: the deny>ask>allow chain, per-shell-segment
    //! checks and containment semantics that the spec enumerates.

    use crate::permissions::{Decision, Mode, PermissionEngine, Rule};
    use crate::shellparse::analyze;

    fn engine(
        mode: Mode,
    ) -> (
        PermissionEngine,
        tokio::sync::mpsc::Receiver<crate::permissions::ApprovalJob>,
    ) {
        PermissionEngine::new(mode, vec![])
    }

    fn engine_with(
        mode: Mode,
        rules: Vec<Rule>,
    ) -> (
        PermissionEngine,
        tokio::sync::mpsc::Receiver<crate::permissions::ApprovalJob>,
    ) {
        PermissionEngine::new(mode, rules)
    }

    async fn check(e: &PermissionEngine, cmd: &str) -> crate::permissions::Verdict {
        let argv: Vec<String> = cmd.split_whitespace().map(String::from).collect();
        let a = analyze(cmd);
        let risk = match a.risk {
            crate::shellparse::ShellRisk::Simple => None,
            r => Some(r),
        };
        // every battery check is bounded: an unanswered ask must surface as
        // a test failure, never a hang
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            e.check(
                "Bash",
                crate::permissions::ToolClass::Execute,
                Some(&argv),
                risk,
                cmd,
            ),
        )
        .await
        .expect("permission check resolved within 5s")
    }

    /// Auto-DENY all approval jobs (fail-closed battery posture).
    fn auto_deny(
        mut rx: tokio::sync::mpsc::Receiver<crate::permissions::ApprovalJob>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            while let Some(j) = rx.recv().await {
                let _ = j.respond.send(crate::permissions::ApprovalAnswer::Deny);
            }
        })
    }

    #[tokio::test]
    async fn deny_beats_ask_beats_allow_regardless_of_order() {
        // order-independence of the precedence chain
        let rules_deny_first = vec![
            Rule {
                tool: "Bash".into(),
                decision: Decision::Deny,
                argv_prefix: Some(vec!["rm".into()]),
            },
            Rule {
                tool: "Bash".into(),
                decision: Decision::Allow,
                argv_prefix: Some(vec!["rm".into(), "-i".into()]),
            },
        ];
        let rules_allow_first = vec![
            Rule {
                tool: "Bash".into(),
                decision: Decision::Allow,
                argv_prefix: Some(vec!["rm".into(), "-i".into()]),
            },
            Rule {
                tool: "Bash".into(),
                decision: Decision::Deny,
                argv_prefix: Some(vec!["rm".into()]),
            },
        ];
        for rules in [rules_deny_first, rules_allow_first] {
            let (e, rx) = engine_with(Mode::Default, rules);
            let _janitor = auto_deny(rx);
            let v = check(&e, "rm -i x").await;
            assert!(
                matches!(v.decision, Decision::Deny),
                "deny wins over allow: {v:?}"
            );
        }
    }

    #[tokio::test]
    async fn git_push_is_ask_in_default_and_allow_in_yolo() {
        // default: an Ask is raised — proven by an approval job arriving
        let (e, mut rx) = engine(Mode::Default);
        let e = std::sync::Arc::new(e);
        let asker = tokio::spawn(async move { check(e.as_ref(), "git push origin main").await });
        let job = rx
            .recv()
            .await
            .expect("ask job arrives on the approval channel");
        assert!(job.summary.contains("git push"), "{}", job.summary);
        let _ = job.respond.send(crate::permissions::ApprovalAnswer::Allow);
        let v = asker.await.unwrap();
        assert!(
            matches!(v.decision, Decision::Allow),
            "approved ask resolves allow: {v:?}"
        );

        // yolo: no approval needed at all
        let (y, mut yrx) = engine(Mode::Yolo);
        let vy = check(&y, "git push origin main").await;
        assert!(
            matches!(vy.decision, Decision::Allow),
            "git push allowed in yolo: {vy:?}"
        );
        assert!(yrx.try_recv().is_err(), "yolo raises no approval job");
    }

    #[tokio::test]
    async fn ask_without_a_listener_fails_closed() {
        let (e, rx) = engine(Mode::Default);
        drop(rx); // nobody home
        let v = check(&e, "git push origin main").await;
        assert!(matches!(v.decision, Decision::Deny), "fail closed: {v:?}");
        assert!(v.reason.contains("fail closed"), "{v:?}");
    }

    #[tokio::test]
    async fn rm_is_denied_by_default_engine() {
        let (e, rx) = engine(Mode::Default);
        let _janitor = auto_deny(rx);
        let v = check(&e, "rm -rf /tmp/x").await;
        assert!(
            matches!(v.decision, Decision::Deny),
            "rm denied by default: {v:?}"
        );
    }

    #[tokio::test]
    async fn chained_deletion_is_caught_per_segment() {
        // `true && rm -rf /` — the innocent head must not launder the tail
        let a = analyze("true && rm -rf /");
        assert!(
            matches!(a.risk, crate::shellparse::ShellRisk::Sequenced),
            "chained rm flagged above Simple: {a:?}"
        );
        assert!(a.segments.len() >= 2, "per-segment split: {a:?}");
        let (e, rx) = engine(Mode::Default);
        let _janitor = auto_deny(rx);
        let v = check(&e, "true && rm -rf /").await;
        assert!(
            matches!(v.decision, Decision::Deny),
            "chained rm denied: {v:?}"
        );
    }

    #[test]
    fn etc_passwd_write_is_an_escape_from_tmp_root() {
        let roots = vec![std::path::PathBuf::from("/tmp")];
        let v = super::path_within_roots(std::path::Path::new("/etc/passwd"), &roots);
        assert!(matches!(v, super::Containment::Escapes { .. }), "{v:?}");
        // via dotdot from inside /tmp too
        let v2 = super::path_within_roots(std::path::Path::new("/tmp/../../etc/passwd"), &roots);
        assert!(matches!(v2, super::Containment::Escapes { .. }), "{v2:?}");
    }

    #[test]
    fn env_sanitization_hides_secrets_but_keeps_path() {
        // command_has_clean_env covers the live probe; here we pin the
        // contract shape: sanitize_env drops MARFIM_* secrets.
        let vars = vec![
            ("MARFIM_API_TOKEN".to_string(), "s3cret".to_string()),
            ("PATH".to_string(), "/usr/bin".to_string()),
        ];
        let out = crate::permissions::sanitize_env(vars);
        assert!(!out.iter().any(|(k, _)| k == "MARFIM_API_TOKEN"));
        assert!(out.iter().any(|(k, _)| k == "PATH"));
    }
}
