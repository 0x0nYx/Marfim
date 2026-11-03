"""task tool — subagent spawning (Claude Code Agent + Oh My Pi worktrees +
Hermes delegate doctrine).

Roles: build (full tools), plan (no mutations), explore (read-only),
review (read-only + code-review system prompt).

Review finding 25 fixes:
- the subagent's ToolCtx uses the SPAWN cwd (worktrees actually take effect);
- worktrees live OUTSIDE the repo tree (marfim home), so `git add -A` can
  never pick them up, and they are removed on completion with a mergeable
  diff returned in the report;
- the child gets a role-restricted permission engine (bypass cannot leak into
  explore/plan/review), the role prompt, project memory, and the parent's
  stop_event.

§4.3: `backend` runs an EXTERNAL agent (opencode / codex / claude-code /
hermes or anything configured under [external_agents]) inside its own
worktree, with the Marfim loop staying the single control plane.
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

from marfim.config import marfim_home
from marfim.tools.base import Tool, ToolCtx, ToolResult
from marfim.util import truncate_middle

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
    "plan": ["read", "grep", "glob", "repomap", "bash", "todo", "skill", "webfetch",
             "websearch"],
    "explore": ["read", "grep", "glob", "repomap", "bash", "todo"],
    "review": ["read", "grep", "glob", "bash", "todo"],
}


def _make_worktree(root: Path, name: str) -> Path | None:
    """Worktrees live under the marfim home, never inside the repo (finding 31:
    an in-repo worktree dir is one `git add -A` away from being committed)."""
    base = marfim_home() / "worktrees" / name
    base.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    try:
        subprocess.run(["git", "worktree", "add", "--detach", str(base)],
                       cwd=str(root), capture_output=True, timeout=60, check=True)
        return base
    except (subprocess.SubprocessError, OSError):
        return None


def _remove_worktree(root: Path, wt: Path) -> None:
    try:
        subprocess.run(["git", "worktree", "remove", "--force", str(wt)],
                       cwd=str(root), capture_output=True, timeout=60, check=False)
    except (subprocess.SubprocessError, OSError):
        pass


def _worktree_diff(wt: Path) -> tuple[str, str]:
    """(diff_text, changed_summary) for a mergeable-diff report (finding 25)."""
    try:
        subprocess.run(["git", "add", "-A"], cwd=str(wt), capture_output=True,
                       timeout=30, check=False)
        diff = subprocess.run(["git", "diff", "--cached", "HEAD"], cwd=str(wt),
                              capture_output=True, text=True, timeout=30, check=False)
        stat = subprocess.run(["git", "diff", "--cached", "--stat", "HEAD"], cwd=str(wt),
                              capture_output=True, text=True, timeout=30, check=False)
        return diff.stdout or "", (stat.stdout or "").strip()
    except (subprocess.SubprocessError, OSError):
        return "", ""


def _run_external(name: str, spec: dict, prompt: str, cwd: Path,
                  timeout: int = 900) -> ToolResult:
    """§4.3: run an external agent headlessly and normalize its output."""
    command = spec.get("command", "")
    if not command:
        return ToolResult(f"error: external agent '{name}' has no command configured",
                          is_error=True)
    args = [command] + list(spec.get("args", []))
    via = spec.get("prompt_via", "arg")
    fmt = spec.get("format", "text")
    if via == "arg":
        args.append(prompt)
    elif via == "stdin":
        pass
    try:
        proc = subprocess.run(args, input=prompt if via == "stdin" else None,
                              capture_output=True, text=True, timeout=timeout,
                              cwd=str(cwd))
    except FileNotFoundError:
        return ToolResult(f"error: external agent '{name}': '{command}' not installed",
                          is_error=True)
    except subprocess.TimeoutExpired:
        return ToolResult(f"error: external agent '{name}' timed out after {timeout}s",
                          is_error=True)
    if proc.returncode != 0:
        return ToolResult(f"error: external agent '{name}' exited {proc.returncode}: "
                          f"{(proc.stderr or proc.stdout or '')[:800]}", is_error=True)
    text = _normalize_stream(proc.stdout, fmt)
    return ToolResult(f"[{name} · exit 0]\n\n{text or '(no output)'}")


def _normalize_stream(raw: str, fmt: str) -> str:
    """stream-json normalizer (§4.3): extract the human-readable report from
    the JSONL conventions of codex/claude-code; passthrough for plain text."""
    if fmt == "text" or not raw.strip():
        return raw
    parts: list[str] = []
    for line in raw.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            evt = json.loads(line)
        except json.JSONDecodeError:
            continue
        # claude-code stream-json: {"type":"assistant","message":{"content":[...]}}
        if evt.get("type") == "assistant":
            for block in (evt.get("message") or {}).get("content") or []:
                if isinstance(block, dict) and block.get("type") == "text":
                    parts.append(block.get("text", ""))
        elif evt.get("type") == "result":
            parts.append(str(evt.get("result") or ""))
        # codex --json: {"msg":{"type":"agent_message","message":"..."}}
        msg = evt.get("msg") or {}
        if msg.get("type") == "agent_message":
            parts.append(str(msg.get("message") or ""))
    if not parts:
        return raw
    return "\n".join(p for p in parts if p)


def _do_task(args: dict, ctx: ToolCtx) -> ToolResult:
    prompt = str(args.get("prompt") or "")
    role = str(args.get("role") or "build")
    model_role = str(args.get("model_role") or "")
    worktree = bool(args.get("worktree"))
    backend = str(args.get("backend") or "")
    if not prompt:
        return ToolResult("error: prompt is required", is_error=True)
    if role not in ROLE_PROMPTS:
        role = "build"
    agent = ctx.agent
    if agent is None:
        return ToolResult("error: no agent context (headless tools only)", is_error=True)
    depth = ctx.depth
    max_depth = agent.config.max_subagent_depth
    if depth >= max_depth and not backend:
        return ToolResult(
            f"error: subagent depth limit reached ({max_depth}); do the work inline",
            is_error=True)

    root = agent.config.project_root
    cwd = ctx.cwd
    wt_path: Path | None = None
    if worktree:
        wt_path = _make_worktree(root, f"marfim-task-{agent.session.sid}-{id(args) % 99999}")
        if wt_path is not None:
            cwd = wt_path
        else:
            return ToolResult("error: could not create a git worktree for isolation",
                              is_error=True)

    if backend:
        spec = agent.config.external_agents.get(backend)
        if spec is None:
            known = ", ".join(sorted(agent.config.external_agents)) or "(none configured)"
            return ToolResult(f"error: unknown backend '{backend}'; configured: {known}",
                              is_error=True)
        out = _run_external(backend, spec, prompt, cwd)
    else:
        from marfim.agent.loop import Agent
        sub = Agent.spawn_subagent(parent=agent, role=role, cwd=cwd, depth=depth + 1,
                                   model_role=model_role)
        try:
            report = sub.run_to_completion(
                prompt, allowed_tools=ROLE_TOOLS.get(role), say=lambda m: None)
        except Exception as exc:  # noqa: BLE001
            if wt_path is not None:
                _remove_worktree(root, wt_path)
            return ToolResult(f"error: subagent failed: {type(exc).__name__}: {exc}",
                              is_error=True)
        prefix = f"[{role} subagent · {len(sub.session.messages)} messages · " \
                 f"{sub.usage.input_tokens + sub.usage.output_tokens} tokens]"
        out = ToolResult(f"{prefix}\n\n{report}")

    text = out.text
    if wt_path is not None:
        diff, stat = _worktree_diff(wt_path)
        if diff:
            text += ("\n\n[mergeable diff from worktree — apply with `git apply`]\n"
                     + truncate_middle(diff, 8000))
        elif stat:
            text += f"\n\n[worktree changes: {stat}]"
        else:
            text += "\n\n[worktree had no changes]"
        _remove_worktree(root, wt_path)
        text += f"\n[worktree removed: {wt_path}]"
    return ToolResult(text, is_error=out.is_error)


def build_tools() -> list[Tool]:
    return [
        Tool(name="task",
             description=("Spawn a subagent for an independent chunk of work. Params: "
                          "prompt (required), role (build|plan|explore|review), "
                          "model_role (optional routing), worktree (bool: isolate in a "
                          "git worktree; a mergeable diff is returned), backend (optional "
                          "external agent name from [external_agents]). The subagent "
                          "reports back its final summary; it cannot see this "
                          "conversation."),
             parameters={"type": "object", "properties": {
                 "prompt": {"type": "string"}, "role": {"type": "string",
                 "enum": ["build", "plan", "explore", "review"]},
                 "model_role": {"type": "string"}, "worktree": {"type": "boolean"},
                 "backend": {"type": "string"}},
                 "required": ["prompt"]},
             handler=_do_task),
    ]
