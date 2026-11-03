"""Interactive REPL — streaming UI with mid-turn steering.

Steering doctrine (opencode v2 / Oh My Pi): input typed while the agent runs
is captured by a reader thread; at the next safe step boundary it is delivered
to the running turn (steer) instead of being lost.
"""

from __future__ import annotations

import queue
import threading
from pathlib import Path

from marfim import __version__
from marfim.agent.loop import Agent
from marfim.config import Config
from marfim.memory import CustomCommand, collect_memory, discover_commands, discover_skills
from marfim.mcp.client import McpManager
from marfim.permissions import PermissionEngine
from marfim.providers.base import build_provider
from marfim.session.store import Session
from marfim.ui import render, slash


class Repl:
    def __init__(self, config: Config, session: Session | None = None,
                 resume_session: Session | None = None, prompt_prefix: str = "› "):
        self.config = config
        config.ensure_dirs()
        self.prompt_prefix = prompt_prefix
        self.memory = collect_memory(config)
        self.skills = discover_skills(config)
        self.commands = discover_commands(config)
        self.mcp = McpManager()
        for name, srv in config.mcp_servers.items():
            if srv.command:
                self.mcp.register(name, srv.command, srv.args, srv.env, config.project_root)
        if self.mcp.clients:
            status = self.mcp.start_all(config.project_root)
            for name, state in status.items():
                render.render_info(f"mcp:{name} → {state}")
        self.permissions = PermissionEngine.from_config(config)
        self.session = resume_session or session or Session.create(
            config.session_dir, str(config.project_root))
        self.agent = Agent.create(
            config=config, session=self.session,
            provider=build_provider(config.provider(), config.default_model),
            permissions=self.permissions, skills=self.skills, memory=self.memory,
            mcp=self.mcp, say=self._on_event, on_permission=self._on_permission)
        self._input_queue: queue.Queue[str] = queue.Queue()
        self._running_turn = False

    # ------------------------------------------------------------------ events

    def _on_event(self, event: dict) -> None:
        kind = event.get("kind")
        if kind == "text_delta":
            render.render_stream(event.get("text", ""))
        elif kind == "tool_start":
            if self._last_was_text:
                render.end_stream()
            self._last_was_text = False
            render.render_tool_start(event.get("name", "?"), event.get("args", {}))
        elif kind == "tool_result":
            render.render_tool_result(event.get("name", "?"), bool(event.get("error")))
        elif kind == "steer":
            render.render_info(f"⟳ steering delivered: {event.get('text', '')[:120]}")

    _last_was_text = False

    def _on_permission(self, tool: str, args: dict, reason: str) -> str:
        answer = render.render_permission_prompt(tool, args, reason)
        if answer == "always":
            rule = self.permissions.grant_always(tool, args)
            self.session.record("grant", {"rule": rule})
            render.render_info(f"  saved rule: {rule}")
            return "approve"
        return answer

    # ------------------------------------------------------------------ repl

    def adopt_session(self, session: Session) -> None:
        self.session = session
        self.agent.session = session
        self.agent.tool_ctx()
        self._sync_grants(session)

    def _sync_grants(self, session: Session) -> None:
        for rule in session.grants:
            if rule not in self.permissions.session_grants:
                self.permissions.session_grants.append(rule)

    def _reader(self) -> None:
        while True:
            try:
                line = input()
            except (EOFError, KeyboardInterrupt):
                self._input_queue.put("\x04")
                return
            self._input_queue.put(line)

    def _handle_custom_command(self, text: str) -> bool:
        if not text.startswith("!"):
            return False
        name, _, rest = text[1:].partition(" ")
        for cmd in self.commands:
            if cmd.name == name:
                rendered = cmd.render(rest)
                self._run_turn(rendered)
                return True
        return False

    def loop(self) -> None:
        render.render_banner()
        render.render_info(f"session {self.session.sid} · provider "
                           f"{self.agent.provider.config.name}/{self.agent.provider.model}"
                           f" · mode {self.permissions.mode.value}")
        if self.session.messages:
            render.render_info(f"resumed with {len(self.session.messages)} messages")
        reader = threading.Thread(target=self._reader, daemon=True)
        reader.start()
        while True:
            try:
                text = input(self.prompt_prefix).strip()
            except (EOFError, KeyboardInterrupt):
                print()
                break
            if not text:
                continue
            if text in ("\x04", "/exit", "/quit"):
                break
            try:
                if slash.dispatch(self, text):
                    continue
            except SystemExit:
                break
            if self._handle_custom_command(text):
                continue
            self._run_turn(text)
        self.mcp.stop_all()
        render.render_info("session saved: " + self.session.sid)

    # ------------------------------------------------------------------ turns

    def _run_turn(self, text: str) -> None:
        self._last_was_text = False
        self._running_turn = True
        reader = threading.Thread(target=self._reader, daemon=True)
        reader.start()
        try:
            result = self.agent.run_turn(text)
        finally:
            self._running_turn = False
            self.agent.stop_event.clear()
        if self._last_was_text:
            render.end_stream()
            self._last_was_text = False
        # deliver any queued steering as its own turn
        follow_ups: list[str] = []
        while True:
            try:
                line = self._input_queue.get_nowait()
            except queue.Empty:
                break
            if line == "\x04":
                self.agent.stop_event.set()
            elif self._running_turn:
                self.agent.steer(line)
            else:
                follow_ups.append(line)
        if result.text and result.stopped_reason != "interrupted":
            render.render_markdown(result.text)
        elif result.stopped_reason:
            render.render_error(result.text or result.stopped_reason)
        for follow in follow_ups:
            self._run_turn(follow)
