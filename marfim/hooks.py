"""Lifecycle hooks — Claude Code event semantics with ECC enforcement spirit.

Configured shell commands receive JSON payloads on stdin and env context:
  MARFIM_EVENT=PreToolUse MARFIM_TOOL=bash
Exit codes: 0 continue · 2 BLOCK (PreToolUse veto w/ stderr message) · other = warn.
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

from marfim.config import Config
from marfim.util import json_dumps


class HookBlocked(Exception):
    def __init__(self, message: str):
        super().__init__(message)
        self.message = message


class HookRunner:
    TIMEOUT = 15

    def __init__(self, config: Config):
        self.config = config

    def run(self, event: str, payload: dict | None = None, cwd: Path | None = None) -> None:
        commands = (self.config.hooks or {}).get(event) or []
        if not commands:
            return
        payload = payload or {}
        proc_input = json_dumps(payload)
        for cmd in commands:
            try:
                proc = subprocess.run(
                    ["bash", "-c", cmd],
                    input=proc_input,
                    capture_output=True,
                    text=True,
                    timeout=self.TIMEOUT,
                    cwd=str(cwd or self.config.project_root),
                    env={**dict(__import__("os").environ),
                         "MARFIM_EVENT": event,
                         "MARFIM_TOOL": str(payload.get("tool", ""))},
                )
            except subprocess.TimeoutExpired:
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
