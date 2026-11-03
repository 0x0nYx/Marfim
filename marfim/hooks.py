"""Lifecycle hooks — Claude Code event semantics with ECC enforcement spirit.

Configured shell commands receive JSON payloads on stdin and env context:
  MARFIM_EVENT=PreToolUse MARFIM_TOOL=bash
Exit codes: 0 continue · 2 BLOCK (PreToolUse veto w/ stderr message) · other = warn.

Review finding 33: a SECURITY hook (PreToolUse) that times out now fails
CLOSED — the call is denied, not waved through.  Hooks also run with a
scrubbed environment (no API keys leak into hook processes); only
TRUSTED configs can define hooks at all (see marfim/trust.py).
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

from marfim.config import Config
from marfim.util import json_dumps, strict_secret_env


class HookBlocked(Exception):
    def __init__(self, message: str):
        super().__init__(message)
        self.message = message


class HookRunner:
    TIMEOUT = 15

    def __init__(self, config: Config):
        self.config = config

    def run(self, event: str, payload: dict | None = None, cwd: Path | None = None,
            fail_closed: bool = False) -> None:
        commands = (self.config.hooks or {}).get(event) or []
        if not commands:
            return
        payload = payload or {}
        proc_input = json_dumps(payload)
        env = strict_secret_env()
        env["MARFIM_EVENT"] = event
        env["MARFIM_TOOL"] = str(payload.get("tool", ""))
        # a vanished project dir must not crash the hook path
        cwd_arg = str(cwd) if cwd and Path(cwd).exists() else None
        for cmd in commands:
            try:
                proc = subprocess.run(
                    ["bash", "-c", cmd],
                    input=proc_input,
                    capture_output=True,
                    text=True,
                    timeout=self.TIMEOUT,
                    cwd=cwd_arg,
                    env=env,
                )
            except subprocess.TimeoutExpired as exc:
                if fail_closed:
                    # finding 33: a security hook that hangs must deny, not pass
                    raise HookBlocked(
                        f"{event} hook timed out after {self.TIMEOUT}s "
                        f"(fail-closed: security hooks must not fail open)") from exc
                continue
            if proc.returncode == 2:
                raise HookBlocked(proc.stderr.strip() or f"blocked by {event} hook")
            # nonzero-but-not-2: advisory; surfaced when verbose
            if proc.returncode != 0 and self.config.verbose:
                import sys
                print(f"[hook:{event}] rc={proc.returncode} {proc.stderr.strip()[:200]}",
                      file=sys.stderr)

    @staticmethod
    def tool_payload(tool: str, args: dict, phase: str) -> dict:
        return {"event": phase, "tool": tool, "input": args}
