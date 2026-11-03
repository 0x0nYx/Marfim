"""task tool — subagent spawning (Claude Code Agent + Oh My Pi worktrees +
Hermes delegate doctrine, simplified to depth-limited nested agents).

Roles: build (full tools), plan (no mutations), explore (read-only),
review (read-only + code-review system prompt).
Optional git-worktree isolation for parallel file work.
"""

from __future__ import annotations

import subprocess
from pathlib import Path

from marfim.tools.base import Tool, ToolCtx, ToolResult

ROLE_PROMPTS = {
    "build": "You are a Marfim build subagent. Implement what is asked. Use tools; "
             "verify your changes (run tests/builds when available). Report what you "
             "changed and the verification result.",
    "plan": "You are a Marfim plan subagent. Investigate and produce an implementation "
            "plan: goals, file-by-file changes, risks, verification steps. Do NOT mutate "
            "anything. Cite concrete file paths.",
    "explore": "You are a Marfim explore subagent. Investigate the codebase and report "
               "findings with file:line references. Do NOT mutate anything.",
    "review": "You are a Marfim review subagent. Review the specified code or diff for "
              "bugs, security issues, and design problems. Report findings as a numbered "
              "list with file:line references and severity.",
}
ROLE_TOOLS = {
    "build": None,                       # full toolset
    "plan": ["read", "grep", "glob", "repomap", "bash", "todo", "skill", "webfetch"],
    "explore": ["read", "grep", "glob", "repomap", "bash", "todo"],
    "review": ["read", "grep", "glob", "bash", "todo"],
}


def _make_worktree(root: Path, name: str) -> Path | None:
    wt = root / ".marfim" / "worktrees" / name
    wt.parent.mkdir(parents=True, exist_ok=True)
    try:
        subprocess.run(["git", "worktree", "add", str(wt), "-b", name],
                       cwd=str(root), capture_output=True, timeout=60, check=True)
        return wt
    except (subprocess.SubprocessError, OSError):
        return None


def _do_task(args: dict, ctx: ToolCtx) -> ToolResult:
    prompt = str(args.get("prompt") or "")
    role = str(args.get("role") or "build")
    model_role = str(args.get("model_role") or "")
    worktree = bool(args.get("worktree"))
    if not prompt:
        return ToolResult("error: prompt is required", is_error=True)
    if role not in ROLE_PROMPTS:
        role = "build"
    agent = ctx.agent
    if agent is None:
        return ToolResult("error: no agent context (headless tools only)", is_error=True)
    depth = ctx.depth
    max_depth = agent.config.max_subagent_depth
    if depth >= max_depth:
        return ToolResult(
            f"error: subagent depth limit reached ({max_depth}); do the work inline",
            is_error=True)

    cwd = ctx.cwd
    wt_path: Path | None = None
    if worktree:
        wt_path = _make_worktree(agent.config.project_root, f"marfim-task-{ctx.depth}-{id(args) % 99999}")
        if wt_path is not None:
            cwd = wt_path

    from marfim.agent.loop import Agent
    cfg = agent.config
    sub = Agent.spawn_subagent(parent=agent, role=role, cwd=cwd, depth=depth + 1,
                               model_role=model_role)
    try:
        report = sub.run_to_completion(
            prompt, allowed_tools=ROLE_TOOLS.get(role), say=lambda m: None)
    except Exception as exc:  # noqa: BLE001
        return ToolResult(f"error: subagent failed: {type(exc).__name__}: {exc}", is_error=True)
    prefix = f"[{role} subagent · {len(sub.session.messages)} messages · " \
             f"{sub.usage.input_tokens + sub.usage.output_tokens} tokens]"
    if wt_path is not None:
        prefix += f"\n[worktree: {wt_path}]"
    return ToolResult(f"{prefix}\n\n{report}")


def build_tools() -> list[Tool]:
    return [
        Tool(name="task",
             description=("Spawn a subagent for an independent chunk of work. Params: "
                          "prompt (required), role (build|plan|explore|review), "
                          "model_role (optional routing), worktree (bool: isolate in a "
                          "git worktree). The subagent reports back its final summary; "
                          "it cannot see this conversation."),
             parameters={"type": "object", "properties": {
                 "prompt": {"type": "string"}, "role": {"type": "string",
                 "enum": ["build", "plan", "explore", "review"]},
                 "model_role": {"type": "string"}, "worktree": {"type": "boolean"}},
                 "required": ["prompt"]},
             handler=_do_task),
    ]
