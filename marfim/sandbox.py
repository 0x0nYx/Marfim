"""Sandboxing backends — Codex doctrine, simplified.

none       : direct execution (documented risk)
bubblewrap : Linux; read-only / + writable workspace & declared dirs
seatbelt   : macOS sandbox-exec profile (best-effort)

The sandbox is defense-in-depth, NOT a security boundary (Hermes SECURITY.md
honesty doctrine). It reduces blast radius for file writes.
"""

from __future__ import annotations

import shutil
import sys
from pathlib import Path


def wrap_command(cmd: list[str], mode: str, writable_dirs: list[Path]) -> list[str]:
    if mode == "none":
        return cmd
    if mode in ("bubblewrap", "auto") and shutil.which("bwrap") and sys.platform == "linux":
        argv = ["bwrap", "--unshare-net" ]
        argv += ["--ro-bind", "/", "/"]
        argv += ["--dev", "/dev"]
        argv += ["--proc", "/proc"]
        argv += ["--tmpfs", "/tmp"]
        for d in writable_dirs:
            d = Path(d).resolve()
            if d.exists():
                argv += ["--bind", str(d), str(d)]
        # keep TMPDIR writable if it is inside workspace (test infra)
        argv += ["--die-with-parent"]
        return argv + cmd
    if mode in ("seatbelt", "auto") and sys.platform == "darwin" and shutil.which("sandbox-exec"):
        writes = ", ".join(f'(subpath "{d.resolve()}")' for d in writable_dirs) or \
                 '(subpath "/usr/local")'
        profile = (f"(version 1)(allow default)(deny file-write*)"
                   f"(allow file-write* (require-any {writes}))")
        return ["sandbox-exec", "-p", profile] + cmd
    return cmd


def sandbox_available(mode: str) -> bool:
    if mode == "none":
        return True
    if mode == "bubblewrap":
        return shutil.which("bwrap") is not None and sys.platform == "linux"
    if mode == "seatbelt":
        return shutil.which("sandbox-exec") is not None and sys.platform == "darwin"
    return False
