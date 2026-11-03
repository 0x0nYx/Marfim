"""Sandboxing backends — Codex doctrine, simplified.

none       : direct execution (explicit opt-out)
bubblewrap : Linux; read-only / + writable workspace & declared dirs
seatbelt   : macOS sandbox-exec profile (best-effort)

Policy (review finding 10): when a sandbox is REQUESTED (bubblewrap/seatbelt)
but the backend is unavailable, Marfim now hard-fails instead of silently
running unsandboxed.  `auto` degrades with a loud warning; `none` is the
explicit opt-out; `sandbox_required = false` in the global config restores the
old fail-open behaviour for machines that cannot sandbox.

Self-defense (Node-tree port): inside the sandbox the harness-control files
(.git/hooks, .git/config, .marfim/, MARFIM.md) are re-mounted READ-ONLY so a
sandboxed bash cannot plant a hook that escapes the sandbox later.  Children
run with a scrubbed environment — no API keys leak into bash or MCP servers.
"""

from __future__ import annotations

import shutil
import sys
from pathlib import Path

from marfim.config import Config, MARFIM_DIR, marfim_home
from marfim.util import strict_secret_env


class SandboxUnavailable(RuntimeError):
    """A sandbox mode was requested but its backend is missing (fail CLOSED)."""


SELF_DEFENSE_PATHS = (f"{MARFIM_DIR}", ".git/hooks", ".git/config", "MARFIM.md")


def _self_defense_binds(root: Path) -> list[list[str]]:
    argv: list[list[str]] = []
    for rel in SELF_DEFENSE_PATHS:
        p = (root / rel).resolve()
        if p.exists():
            argv.append(["--ro-bind", str(p), str(p)])
    home = marfim_home().resolve()
    if home.exists():
        argv.append(["--ro-bind", str(home), str(home)])
    return argv


def wrap_command(cmd: list[str], mode: str, writable_dirs: list[Path],
                 network: bool = True, project_root: Path | None = None,
                 scrub_env: bool = True) -> list[str]:
    """Wrap `cmd` for the requested sandbox mode.

    Raises SandboxUnavailable when an explicitly requested backend is missing
    (fail closed).  `auto` degrades to no sandbox with a stderr warning.
    """
    if mode == "none":
        return cmd
    if mode in ("bubblewrap", "auto") and sys.platform == "linux":
        if not shutil.which("bwrap"):
            if mode == "auto":
                _warn("sandbox auto: bubblewrap not found; running UNSANDBOXED")
                return cmd
            if _required_ok():
                _warn("sandbox_required=false: bubblewrap missing; running UNSANDBOXED "
                      "(explicit config opt-out)")
                return cmd
            raise SandboxUnavailable(
                "sandbox mode 'bubblewrap' requested but bwrap is not installed; "
                "install bubblewrap, set sandbox_mode='none', or set "
                "sandbox_required=false in the GLOBAL config to opt out explicitly")
        argv = []
        if not network:
            argv.append("--unshare-net")
        argv += ["--ro-bind", "/", "/"]
        argv += ["--dev", "/dev"]
        argv += ["--proc", "/proc"]
        argv += ["--tmpfs", "/tmp"]
        for d in writable_dirs:
            d = Path(d).resolve()
            if d.exists():
                argv += ["--bind", str(d), str(d)]
        # self-defense: harness-control paths are re-mounted read-only AFTER
        # the writable binds, so they win (Node selfDefenseBinds doctrine)
        if project_root is not None:
            for bind in _self_defense_binds(Path(project_root).resolve()):
                argv += bind
        argv += ["--die-with-parent"]
        if scrub_env:
            argv.append("--clearenv")
            env = strict_secret_env()
            for key, val in env.items():
                argv += ["--setenv", key, val]
        return argv + cmd
    if mode in ("seatbelt", "auto") and sys.platform == "darwin":
        if not shutil.which("sandbox-exec"):
            if mode == "auto":
                _warn("sandbox auto: sandbox-exec not found; running UNSANDBOXED")
                return cmd
            if _required_ok():
                _warn("sandbox_required=false: sandbox-exec missing; running UNSANDBOXED")
                return cmd
            raise SandboxUnavailable(
                "sandbox mode 'seatbelt' requested but sandbox-exec is not available")
        writes = ", ".join(f'(subpath "{d.resolve()}")' for d in writable_dirs) or \
                 '(subpath "/usr/local")'
        profile = (f"(version 1)(allow default)(deny file-write*)"
                   f"(allow file-write* (require-any {writes}))")
        return ["sandbox-exec", "-p", profile] + cmd
    if mode in ("bubblewrap", "seatbelt"):
        # requested on a platform without that backend
        if _required_ok():
            _warn(f"sandbox_required=false: {mode} unsupported here; running UNSANDBOXED")
            return cmd
        raise SandboxUnavailable(
            f"sandbox mode '{mode}' is not supported on {sys.platform}")
    _warn(f"unknown sandbox mode '{mode}'; running UNSANDBOXED")
    return cmd


def _required_ok() -> bool:
    """True when failing open is allowed (explicit sandbox_required=false)."""
    return not _cfg_required()


def _cfg_required() -> bool:
    """Read sandbox_required without a full config load (env + global file)."""
    import os
    import tomllib
    if os.environ.get("MARFIM_SANDBOX_REQUIRED", "").lower() in ("0", "false", "no"):
        return False
    try:
        data = tomllib.loads((marfim_home() / "config.toml").read_text(encoding="utf-8"))
        if data.get("sandbox_required") is False:
            return False
        sb = data.get("sandbox") or {}
        if isinstance(sb, dict) and sb.get("required") is False:
            return False
    except (OSError, tomllib.TOMLDecodeError):
        pass
    return True


def _warn(msg: str) -> None:
    print(f"[marfim:sandbox] {msg}", file=sys.stderr)


def sandbox_available(mode: str) -> bool:
    if mode == "none":
        return True
    if mode == "bubblewrap":
        return shutil.which("bwrap") is not None and sys.platform == "linux"
    if mode == "seatbelt":
        return shutil.which("sandbox-exec") is not None and sys.platform == "darwin"
    if mode == "auto":
        return True
    return False


def build_wrap_context(config: Config) -> dict:
    """Convenience bundle used by the bash tool."""
    return {
        "network": bool(config.network),
        "project_root": config.project_root,
        "required": bool(getattr(config, "sandbox_required", True)),
    }
