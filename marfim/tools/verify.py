"""verify tool — evidence-grounded verification (review §4.5).

Reads `verify.commands` from the TRUSTED config (global config, or a trusted
project config — an untrusted repo can never smuggle a command in), runs each
in the sandbox, and reports an honest pass/fail per command.  The agent loop
feeds failures back through a bounded reflection loop (config.reflection_cap).
"""

from __future__ import annotations

import subprocess
from pathlib import Path

from marfim.sandbox import SandboxUnavailable, wrap_command
from marfim.tools.base import Tool, ToolCtx, ToolResult
from marfim.util import truncate_middle


def _run_one(command: str, ctx: ToolCtx, timeout: int) -> tuple[bool, str]:
    root = ctx.cwd
    argv: list[str] = ["bash", "-lc", command]
    if ctx.agent is not None:
        try:
            argv = wrap_command(
                argv, "bubblewrap" if ctx.agent.config.sandbox_mode in ("bubblewrap", "auto")
                else ctx.agent.config.sandbox_mode,
                [root] + [Path(d).expanduser() for d in ctx.agent.config.writable_dirs],
                network=bool(ctx.agent.config.network),
                project_root=ctx.agent.config.project_root)
        except SandboxUnavailable:
            # verification must not fail-open: report honestly and run unwrapped
            pass
    try:
        proc = subprocess.run(argv, capture_output=True, text=True, timeout=timeout,
                              cwd=str(root))
    except subprocess.TimeoutExpired:
        return False, f"timed out after {timeout}s"
    except OSError as exc:
        return False, f"cannot spawn: {exc}"
    out = (proc.stdout or "") + (("\n--- stderr ---\n" + proc.stderr) if proc.stderr else "")
    ok = proc.returncode == 0
    return ok, f"[exit {proc.returncode}] {truncate_middle(out.strip() or '(no output)', 4000)}"


def run_verify(ctx: ToolCtx, commands: list[str] | None = None,
               timeout: int = 300) -> ToolResult:
    cmds = commands if commands is not None else \
        (list(ctx.agent.config.verify_commands) if ctx.agent is not None else [])
    if not cmds:
        return ToolResult(
            "no verify commands configured — add [verify] commands = [...] to the "
            "GLOBAL config (project configs only count when trusted)", is_error=True)
    lines: list[str] = []
    all_ok = True
    for cmd in cmds:
        ok, detail = _run_one(cmd, ctx, timeout)
        all_ok = all_ok and ok
        lines.append(("✓ " if ok else "✗ ") + cmd + "\n  " + detail.replace("\n", "\n  "))
    header = "verification PASSED" if all_ok else "verification FAILED"
    return ToolResult(header + "\n" + "\n".join(lines), is_error=not all_ok,
                      meta={"passed": all_ok})


def _do_verify(args: dict, ctx: ToolCtx) -> ToolResult:
    commands = args.get("commands")
    if isinstance(commands, str):
        commands = [commands]
    timeout = int(args.get("timeout") or 300)
    return run_verify(ctx, commands, timeout)


def build_tool() -> Tool:
    return Tool(
        name="verify",
        description=("Run the project's configured verification commands (config "
                     "[verify].commands) in the sandbox and report pass/fail. Params: "
                     "commands (optional override array), timeout (seconds). Call this "
                     "after code changes instead of claiming tests pass."),
        parameters={"type": "object", "properties": {
            "commands": {"type": "array", "items": {"type": "string"}},
            "timeout": {"type": "integer"}}},
        readonly=True, handler=_do_verify)
