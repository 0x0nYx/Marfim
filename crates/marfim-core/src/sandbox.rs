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

    /// Build a Command with: sanitized env, process group 0, cwd, and
    /// (when possible) network isolation.
    pub fn command(&self, argv0: &str) -> Command {
        let mut cmd = if self.unshare_available && !self.network {
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
        cmd
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
        let mut cmd = s.command("/bin/sh");
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
        let out = s.command("/usr/bin/env").output().await.expect("env runs");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            !text.contains("MARFIM_TEST_SECRET"),
            "secret leaked: {text}"
        );
        assert!(text.contains("PATH="), "PATH must pass through");
    }

    #[tokio::test]
    async fn process_group_survives_wrapper() {
        let s = Sandbox::probe();
        let mut cmd = s.command("/bin/echo");
        cmd.arg("ok");
        let out = cmd.output().await.unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "ok");
    }
}
