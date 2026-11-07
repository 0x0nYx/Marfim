//! Platform abstraction (W1.2): ONE trait for every OS assumption the
//! harness makes — the shell path, the sandbox exec-helper binary, the
//! config/data/cache directories, and the `unshare` probe. The desktop
//! behavior is the default; the android cfg picks `/system/bin/sh` and
//! app-injected directories (NO `HOME` assumption — android has none;
//! the embedding sets `MARFIM_HOME`/`MARFIM_DATA`/`MARFIM_CACHE` before
//! the first call, e.g. via the W4.1 FFI start contract).
//!
//! Everything here is deterministic and injectable: `default_platform()`
//! resolves once per process, and every consumer accepts an override so
//! tests can pin behavior.

use std::path::PathBuf;
use std::sync::Arc;

pub trait Platform: Send + Sync {
    /// POSIX shell used for `sh -c` execution (bash tool, hooks).
    fn shell_path(&self) -> &str;
    /// Path of the sandbox exec-helper (the binary that re-execs itself
    /// as `sandbox-exec` to apply Landlock/Seatbelt). Err = unavailable.
    fn exec_helper(&self) -> Result<PathBuf, String>;
    /// Config dir (the `MARFIM_HOME` analog: trust store, config layers).
    fn config_dir(&self) -> PathBuf;
    /// Data dir (session journals and other durable, rebuildable state).
    fn data_dir(&self) -> PathBuf;
    /// Cache dir (disposable: catalogs, parsed snapshots).
    fn cache_dir(&self) -> PathBuf;
    /// Can `unshare --net` actually run here? (Not merely exists —
    /// containers often EPERM it.)
    fn probe_unshare(&self) -> bool;
    /// Platform tag for honest telemetry; never used for behavior
    /// branches outside this module.
    fn name(&self) -> &'static str;
}

/// Desktop default (linux/macOS/windows): standard POSIX paths, XDG
/// fallbacks, HOME honored when present.
pub struct Desktop;

impl Platform for Desktop {
    fn shell_path(&self) -> &str {
        "/bin/sh"
    }

    fn exec_helper(&self) -> Result<PathBuf, String> {
        std::env::current_exe().map_err(|e| format!("cannot locate marfim binary: {e}"))
    }

    fn config_dir(&self) -> PathBuf {
        desktop_dir(
            &|k| std::env::var(k).ok(),
            "MARFIM_HOME",
            "MARFIM_DATA",
            "MARFIM_CACHE",
        )
        .0
    }

    fn data_dir(&self) -> PathBuf {
        desktop_dir(
            &|k| std::env::var(k).ok(),
            "MARFIM_HOME",
            "MARFIM_DATA",
            "MARFIM_CACHE",
        )
        .1
    }

    fn cache_dir(&self) -> PathBuf {
        desktop_dir(
            &|k| std::env::var(k).ok(),
            "MARFIM_HOME",
            "MARFIM_DATA",
            "MARFIM_CACHE",
        )
        .2
    }

    fn probe_unshare(&self) -> bool {
        std::process::Command::new("unshare")
            .arg("--net")
            .arg("--")
            .arg("/bin/true")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn name(&self) -> &'static str {
        "desktop"
    }
}

/// Desktop dir resolution kept byte-identical to the pre-W1.2
/// `marfim_home()`: MARFIM_HOME → $HOME/.marfim → "." (no HOME at all:
/// the "." fallback keeps the process honest instead of guessing).
/// Pure over an injected env lookup so tests never mutate the process
/// environment (determinism doctrine).
fn desktop_dir(
    env: &dyn Fn(&str) -> Option<String>,
    home_key: &str,
    data_key: &str,
    cache_key: &str,
) -> (PathBuf, PathBuf, PathBuf) {
    let home = env(home_key).map(PathBuf::from).unwrap_or_else(|| {
        env("HOME")
            .map(|h| PathBuf::from(h).join(".marfim"))
            .unwrap_or_else(|| PathBuf::from("."))
    });
    let data = env(data_key)
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("data"));
    let cache = env(cache_key)
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("cache"));
    (home, data, cache)
}

/// Android embedding (W4): `/system/bin/sh` is the documented shell
/// location in AOSP (mksh); directories come from the embedding via the
/// `MARFIM_*` env contract (set by the FFI start path from app-private
/// storage) — never from `HOME`, which android does not provide.
#[cfg(target_os = "android")]
pub struct Android;

#[cfg(target_os = "android")]
impl Platform for Android {
    fn shell_path(&self) -> &str {
        "/system/bin/sh"
    }

    fn exec_helper(&self) -> Result<PathBuf, String> {
        std::env::current_exe().map_err(|e| format!("cannot locate exec helper: {e}"))
    }

    fn config_dir(&self) -> PathBuf {
        env_dir("MARFIM_HOME")
    }

    fn data_dir(&self) -> PathBuf {
        env_dir("MARFIM_DATA")
    }

    fn cache_dir(&self) -> PathBuf {
        env_dir("MARFIM_CACHE")
    }

    fn probe_unshare(&self) -> bool {
        // unshare is not usable from an unprivileged app process; do not
        // probe (the probe itself would be denied) — report unavailable.
        false
    }

    fn name(&self) -> &'static str {
        "android"
    }
}

#[cfg(target_os = "android")]
fn env_dir(key: &str) -> PathBuf {
    std::env::var(key)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// The process-wide platform. Resolved once per call site through this
/// constructor; tests pin behavior by constructing `Desktop` directly.
pub fn default_platform() -> Arc<dyn Platform> {
    #[cfg(target_os = "android")]
    {
        Arc::new(Android)
    }
    #[cfg(not(target_os = "android"))]
    {
        Arc::new(Desktop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_shell_and_helper_resolve() {
        let p = Desktop;
        assert_eq!(p.shell_path(), "/bin/sh");
        assert_eq!(p.name(), "desktop");
        // exec_helper resolves to *some* binary in every cargo/test context
        assert!(p.exec_helper().is_ok());
    }

    #[test]
    fn desktop_dirs_marfim_home_wins() {
        let (home, data, cache) = desktop_dir(
            &|k| {
                if k == "MARFIM_HOME" {
                    Some("/tmp/mh".into())
                } else {
                    None
                }
            },
            "MARFIM_HOME",
            "MARFIM_DATA",
            "MARFIM_CACHE",
        );
        assert_eq!(home, PathBuf::from("/tmp/mh"));
        assert_eq!(data, PathBuf::from("/tmp/mh/data"));
        assert_eq!(cache, PathBuf::from("/tmp/mh/cache"));
    }

    #[test]
    fn desktop_dirs_home_fallback_and_overrides() {
        let (home, data, cache) = desktop_dir(
            &|k| match k {
                "HOME" => Some("/home/u".into()),
                "MARFIM_DATA" => Some("/var/marfim".into()),
                "MARFIM_CACHE" => Some("/tmp/mc".into()),
                _ => None,
            },
            "MARFIM_HOME",
            "MARFIM_DATA",
            "MARFIM_CACHE",
        );
        assert_eq!(home, PathBuf::from("/home/u/.marfim"));
        assert_eq!(data, PathBuf::from("/var/marfim"));
        assert_eq!(cache, PathBuf::from("/tmp/mc"));
    }

    #[test]
    fn desktop_dirs_no_env_at_all_is_honest_dot() {
        let (home, _, _) = desktop_dir(&|_| None, "MARFIM_HOME", "MARFIM_DATA", "MARFIM_CACHE");
        assert_eq!(home, PathBuf::from("."));
    }
}
