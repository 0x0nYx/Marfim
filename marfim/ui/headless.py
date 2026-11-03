"""Headless mode — `marfim run -p` (Claude Code headless doctrine).

Output formats: text | json | stream-json (JSONL events).
Honest exit codes: 0 ok, 1 agent error, 2 permission denial blocked the task.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

from marfim.agent.loop import Agent
from marfim.config import Config
from marfim.memory import collect_memory, discover_skills
from marfim.mcp.client import McpManager
from marfim.permissions import Mode, PermissionEngine
from marfim.providers.base import build_provider
from marfim.session.store import Session, find_session


def run_headless(config: Config, prompt: str, output_format: str = "text",
                 resume: str | None = None, max_turns: int | None = None,
                 stream_events: bool = False) -> int:
    config.ensure_dirs()
    if resume:
        target = find_session(config.session_dir, resume)
        if target is None:
            print(json.dumps({"error": f"session '{resume}' not found"}))
            return 1
        session = target
    else:
        session = Session.create(config.session_dir, str(config.project_root))

    permissions = PermissionEngine.from_config(config)
    mcp = McpManager()
    for name, srv in config.mcp_servers.items():
        if srv.command:
            mcp.register(name, srv.command, srv.args, srv.env, config.project_root)
    if mcp.clients:
        mcp.start_all(config.project_root)

    agent = Agent.create(
        config=config, session=session,
        provider=build_provider(config.provider(), config.default_model),
        permissions=permissions, skills=discover_skills(config),
        memory=collect_memory(config), mcp=mcp,
        say=(lambda ev: _emit(ev, "stream")) if output_format == "stream-json" else (lambda ev: None),
        on_permission=None)  # headless: ask → deny (documented)
    if max_turns:
        agent.config.max_turns = max_turns

    if output_format == "stream-json":
        _emit({"kind": "system", "session": session.sid, "model": agent.provider.model}, "stream")

    try:
        result = agent.run_turn(prompt)
    finally:
        mcp.stop_all()

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

    if result.stopped_reason in ("provider_error", "max_turns", "interrupted"):
        return 1
    return 0


def _emit(event: dict, _fmt: str) -> None:
    print(json.dumps({"type": event.get("kind", "event"), **event}, ensure_ascii=False,
                     default=str))
    sys.stdout.flush()
