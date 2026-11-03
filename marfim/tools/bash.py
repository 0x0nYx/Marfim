"""Bash execution tool — PTY-less with timeout, background jobs, execpolicy.

Doctrine merged:
- Codex: prefix execpolicy rules (allow/prompt/deny) + sandbox wrapper.
- Claude Code: persistent background shells with log polling.
- Goose: shell output is bounded and explicit.

Review finding 5: background jobs now travel the SAME execpolicy + sandbox
path as foreground commands, run in their own session, are capped, and log to
0600 files.
"""

from __future__ import annotations

import os
import shlex
import subprocess
import threading
from pathlib import Path

from marfim.config import Config, MARFIM_DIR
from marfim.sandbox import SandboxUnavailable, wrap_command
from marfim.tools.base import Tool, ToolCtx, ToolResult
from marfim.util import gen_id, truncate_tail

JOBS_DIR = ".marfim/jobs"
MAX_BACKGROUND_JOBS = 8

_jobs_lock = threading.Lock()
_active_jobs: dict[str, subprocess.Popen] = {}


def _job_file(root: Path, job_id: str) -> Path:
    return root / JOBS_DIR / f"{job_id}.log"


def _reap_finished_jobs() -> None:
    with _jobs_lock:
        for job_id, proc in list(_active_jobs.items()):
            if proc.poll() is not None:
                del _active_jobs[job_id]


def _do_bash(args: dict, ctx: ToolCtx) -> ToolResult:
    command = str(args.get("command") or "")
    if not command.strip():
        return ToolResult("error: command is required", is_error=True)
    timeout = int(args.get("timeout") or (ctx.agent.config.bash_timeout if ctx.agent else 120))
    background = bool(args.get("background"))
    description = str(args.get("description") or "")

    root = ctx.cwd

    # execpolicy + sandbox apply to BOTH paths (review finding 5)
    effective = _execpolicy(command, ctx)
    if effective is not None:
        return effective
    writable = [root]
    if ctx.agent is not None:
        writable += ctx.agent.config.effective_dirs()
    if ctx.agent is not None:
        try:
            argv = wrap_command(["bash", "-lc", command], _sandbox_mode(ctx),
                                [Path(str(w)) for w in writable],
                                network=bool(ctx.agent.config.network),
                                project_root=ctx.agent.config.project_root)
        except SandboxUnavailable as exc:
            return ToolResult(f"error: sandbox unavailable — refusing to run: {exc}",
                              is_error=True)
    else:
        argv = ["bash", "-lc", command]

    if background:
        _reap_finished_jobs()
        with _jobs_lock:
            if len(_active_jobs) >= MAX_BACKGROUND_JOBS:
                return ToolResult(
                    f"error: background job cap reached ({MAX_BACKGROUND_JOBS}); "
                    f"poll or wait for existing jobs first", is_error=True)
            job_id = gen_id("job")
            log = _job_file(root, job_id)
            log.parent.mkdir(parents=True, exist_ok=True)
            # 0600 log: the output may quote secret-adjacent material
            fd = os.open(str(log), os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
            try:
                proc = subprocess.Popen(
                    argv, stdout=fd, stderr=subprocess.STDOUT,
                    cwd=str(root), start_new_session=True)
            finally:
                os.close(fd)
            _active_jobs[job_id] = proc
        return ToolResult(f"started background job {job_id} (pid {proc.pid}); "
                          f"poll with bash: tail -n 50 {log}")

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
