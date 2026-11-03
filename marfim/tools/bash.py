"""Bash execution tool — PTY-less with timeout, background jobs, execpolicy.

Doctrine merged:
- Codex: prefix execpolicy rules (allow/prompt/deny) + sandbox wrapper.
- Claude Code: persistent background shells with log polling.
- Goose: shell output is bounded and explicit.
"""

from __future__ import annotations

import os
import shlex
import subprocess
import time
from pathlib import Path

from marfim.config import Config, MARFIM_DIR
from marfim.sandbox import wrap_command
from marfim.tools.base import Tool, ToolCtx, ToolResult
from marfim.util import gen_id, truncate_tail

JOBS_DIR = ".marfim/jobs"


def _job_file(root: Path, job_id: str) -> Path:
    return root / JOBS_DIR / f"{job_id}.log"


def _do_bash(args: dict, ctx: ToolCtx) -> ToolResult:
    command = str(args.get("command") or "")
    if not command.strip():
        return ToolResult("error: command is required", is_error=True)
    timeout = int(args.get("timeout") or (ctx.agent.config.bash_timeout if ctx.agent else 120))
    background = bool(args.get("background"))
    description = str(args.get("description") or "")

    root = ctx.cwd
    if background:
        job_id = gen_id("job")
        log = _job_file(root, job_id)
        log.parent.mkdir(parents=True, exist_ok=True)
        with open(log, "w", encoding="utf-8") as fh:
            proc = subprocess.Popen(
                ["bash", "-lc", command], stdout=fh, stderr=subprocess.STDOUT,
                cwd=str(root), start_new_session=True)
        return ToolResult(f"started background job {job_id} (pid {proc.pid}); "
                          f"poll with bash: tail -n 50 {log}")

    effective = _execpolicy(command, ctx)
    if effective is not None:
        return effective
    writable = [root]
    if ctx.agent is not None:
        writable += ctx.agent.config.effective_dirs()
    argv = wrap_command(["bash", "-lc", command], _sandbox_mode(ctx), [Path(str(w)) for w in writable])
    try:
        proc = subprocess.run(argv, capture_output=True, text=True, timeout=timeout,
                              cwd=str(root))
    except subprocess.TimeoutExpired as exc:
        partial = (exc.stdout or "") if isinstance(exc.stdout, str) else ""
        return ToolResult(f"error: timed out after {timeout}s\n{truncate_tail(partial, 2000)}",
                          is_error=True, meta={"timeout": True})
    out = proc.stdout or ""
    err = proc.stderr or ""
    text = f"$ {command}\n" + (description and f"# {description}\n" or "")
    if out:
        text += out
    if err:
        text += ("\n--- stderr ---\n" + err)
    text += f"\n[exit {proc.returncode}]"
    ok = proc.returncode == 0
    return ToolResult(text.strip() or "(no output)", meta={"exit": proc.returncode},
                      is_error=not ok)


def _sandbox_mode(ctx: ToolCtx) -> str:
    if ctx.agent is None:
        return "none"
    cfg = ctx.agent.config
    mode = cfg.sandbox_mode
    if mode == "auto":
        import sys
        return "bubblewrap" if sys.platform == "linux" else "seatbelt"
    return mode


def _execpolicy(command: str, ctx: ToolCtx) -> ToolResult | None:
    """Codex-style prefix rules from config.execpolicy_deny (deny-list only;
    allow/ask stays with the permission engine)."""
    if ctx.agent is None:
        return None
    rules = getattr(ctx.agent.config, "execpolicy_deny", []) or []
    try:
        base = " ".join(shlex.split(command)[:2])
    except ValueError:
        base = command.split()[0] if command.split() else ""
    for rule in rules:
        if command.startswith(rule) or base == rule:
            return ToolResult(f"error: execpolicy denies command prefix '{rule}'",
                              is_error=True, meta={"denied": rule})
    return None


def build_tools() -> list[Tool]:
    return [
        Tool(name="bash",
             description=("Run a shell command in the project root. Params: command "
                          "(required), timeout (seconds), background (bool), description. "
                          "Use background=true for long-running processes; poll the log "
                          "path that is returned."),
             parameters={"type": "object", "properties": {
                 "command": {"type": "string"}, "timeout": {"type": "integer"},
                 "background": {"type": "boolean"}, "description": {"type": "string"}},
                 "required": ["command"]},
             handler=_do_bash),
    ]
