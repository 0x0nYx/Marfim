"""The agent loop — Marfim's heart.

Merged doctrine:
- goose: an ordered, effect-driven turn; every mutation flows through the
  permission engine; nothing executes without a ruling.
- codex: per-turn tool planning, honest retries, compaction before sampling.
- opencode v2: steering queue — user input arriving mid-turn is delivered at
  the next safe step boundary (steer) or queued for next turn.
- Claude Code: permission prompts inline; hooks veto at PreToolUse (exit 2).
- Aider: reflection cap on repeated failures; weak-model routing.
- Oh My Pi: stale-read rejection is structural (GateGuard in fs tools).
- Hermes: usage ledger per turn; auxiliary-model routing for titles/commits.
"""

from __future__ import annotations

import json
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path

from marfim.agent import compaction
from marfim.agent.prompts import build_system_prompt
from marfim.config import Config
from marfim.hooks import HookBlocked, HookRunner
from marfim.memory import MemorySection, Skill
from marfim.permissions import Decision, Mode, PermissionEngine
from marfim.providers.base import Provider, ProviderError, ToolCall, Usage, build_provider, \
    messages_tokens, retry_iterator
from marfim.session.store import Session
from marfim.tools.base import Tool, ToolCtx, ToolResult
from marfim.tools.registry import default_tools, get_tool
from marfim.util import repair_json_args


@dataclass
class TurnResult:
    text: str = ""
    tool_calls: int = 0
    turns: int = 0
    usage: Usage = field(default_factory=Usage)
    stopped_reason: str = ""


