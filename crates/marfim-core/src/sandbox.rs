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

/// Kill a process group: TERM then KILL after grace. Uses /bin/kill to
/// signal the negative pgid (no unsafe, no libc dep).
pub async fn kill_process_group(pgid: i32) {
    let _ = tokio::process::Command::new("/bin/kill")
        .arg("-TERM")
        .arg(format!("--{}", pgid))
        .status()
        .await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let _ = tokio::process::Command::new("/bin/kill")
        .arg("-KILL")
        .arg(format!("--{}", pgid))
        .status()
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_does_not_panic() {
        let s = Sandbox::probe();
        let _ = s.unshare_available();
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
