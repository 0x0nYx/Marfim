"""Anthropic Messages API wire protocol (SSE streaming).

Handles system-as-top-level, content blocks, tool_use/tool_result pairs and
usage with cache-read tokens.
"""

from __future__ import annotations

import json
import urllib.error
from typing import Iterator

from marfim.providers.base import Provider, ProviderError, ToolSpec, Usage


class AnthropicProvider(Provider):
    name = "anthropic"

    def stream(self, messages: list[dict], tools: list[ToolSpec],
               system: str = "") -> Iterator[dict]:
        def _call():
            return self._open(messages, tools, system)
        resp = _call()
        return self._consume(resp)

    # ------------------------------------------------------------------ wire

    def _open(self, messages: list[dict], tools: list[ToolSpec], system: str):
        url = self.config.base_url.rstrip("/") + "/v1/messages"
        wire_msgs = [self._translate_out(m) for m in messages]
        body: dict = {
            "model": self.model,
            "max_tokens": 16384,
            "stream": True,
            "messages": wire_msgs,
        }
        if system:
            body["system"] = system
        if tools:
            body["tools"] = [
                {"name": t.name, "description": t.description,
                 "input_schema": t.parameters or {"type": "object", "properties": {}}}
                for t in tools
            ]
        headers = {
            "Content-Type": "application/json",
            "x-api-key": self.config.key() or "no-key",
            "anthropic-version": "2023-06-01",
            "User-Agent": "marfim/0.1",
        }
        try:
            return self._post(url, headers, body)
        except urllib.error.HTTPError as exc:
            detail = ""
            try:
                detail = exc.read().decode("utf-8", "replace")[:400]
            except Exception:
                pass
            raise ProviderError(f"HTTP {exc.code}: {detail}", status=exc.code) from exc
        except (urllib.error.URLError, TimeoutError, ConnectionError, OSError) as exc:
            raise ProviderError(f"transport error: {exc}", kind="transport") from exc

    # ---------------------------------------------------------------- consume

    def _consume(self, resp) -> Iterator[dict]:
        usage = Usage()
        stop_reason = ""
        current_tool: dict | None = None
        tools_out: list[dict] = []
        try:
            for line in self._sse_lines(resp):
                if not line.startswith("data:"):
                    continue
                payload = line[5:].strip()
                if not payload:
                    continue
                evt = json.loads(payload)
                etype = evt.get("type")
                if etype == "message_start":
                    m = evt.get("message", {}).get("usage", {})
                    usage.input_tokens = int(m.get("input_tokens") or 0)
                    usage.cached_tokens = int((m.get("cache_read_input_tokens") or 0))
                elif etype == "content_block_start":
                    block = evt.get("content_block", {})
                    if block.get("type") == "tool_use":
                        current_tool = {"id": block.get("id", ""), "name": block.get("name", ""),
                                        "args": ""}
                elif etype == "content_block_delta":
                    delta = evt.get("delta", {})
                    if delta.get("type") == "text_delta":
                        yield {"type": "text_delta", "text": delta.get("text", "")}
                    elif delta.get("type") == "input_json_delta" and current_tool is not None:
                        current_tool["args"] += delta.get("partial_json", "")
                elif etype == "content_block_stop":
                    if current_tool is not None:
                        tools_out.append(current_tool)
                        current_tool = None
                elif etype == "message_delta":
                    d = evt.get("delta", {})
                    stop_reason = d.get("stop_reason") or stop_reason
                    u = evt.get("usage", {})
                    usage.output_tokens = int(u.get("output_tokens") or usage.output_tokens)
                elif etype == "error":
                    err = evt.get("error", {})
                    raise ProviderError(str(err.get("message", "stream error")), kind="stream")
        finally:
            try:
                resp.close()
            except Exception:
                pass
        for tool in tools_out:
            yield {"type": "tool_call", "id": tool["id"], "name": tool["name"],
                   "arguments": tool["args"] or "{}"}
        yield {"type": "done", "usage": usage, "stop_reason": stop_reason or "end_turn"}

    # -------------------------------------------------------------- translate

    def _translate_out(self, msg: dict) -> dict:
        role = msg.get("role", "user")
        if role == "tool":
            return {
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": msg.get("tool_call_id", ""),
                    "content": msg.get("content") if isinstance(msg.get("content"), str)
                    else json.dumps(msg.get("content"), default=str),
                }],
            }
        content = msg.get("content")
        tool_calls = msg.get("tool_calls") or []
        blocks: list[dict] = []
        if isinstance(content, str) and content:
            blocks.append({"type": "text", "text": content})
        for tc in tool_calls:
            args = tc.get("arguments", "{}")
            try:
                input_obj = json.loads(args) if isinstance(args, str) else args
            except json.JSONDecodeError:
                input_obj = {"raw": args}
            blocks.append({"type": "tool_use", "id": tc.get("id", ""),
                           "name": tc["name"], "input": input_obj})
        if not blocks:
            blocks = [{"type": "text", "text": ""}]
        return {"role": "assistant" if role == "assistant" else "user", "content": blocks}
