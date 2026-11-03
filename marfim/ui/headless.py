"""Headless mode — `marfim -p` (Claude Code headless doctrine).

Output formats: text | json | stream-json (JSONL events).
Honest exit codes: 0 ok · 1 agent error · 2 a permission denial blocked the
task (documented AND actually returned now — review finding 31).
Lifecycle hooks (finding 31): SessionStart and Stop fire here too.
"""

from __future__ import annotations

import json
import sys

from marfim.agent.loop import Agent
from marfim.config import Config
from marfim.memory import collect_memory, discover_skills
from marfim.mcp.client import McpManager
from marfim.permissions import Mode, PermissionEngine
from marfim.providers.base import build_provider
from marfim.router import Router
from marfim.session.store import Session, find_session


def run_headless(config: Config, prompt: str, output_format: str = "text",
                 resume: str | None = None, max_turns: int | None = None,
                 stream_events: bool = False, session_name: str = "") -> int:
    config.ensure_dirs()
    if config.untrusted_notice:
        print(f"warning: {config.untrusted_notice}", file=sys.stderr)
    if resume:
        target = find_session(config.session_dir, resume)
        if target is None:
            print(json.dumps({"error": f"session '{resume}' not found"}))
            return 1
        session = target
    else:
        session = Session.create(config.session_dir, str(config.project_root),
                                 title=session_name)

    permissions = PermissionEngine.from_config(config)
    mcp = McpManager()
    for name, srv in config.mcp_servers.items():
        mcp.register(name, srv.command, srv.args, srv.env, config.project_root,
                     url=srv.url, headers=srv.headers)
    if mcp.clients:
        mcp.start_all(config.project_root)

    agent = Agent.create(
        config=config, session=session,
        provider=build_provider(config.provider(), config.default_model,
                                max_output_tokens=config.max_output_tokens),
        permissions=permissions, skills=discover_skills(config),
        memory=collect_memory(config), mcp=mcp, router=Router(config),
        say=(lambda ev: _emit(ev, "stream")) if output_format == "stream-json" else (lambda ev: None),
        on_permission=None)  # headless: ask → deny (documented)
    if max_turns:
        agent.config.max_turns = max_turns

    if output_format == "stream-json":
        _emit({"kind": "system", "session": session.sid, "model": agent.provider.model}, "stream")

    agent.hooks.run("SessionStart", {"session": session.sid})
    exit_code = 0
    try:
        result = agent.run_turn(prompt)
    finally:
        agent.hooks.run("Stop", {"session": session.sid})
        mcp.stop_all()

    # finding 31: exit code 2 when a permission denial blocked the task
    permission_denied = any(
        m.get("role") == "tool" and m.get("is_error")
        and "permission denied" in str(m.get("content", "")).lower()
        for m in session.messages)

    if output_format == "json":
        payload = {
            "session": session.sid,
            "model": agent.provider.model,
            "text": result.text,
            "turns": result.turns,
            "tool_calls": result.tool_calls,
            "usage": {"input": agent.usage.input_tokens, "output": agent.usage.output_tokens},
            "stopped_reason": result.stopped_reason or "ok",
            "messages": session.messages,
        }
        print(json.dumps(payload, ensure_ascii=False, indent=2, default=str))
    elif output_format == "stream-json":
        _emit({"kind": "result", "text": result.text, "turns": result.turns,
               "tool_calls": result.tool_calls,
               "usage": {"input": agent.usage.input_tokens, "output": agent.usage.output_tokens},
               "stopped_reason": result.stopped_reason or "ok"}, "stream")
    else:
        if result.text:
            print(result.text)
        if result.stopped_reason not in ("", "ok"):
            print(f"[{result.stopped_reason}]", file=sys.stderr)

    if result.stopped_reason in ("provider_error", "compaction_failed"):
        exit_code = 1
    elif result.stopped_reason in ("max_turns", "interrupted", "length"):
        exit_code = 1
    elif permission_denied:
        exit_code = 2
    return exit_code


def _emit(event: dict, _fmt: str) -> None:
    print(json.dumps({"type": event.get("kind", "event"), **event}, ensure_ascii=False,
                     default=str))
    sys.stdout.flush()
