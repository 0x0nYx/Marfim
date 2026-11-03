"""Interactive REPL — streaming UI with REAL mid-turn steering.

Review finding 28 (the stdin race): there is now exactly ONE input owner — a
single reader thread started once, whose lines flow through a queue.  The
permission prompt consumes its answer from the SAME queue, so nothing
competes in input().

Review finding 29 (steering/Ctrl-C unreachable): each turn runs in a worker
thread; the main thread stays on input; Ctrl-C sets the agent's stop_event
instead of tearing down the REPL.  Lines typed mid-turn are delivered as
steering at the next tool boundary (opencode v2 doctrine); `/`-lines typed
mid-turn are queued as commands for after the turn.

Lifecycle hooks (finding 31): SessionStart and Stop actually fire.
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
from marfim.router import Router
from marfim.session.store import Session
from marfim.ui import render, slash

EOF_SENTINEL = "\x04"


class Repl:
    def __init__(self, config: Config, session: Session | None = None,
                 resume_session: Session | None = None, prompt_prefix: str = "› ",
                 session_name: str = ""):
        self.config = config
        config.ensure_dirs()
        self.prompt_prefix = prompt_prefix
        self.memory = collect_memory(config)
        self.skills = discover_skills(config)
        self.commands = discover_commands(config)
        self.mcp = McpManager()
        for name, srv in config.mcp_servers.items():
            self.mcp.register(name, srv.command, srv.args, srv.env, config.project_root,
                              url=srv.url, headers=srv.headers)
        if self.mcp.clients:
            status = self.mcp.start_all(config.project_root)
            for name, state in status.items():
                render.render_info(f"mcp:{name} → {state}")
        self.permissions = PermissionEngine.from_config(config)
        if resume_session or session:
            self.session = resume_session or session
        else:
            self.session = Session.create(config.session_dir, str(config.project_root),
                                          title=session_name)
        self.router = Router(config)
        self.agent = Agent.create(
            config=config, session=self.session,
            provider=build_provider(config.provider(), config.default_model,
                                    max_output_tokens=config.max_output_tokens),
            permissions=self.permissions, skills=self.skills, memory=self.memory,
            mcp=self.mcp, say=self._on_event, on_permission=self._on_permission,
            router=self.router)
        self._input_queue: queue.Queue[str] = queue.Queue()
        self._ask_requests: queue.Queue[tuple] = queue.Queue()
        self._running_turn = False
        self._last_was_text = False
        self._streamed_this_turn = False

    # ------------------------------------------------------------------ events

    def _on_event(self, event: dict) -> None:
        kind = event.get("kind")
        if kind == "text_delta":
            render.render_stream(event.get("text", ""))
            self._last_was_text = True
            self._streamed_this_turn = True
        elif kind == "reasoning_delta":
            pass  # surfaced in verbose mode only; never part of the answer
        elif kind == "tool_start":
            if self._last_was_text:
                render.end_stream()
                self._last_was_text = False
            render.render_tool_start(event.get("name", "?"), event.get("args", {}))
        elif kind == "tool_result":
            render.render_tool_result(event.get("name", "?"), bool(event.get("error")))
        elif kind == "steer":
            render.render_info(f"⟳ steering delivered: {event.get('text', '')[:120]}")
        elif kind == "info":
            render.render_info(event.get("text", ""))

    # ------------------------------------------------------------- permissions

    def _on_permission(self, tool: str, args: dict, reason: str) -> str:
        """Runs on the TURN WORKER thread: marshal the question to the main
        thread, which owns the terminal (finding 28)."""
        ev = threading.Event()
        box: dict = {"answer": "deny"}
        self._ask_requests.put((tool, args, reason, ev, box))
        ev.wait()
        return box["answer"]

    def _prompt_via_queue(self, tool: str, args: dict, reason: str) -> str:
        """Main thread: display the question, read the answer via THE queue."""
        render.render_permission_header(tool, args, reason)
        while True:
            try:
                line = self._input_queue.get()
            except (KeyboardInterrupt, EOFError):
                answer = "deny"
                break
            line = line.strip().lower()
            if line in ("a", "always"):
                answer = "always"
                break
            if line in ("n", "no"):
                answer = "deny"
                break
            if line in ("y", "yes", ""):
                answer = "approve"
                break
            render.render_info("  answer with y / n / a")
        if answer == "always":
            rule = self.permissions.grant_always(tool, args)
            if rule:
                self.session.record("grant", {"rule": rule})
                render.render_info(f"  saved rule: {rule}")
            else:
                render.render_info("  (not persisted: interpreters/git/shell wrappers are "
                                   "never auto-granted — approving this call only)")
            return "approve"
        return answer

    # ------------------------------------------------------------------ input

    def _reader(self) -> None:
        """THE single input owner (finding 28)."""
        while True:
            try:
                line = input()
            except (EOFError, KeyboardInterrupt):
                self._input_queue.put(EOF_SENTINEL)
                return
            self._input_queue.put(line)

    def adopt_session(self, session: Session) -> None:
        self.session = session
        self.agent.session = session
        self.agent.tool_ctx()
        self._sync_grants(session)

    def _sync_grants(self, session: Session) -> None:
        for rule in session.grants:
            if rule not in self.permissions.session_grants:
                self.permissions.session_grants.append(rule)

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
        if self.config.untrusted_notice:
            render.render_error(self.config.untrusted_notice)
        if self.session.messages:
            render.render_info(f"resumed with {len(self.session.messages)} messages")
        self.agent.hooks.run("SessionStart", {"session": self.session.sid})  # finding 31
        reader = threading.Thread(target=self._reader, daemon=True, name="marfim-input")
        reader.start()
        while True:
            try:
                text = self._input_queue.get().strip()
            except (KeyboardInterrupt, EOFError):
                print()
                break
            if not text:
                continue
            if text in (EOF_SENTINEL, "/exit", "/quit"):
                break
            try:
                if slash.dispatch(self, text):
                    continue
            except SystemExit:
                break
            if self._handle_custom_command(text):
                continue
            self._run_turn(text)
        self.agent.hooks.run("Stop", {"session": self.session.sid})            # finding 31
        self.mcp.stop_all()
        render.render_info("session saved: " + self.session.sid)

    # ------------------------------------------------------------------ turns

    def _run_turn(self, text: str) -> None:
        """Run one turn in a WORKER thread; the main thread keeps answering
        permission prompts, steering, and Ctrl-C (findings 28/29)."""
        self._last_was_text = False
        self._streamed_this_turn = False
        self._running_turn = True
        result_box: dict = {}
        worker = threading.Thread(target=self._turn_worker, args=(text, result_box),
                                  daemon=True, name="marfim-turn")
        worker.start()
        follow_ups: list[str] = []
        try:
            while worker.is_alive():
                # 1. answer pending permission prompts
                try:
                    tool, args, reason, ev, box = self._ask_requests.get_nowait()
                    box["answer"] = self._prompt_via_queue(tool, args, reason)
                    ev.set()
                    continue
                except queue.Empty:
                    pass
                # 2. route user lines: steering or queued commands
                try:
                    line = self._input_queue.get(timeout=0.05)
                except queue.Empty:
                    continue
                if line == EOF_SENTINEL:
                    self.agent.stop_event.set()
                    render.render_info("⏹ interrupting…")
                elif line.startswith("/"):
                    follow_ups.append(line)
                else:
                    self.agent.steer(line)
                    render.render_info(f"⟳ steering queued: {line[:80]}")
        except KeyboardInterrupt:
            # Ctrl-C mid-turn interrupts the agent instead of killing the REPL
            self.agent.stop_event.set()
            render.render_info("⏹ interrupted (Ctrl-C)")
        worker.join(timeout=30)
        self._running_turn = False
        self.agent.stop_event.clear()
        if self._last_was_text:
            render.end_stream()
            self._last_was_text = False
        result = result_box.get("result")
        if result is None and "error" in result_box:
            render.render_error(f"turn crashed: {result_box['error']}")
            result = None
        if result is not None:
            if result.text and result.stopped_reason != "interrupted":
                if self._streamed_this_turn:
                    # the answer was already streamed live — do not print it twice
                    render.end_stream()
                else:
                    render.render_markdown(result.text)
            elif result.stopped_reason:
                render.render_error(result.text or result.stopped_reason)
        for follow in follow_ups:
            try:
                if slash.dispatch(self, follow):
                    continue
            except SystemExit:
                return
            self._run_turn(follow)

    def _turn_worker(self, text: str, box: dict) -> None:
        try:
            box["result"] = self.agent.run_turn(text)
        except Exception as exc:  # noqa: BLE001 — the REPL must survive anything
            box["error"] = f"{type(exc).__name__}: {exc}"
