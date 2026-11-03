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

Review fixes carried here:
- 15: tool arguments are schema-validated before dispatch.
- 19: unparseable tool-call JSON surfaces the PARSE error, not a misleading
  "argument required" error.
- 20: interrupts leave NO dangling tool calls — synthetic `interrupted`
  results are appended on every exit path.
- 22: failed/empty compaction keeps the history (never replaces it with
  nothing); compaction runs on a turn boundary.
- 23: context windows come from provider-qualified model ids via the router,
  cached catalog, no network fetch in the loop.
- 24: `reasoning` events are kept out of the answer/history; `length`
  stop-reason is handled honestly; volatile env moved out of the system
  prompt (cache discipline) into the first user message.
- 25: subagents get their own cwd, memory, role prompt engine, and the
  parent's stop_event.
- §4.4: task calls in one turn run in a bounded thread pool.
- §4.5: verify-driven reflection loop, bounded by config.reflection_cap.
"""

from __future__ import annotations

import json
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field
from pathlib import Path

from marfim.agent import compaction
from marfim.agent.prompts import build_system_prompt, volatile_environment_block
from marfim.config import Config
from marfim.hooks import HookBlocked, HookRunner
from marfim.memory import MemorySection, Skill
from marfim.permissions import Decision, Mode, PermissionEngine
from marfim.providers.base import Provider, ProviderError, ToolCall, Usage, \
    retry_iterator
from marfim.router import Router
from marfim.session.store import Session
from marfim.tools.base import Tool, ToolCtx, ToolResult, validate_args
from marfim.tools.registry import default_tools, get_tool
from marfim.util import repair_json_args

MUTATION_TOOLS = ("write", "edit", "patch", "multi_edit")


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
                 say=None, on_permission=None, stop_event: threading.Event | None = None,
                 router: Router | None = None, default_cwd: Path | None = None):
        self.config = config
        self.session = session
        self.provider = provider
        self.permissions = permissions or PermissionEngine.from_config(config)
        self.skills = skills or []
        self.memory = memory or []
        self.role = role
        self.depth = depth
        self.mcp = mcp
        self.router = router
        self.default_cwd = default_cwd
        self.hooks = HookRunner(config)
        self.usage = Usage()
        self.stop_event = stop_event or threading.Event()
        self.say = say or (lambda _msg: None)
        self.on_permission = on_permission  # (tool, args, ruling) -> "approve"|"deny"|"always"
        self._steer_queue: list[str] = []
        self._queue_lock = threading.Lock()
        self._tools: list[Tool] | None = None
        self._ctx: ToolCtx | None = None
        self.system_prompt = build_system_prompt(config, self.memory, self.skills, role)
        self._reflection_used = 0

    # ------------------------------------------------------------- factories

    @classmethod
    def create(cls, config: Config, session: Session, provider: Provider,
               permissions: PermissionEngine | None = None, skills=None, memory=None,
               mcp=None, say=None, on_permission=None, role: str = "build",
               depth: int = 0, stop_event: threading.Event | None = None,
               router: Router | None = None, default_cwd: Path | None = None) -> "Agent":
        return cls(config=config, session=session, provider=provider,
                   permissions=permissions, skills=skills, memory=memory, mcp=mcp,
                   depth=depth, role=role, say=say, on_permission=on_permission,
                   stop_event=stop_event, router=router, default_cwd=default_cwd)

    @classmethod
    def spawn_subagent(cls, parent: "Agent", role: str, cwd: Path, depth: int,
                       model_role: str = "") -> "Agent":
        """Child agent: own session file, role prompt, restricted cwd, role
        permission engine, shared stop_event (review finding 25)."""
        config = parent.config
        session = Session.create(config.session_dir, str(cwd), title=f"{role} subagent")
        provider = parent.provider
        if model_role:
            candidates = (config.model_roles.get(model_role) or "").strip()
            if candidates:
                first = candidates.split(",")[0].strip()
                if first and "/" in first:
                    prov_name, _, model = first.partition("/")
                    from marfim.providers.base import build_provider as bp
                    provider = bp(config.provider(prov_name), model,
                                  max_output_tokens=config.max_output_tokens)
        sub = cls(config=config, session=session, provider=provider,
                  permissions=parent.permissions.child_for_role(
                      role, _role_tools(role)),
                  skills=parent.skills, memory=parent.memory,
                  depth=depth, role=role, say=lambda _m: None,
                  on_permission=parent.on_permission,
                  stop_event=parent.stop_event, router=parent.router,
                  default_cwd=cwd)
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
                cwd=cwd or self.default_cwd or self.config.project_root,
                session=self.session, agent=self,
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
        # finding 31: the engine consults the LIVE toolset's readonly flags,
        # so webfetch/websearch stop prompting in default mode and work in plan
        self.permissions.readonly_tools = {t.name for t in tools if t.readonly}
        if not self.session.messages:
            # cache discipline (finding 24): volatile environment lives in the
            # first user message, not the (byte-stable) system prompt
            env = volatile_environment_block(self.config)
            if env:
                user_input = f"<environment>\n{env}\n</environment>\n\n{user_input}"
        self.session.append_message({"role": "user", "content": user_input})

        result = TurnResult()
        max_turns = self.config.max_turns
        failures = 0
        self._reflection_used = 0

        for turn in range(max_turns):
            if self.stop_event.is_set():
                result.stopped_reason = result.stopped_reason or "interrupted"
                break
            result.turns = turn + 1
            messages, compaction_ok = self._messages_with_compaction()
            if not compaction_ok:
                result.stopped_reason = "compaction_failed"
                result.text = result.text or "(compaction failed; history kept; free context and /compact manually)"
                break
            text_parts: list[str] = []
            tool_calls: list[ToolCall] = []
            usage = Usage()
            error: ProviderError | None = None
            # a failure at stream-OPEN time must enter the same honest retry
            # path as mid-stream failures — never escape as a traceback
            try:
                events = self._stream_events(messages, tools)
            except ProviderError as exc:
                error = exc
                events = iter(())
            stop_reason = ""
            try:
                for event in events:
                    etype = event.get("type")
                    if etype == "text_delta":
                        text_parts.append(event.get("text", ""))
                        self.say({"kind": "text_delta", "text": event.get("text", "")})
                    elif etype == "reasoning":
                        # finding 24: CoT is surfaced, never stored as answer
                        self.say({"kind": "reasoning_delta", "text": event.get("text", "")})
                    elif etype == "tool_call":
                        tool_calls.append(ToolCall(id=event.get("id", ""),
                                                   name=event.get("name", ""),
                                                   arguments=event.get("arguments", "{}")))
                    elif etype == "done":
                        usage = event.get("usage") or Usage()
                        stop_reason = event.get("stop_reason", "")
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
            if self.router is not None:
                self.router.add_spend("default", self.provider.config.name,
                                      self.provider.model, usage.input_tokens,
                                      usage.output_tokens)
            assistant_msg: dict = {"role": "assistant", "content": text or None}
            if tool_calls:
                assistant_msg["tool_calls"] = [
                    {"id": tc.id, "name": tc.name, "arguments": tc.arguments}
                    for tc in tool_calls]
            self.session.append_message(assistant_msg)

            if not tool_calls:
                result.text = text
                if stop_reason == "length":
                    # finding 24: handle honestly instead of ignoring
                    result.stopped_reason = "length"
                    result.text += "\n[max output length reached — ask me to continue]"
                # steering queue becomes the next user turn driver
                self._flush_steering_into_session()
                break

            # dispatch tools (safe step boundary: deliver steering here)
            steer = self._drain_steering()
            if steer:
                self.say({"kind": "steer", "text": steer})
                self.session.append_message({"role": "user", "content": steer})
            self._turn_batch_started = False

            executed, interrupted = self._dispatch_tools(tool_calls, tools, ctx, result)
            if interrupted:
                result.stopped_reason = "interrupted"
                break

            # §4.5 verify-driven reflection (bounded)
            if (self.config.verify_commands and self.role == "build"
                    and not self.stop_event.is_set()
                    and any(name in MUTATION_TOOLS for name in executed)
                    and self._reflection_used < self.config.reflection_cap):
                from marfim.tools.verify import run_verify
                vr = run_verify(ctx)
                self._reflection_used += 1
                self.session.append_message({
                    "role": "user",
                    "content": f"[marfim verify · {'PASSED' if not vr.is_error else 'FAILED'}]\n"
                               f"{vr.text}"})
                if vr.is_error:
                    self.say({"kind": "info", "text": "verification failed — reflecting"})
                    continue
                break
        else:
            result.text = result.text or "(max turns reached)"
            result.stopped_reason = result.stopped_reason or "max_turns"

        if result.stopped_reason == "interrupted" and not result.text:
            result.text = "(interrupted)"
        return result

    # -------------------------------------------------------------- dispatch

    def _dispatch_tools(self, tool_calls: list[ToolCall], tools: list[Tool],
                        ctx: ToolCtx, result: TurnResult) -> tuple[list[str], bool]:
        """Execute tool calls in order; consecutive `task` calls run in a
        bounded thread pool (§4.4).  Returns (executed_names, interrupted).
        Every exit path leaves NO unanswered tool call (finding 20)."""
        executed: list[str] = []
        i = 0
        while i < len(tool_calls):
            if self.stop_event.is_set():
                # synthetic results for everything unanswered
                self._fill_interrupted(tool_calls[i:], executed_ids=set())
                return executed, True
            tc = tool_calls[i]
            if tc.name == "task" and self.config.max_parallel_tasks > 1:
                group: list[ToolCall] = []
                while i < len(tool_calls) and tool_calls[i].name == "task":
                    group.append(tool_calls[i])
                    i += 1
                if len(group) > 1:
                    executed += self._run_parallel_tasks(group, tools, ctx, result)
                    continue
                executed.append(self._run_one_tool(tc, tools, ctx))
                result.tool_calls += 1
                continue
            executed.append(self._run_one_tool(tc, tools, ctx))
            result.tool_calls += 1
            i += 1
        return executed, False

    def _run_one_tool(self, tc: ToolCall, tools: list[Tool], ctx: ToolCtx) -> str:
        tool_result = self._execute_tool(tc, tools, ctx)
        self.session.append_message({
            "role": "tool", "tool_call_id": tc.id, "name": tc.name,
            "content": tool_result.text,
            "is_error": tool_result.is_error})
        self.say({"kind": "tool_result", "name": tc.name,
                  "error": tool_result.is_error})
        return tc.name

    def _run_parallel_tasks(self, group: list[ToolCall], tools: list[Tool],
                            ctx: ToolCtx, result: TurnResult) -> list[str]:
        """Bounded thread pool for consecutive task calls; results are recorded
        in call order; Ctrl-C cancels the rest (§4.4)."""
        max_workers = max(1, min(self.config.max_parallel_tasks, len(group)))
        executed: list[str | None] = [None] * len(group)
        with ThreadPoolExecutor(max_workers=max_workers,
                                thread_name_prefix="marfim-task") as pool:
            futures = {pool.submit(self._execute_tool, tc, tools, ctx): idx
                       for idx, tc in enumerate(group)}
            for future, idx in futures.items():
                if self.stop_event.is_set():
                    # cancel the rest; the executor's already-submitted work
                    # finishes, but we stop accepting new results
                    for f in futures:
                        f.cancel()
                    break
                try:
                    tool_result = future.result()
                except Exception as exc:  # noqa: BLE001 — a task crash is a tool error
                    tool_result = ToolResult(f"error: task crashed: {type(exc).__name__}: {exc}",
                                             is_error=True)
                tc = group[idx]
                self.session.append_message({
                    "role": "tool", "tool_call_id": tc.id, "name": tc.name,
                    "content": tool_result.text,
                    "is_error": tool_result.is_error})
                self.say({"kind": "tool_result", "name": tc.name,
                          "error": tool_result.is_error})
                executed[idx] = tc.name
                result.tool_calls += 1
        # synthetic interrupted results for anything unrecorded (finding 20)
        for idx, tc in enumerate(group):
            if executed[idx] is None:
                self.session.append_message({
                    "role": "tool", "tool_call_id": tc.id, "name": tc.name,
                    "content": "interrupted", "is_error": True})
        return [name for name in executed if name]

    def _fill_interrupted(self, pending: list[ToolCall], executed_ids: set) -> None:
        for tc in pending:
            if tc.id in executed_ids:
                continue
            self.session.append_message({
                "role": "tool", "tool_call_id": tc.id, "name": tc.name,
                "content": "interrupted", "is_error": True})

    # -------------------------------------------------------------- internals

    def _stream_events(self, messages: list, tools: list[Tool]):
        specs = [t.spec() for t in tools]
        if self.router is not None and (self.config.model_roles.get("default") or "").strip():
            return self.router.stream_with_failover(messages, specs,
                                                    self.system_prompt, role="default")
        return retry_iterator(
            lambda: self.provider.stream(messages, specs, system=self.system_prompt),
            retry_max=self.config.retry_max)

    def _messages_with_compaction(self) -> tuple[list, bool]:
        """Returns (messages, ok).  A failed or empty compaction keeps the
        history and reports failure instead of replacing history with nothing
        (review finding 22)."""
        messages = list(self.session.messages)
        if self.router is not None:
            window = self.router.context_window(self.provider.config.name,
                                                self.provider.model)
        else:
            # provider-qualified lookup against the cached catalog (finding 23)
            window = compaction.context_window_for(
                f"{self.provider.config.name}/{self.provider.model}")
        if not compaction.needs_compaction(messages, window, self.config.compact_threshold):
            return compaction.prune_tool_results(messages), True
        self.hooks.run("PreCompact", {"session": self.session.sid})
        try:
            def _complete(msgs):
                out_text = ""
                for event in self.provider.stream(msgs, [],
                                                  system="You compress conversations."):
                    etype = event.get("type")
                    if etype == "text_delta":
                        out_text += event.get("text", "")
                    elif etype == "error":
                        raise ProviderError(event.get("message", "summary stream error"))
                if not out_text.strip():
                    raise ProviderError("empty summary from provider")
                return out_text

            new_messages, summary = compaction.compact_messages(messages, _complete)
        except (ProviderError, ValueError) as exc:
            self.say({"kind": "info",
                      "text": f"compaction failed ({exc}); continuing with full history"})
            return compaction.prune_tool_results(messages), True
        # persist the compaction boundary as ONE record load can replay
        # (finding 21).  An empty summary means compaction was a no-op —
        # record nothing, or resume would corrupt the history.
        if summary:
            self.session.record("compact", {"summary": summary, "keep": len(new_messages)})
            self.session.messages = new_messages
        return list(new_messages), True

    def _execute_tool(self, tc: ToolCall, tools: list[Tool], ctx: ToolCtx) -> ToolResult:
        tool = get_tool(tools, tc.name)
        args = repair_json_args(tc.arguments)
        preview = {k: (f"<{len(str(v))} chars>" if k in ("content", "patch", "old_string",
                                                         "new_string", "command") and len(str(v)) > 80
                       else v) for k, v in list(args.items())[:6]}
        self.say({"kind": "tool_start", "name": tc.name, "args": preview})

        if tool is None:
            return ToolResult(f"error: unknown tool '{tc.name}'", is_error=True)

        # finding 19: a parse failure must say so, not "file_path is required"
        if len(args) == 1 and "raw" in args and str(tc.arguments or "").strip():
            return ToolResult(
                f"error: tool-call arguments are not valid JSON ({str(tc.arguments)[:200]}"
                f"…); re-issue the call with a valid JSON object", is_error=True)

        # finding 15: schema validation before dispatch
        err = validate_args(tool, args)
        if err:
            return ToolResult(f"error: {err}; re-issue the call with correct arguments",
                              is_error=True)

        try:
            # fail_closed: a PreToolUse hook that errors/hangs must deny the
            # call, never wave it through (review finding 33)
            self.hooks.run("PreToolUse", HookRunner.tool_payload(tc.name, args, "PreToolUse"),
                           cwd=ctx.cwd, fail_closed=True)
        except HookBlocked as exc:
            return ToolResult(f"error: blocked by PreToolUse hook: {exc.message}", is_error=True)

        ruling = self.permissions.check(tc.name, args, self.config.project_root)
        if ruling.decision is Decision.DENY:
            return ToolResult(f"error: permission denied — {ruling.reason}", is_error=True)
        if ruling.decision is Decision.ASK:
            answer = self._ask_permission(tc.name, args, ruling.reason)
            if answer == "deny":
                return ToolResult("error: permission denied — the user denied this action",
                                  is_error=True)

        # file mutation checkpointing (Claude Code checkpoints).  One batch
        # per TURN: every mutation a turn makes is rewound together (the
        # batch starts lazily at the turn's first mutation).
        if tc.name in MUTATION_TOOLS:
            if not getattr(self, "_turn_batch_started", False):
                self.session.next_batch()
                self._turn_batch_started = True
            self._snapshot_targets(tc.name, args, ctx)

        result = tool.run(args, ctx)
        try:
            self.hooks.run("PostToolUse", HookRunner.tool_payload(
                tc.name, args, "PostToolUse") | {"result": result.text[:2000]}, cwd=ctx.cwd)
        except HookBlocked:
            pass
        return result

    def _snapshot_targets(self, tool_name: str, args: dict, ctx: ToolCtx) -> None:
        paths: list[str] = []
        if tool_name in ("patch", "apply_patch"):
            from marfim.tools.patch import patch_paths, PatchError
            try:
                paths = patch_paths(str(args.get("patch") or ""))
            except PatchError:
                paths = []
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
        with self._queue_lock:
            self._steer_queue.append(text)

    def _drain_steering(self) -> str:
        with self._queue_lock:
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

    def usage_totals(self) -> tuple[int, int]:
        return self.usage.input_tokens, self.usage.output_tokens


def _role_tools(role: str) -> list[str] | None:
    from marfim.tools.task import ROLE_TOOLS
    return ROLE_TOOLS.get(role)