class Agent:
    def __init__(self, config: Config, session: Session, provider: Provider,
                 permissions: PermissionEngine | None = None,
                 skills: list[Skill] | None = None,
                 memory: list[MemorySection] | None = None,
                 mcp=None, depth: int = 0, role: str = "build",
                 say=None, on_permission=None):
        self.config = config
        self.session = session
        self.provider = provider
        self.permissions = permissions or PermissionEngine.from_config(config)
        self.skills = skills or []
        self.memory = memory or []
        self.role = role
        self.depth = depth
        self.mcp = mcp
        self.hooks = HookRunner(config)
        self.usage = Usage()
        self.stop_event = threading.Event()
        self.say = say or (lambda _msg: None)
        self.on_permission = on_permission  # (tool, args, ruling) -> "approve"|"deny"|"always"
        self._steer_queue: list[str] = []
        self._tools: list[Tool] | None = None
        self._ctx: ToolCtx | None = None
        self.system_prompt = build_system_prompt(config, self.memory, self.skills, role)

    # ------------------------------------------------------------- factories

    @classmethod
    def create(cls, config: Config, session: Session, provider: Provider,
               permissions: PermissionEngine | None = None, skills=None, memory=None,
               mcp=None, say=None, on_permission=None, role: str = "build",
               depth: int = 0) -> "Agent":
        return cls(config=config, session=session, provider=provider,
                   permissions=permissions, skills=skills, memory=memory, mcp=mcp,
                   depth=depth, role=role, say=say, on_permission=on_permission)

    @classmethod
    def spawn_subagent(cls, parent: "Agent", role: str, cwd: Path, depth: int,
                       model_role: str = "") -> "Agent":
        """Child agent: own session file, role prompt, restricted cwd."""
        config = parent.config
        session = Session.create(config.session_dir, str(cwd), title=f"{role} subagent")
        provider = parent.provider
        if model_role:
            routed = config.model_roles.get(model_role) or ""
            if routed and "/" in routed:
                prov_name, _, model = routed.partition("/")
                from marfim.providers.base import build_provider as bp
                provider = bp(config.provider(prov_name), model)
        sub = cls(config=config, session=session, provider=provider,
                  permissions=parent.permissions, skills=parent.skills, memory=[],
                  depth=depth, role=role, say=lambda _m: None,
                  on_permission=parent.on_permission)
        sub._tools_override = None
        return sub

    # ----------------------------------------------------------------- tools

    def build_toolset(self, allowed: list[str] | None = None) -> list[Tool]:
        if self._tools is None:
            self._tools = default_tools(self.skills, self.mcp)
        if allowed is None:
            return list(self._tools)
        return [t for t in self._tools if t.name in allowed]

    def tool_ctx(self, cwd: Path | None = None) -> ToolCtx:
        if self._ctx is None or (cwd and cwd != self._ctx.cwd):
            self._ctx = ToolCtx(
                cwd=cwd or self.config.project_root, session=self.session, agent=self,
                say=self.say, depth=self.depth)
        return self._ctx

    # ------------------------------------------------------------- execution

    def run_to_completion(self, user_input: str, allowed_tools: list[str] | None = None,
                          say=None) -> str:
        """Subagent entry: run until final text, return it."""
        if say is not None:
            self.say = say
        result = self.run_turn(user_input, allowed_tools=allowed_tools)
        return result.text

    def run_turn(self, user_input: str, allowed_tools: list[str] | None = None) -> TurnResult:
        """One full user request: stream, dispatch tools, loop until final text."""
        tools = self.build_toolset(allowed_tools)
        ctx = self.tool_ctx()
        if self.session.messages:
            # resumed conversation: keep prior history
            pass
        self.session.append_message({"role": "user", "content": user_input})

        result = TurnResult()
        max_turns = self.config.max_turns
        failures = 0

        for turn in range(max_turns):
            if self.stop_event.is_set():
                result.stopped_reason = "interrupted"
                break
            result.turns = turn + 1
            messages = self._messages_with_compaction()
            events = self._stream_events(messages, tools)
            text_parts: list[str] = []
            tool_calls: list[ToolCall] = []
            usage = Usage()
            error: ProviderError | None = None
            try:
                for event in events:
                    etype = event.get("type")
                    if etype == "text_delta":
                        text_parts.append(event.get("text", ""))
                        self.say({"kind": "text_delta", "text": event.get("text", "")})
                    elif etype == "tool_call":
                        tool_calls.append(ToolCall(id=event.get("id", ""),
                                                   name=event.get("name", ""),
                                                   arguments=event.get("arguments", "{}")))
                    elif etype == "done":
                        usage = event.get("usage") or Usage()
                    elif etype == "error":
                        error = ProviderError(event.get("message", "stream error"))
                        break
            except ProviderError as exc:
                error = exc

            if error is not None:
                failures += 1
                if failures > self.config.retry_max:
                    result.text = "provider failed: " + error.args[0]
                    result.stopped_reason = "provider_error"
                    self.session.append_message({"role": "assistant", "content": result.text})
                    break
                time.sleep(1.0 * failures)
                continue
            failures = 0

            text = "".join(text_parts)
            self.usage.add(usage)
            self.session.add_usage(usage)
            assistant_msg: dict = {"role": "assistant", "content": text or None}
            if tool_calls:
                assistant_msg["tool_calls"] = [
                    {"id": tc.id, "name": tc.name, "arguments": tc.arguments}
                    for tc in tool_calls]
            self.session.append_message(assistant_msg)

            if not tool_calls:
                result.text = text
                # steering queue becomes the next user turn driver
                self._flush_steering_into_session()
                break

            # dispatch tools (safe step boundary: deliver steering here)
            steer = self._drain_steering()
            if steer:
                self.say({"kind": "steer", "text": steer})
                self.session.append_message({"role": "user", "content": steer})

            for tc in tool_calls:
                if self.stop_event.is_set():
                    result.stopped_reason = "interrupted"
                    break
                tool_result = self._execute_tool(tc, tools, ctx)
                result.tool_calls += 1
                self.session.append_message({
                    "role": "tool", "tool_call_id": tc.id, "name": tc.name,
                    "content": tool_result.text,
                    "is_error": tool_result.is_error})
                self.say({"kind": "tool_result", "name": tc.name,
                          "error": tool_result.is_error})
        else:
            result.text = result.text or "(max turns reached)"
            result.stopped_reason = result.stopped_reason or "max_turns"

        if result.stopped_reason == "interrupted" and not result.text:
            result.text = "(interrupted)"
        return result

    # -------------------------------------------------------------- internals

    def _stream_events(self, messages: list, tools: list[Tool]):
        specs = [t.spec() for t in tools]
        return retry_iterator(
            lambda: self.provider.stream(messages, specs, system=self.system_prompt),
            retry_max=self.config.retry_max)

    def _messages_with_compaction(self) -> list:
        messages = list(self.session.messages)
        window = compaction.context_window_for(
            self.provider.model) if self.provider.model else 200_000
        if compaction.needs_compaction(messages, window, self.config.compact_threshold):
            self.hooks.run("PreCompact", {"session": self.session.sid})
            def _complete(msgs):
                out_text = ""
                for event in self.provider.stream(msgs, [], system="You compress conversations."):
                    if event.get("type") == "text_delta":
                        out_text += event.get("text", "")
                return out_text or "(empty summary)"
            new_messages, _summary = compaction.compact_messages(messages, _complete)
            # persist compaction boundary as a session reset + summary
            self.session.record("summary", {"summary": _summary})
            self.session.messages = new_messages
            self.session.record("truncate", {"keep": len(new_messages)})
            return new_messages
        return compaction.prune_tool_results(messages)

    def _execute_tool(self, tc: ToolCall, tools: list[Tool], ctx: ToolCtx) -> ToolResult:
        tool = get_tool(tools, tc.name)
        args = repair_json_args(tc.arguments)
        preview = {k: (f"<{len(str(v))} chars>" if k in ("content", "patch", "old_string",
                                                         "new_string", "command") and len(str(v)) > 80
                       else v) for k, v in list(args.items())[:6]}
        self.say({"kind": "tool_start", "name": tc.name, "args": preview})

        if tool is None:
            return ToolResult(f"error: unknown tool '{tc.name}'", is_error=True)

        try:
            self.hooks.run("PreToolUse", HookRunner.tool_payload(tc.name, args, "PreToolUse"),
                           cwd=ctx.cwd)
        except HookBlocked as exc:
            return ToolResult(f"error: blocked by PreToolUse hook: {exc.message}", is_error=True)

        ruling = self.permissions.check(tc.name, args, self.config.project_root)
        if ruling.decision is Decision.DENY:
            return ToolResult(f"error: permission denied — {ruling.reason}", is_error=True)
        if ruling.decision is Decision.ASK:
            answer = self._ask_permission(tc.name, args, ruling.reason)
            if answer == "deny":
                return ToolResult("error: user denied this action", is_error=True)

        # file mutation checkpointing (Claude Code checkpoints)
        if tc.name in ("write", "edit", "patch"):
            self._snapshot_targets(tc.name, args)

        result = tool.run(args, ctx)
        try:
            self.hooks.run("PostToolUse", HookRunner.tool_payload(
                tc.name, args, "PostToolUse") | {"result": result.text[:2000]}, cwd=ctx.cwd)
        except HookBlocked:
            pass
        return result

    def _snapshot_targets(self, tool_name: str, args: dict) -> None:
        paths: list[str] = []
        if tool_name == "patch":
            from marfim.tools.patch import parse_patch, PatchError
            try:
                for op in parse_patch(str(args.get("patch") or "")):
                    paths.append(op["path"])
            except PatchError:
                pass
        else:
            p = args.get("file_path") or args.get("path")
            if p:
                paths.append(str(p))
        for raw in paths:
            path = Path(raw).expanduser()
            if not path.is_absolute():
                path = self.config.project_root / path
            if path.exists():
                self.session.snapshot_file(path, self.config.project_root)

    def _ask_permission(self, tool: str, args: dict, reason: str) -> str:
        if self.on_permission is None:
            # headless default: deny with guidance (Claude headless doctrine)
            return "deny"
        return self.on_permission(tool, args, reason) or "deny"

    # ------------------------------------------------------------- steering

    def steer(self, text: str) -> None:
        """Called from the UI thread when the user types mid-turn."""
        self._steer_queue.append(text)

    def _drain_steering(self) -> str:
        if not self._steer_queue:
            return ""
        msg = "\n".join(self._steer_queue)
        self._steer_queue = []
        return f"[mid-turn steering] {msg}"

    def _flush_steering_into_session(self) -> None:
        steer = self._drain_steering()
        if steer:
            self.session.append_message({"role": "user", "content": steer})

    # ---------------------------------------------------------------- usage

    @property
    def usage_in(self) -> int:
        return self.usage.input_tokens + (0 if self.session is None else 0)

    def usage_totals(self) -> tuple[int, int]:
        return self.usage.input_tokens, self.usage.output_tokens
