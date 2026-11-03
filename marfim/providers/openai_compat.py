"""OpenAI-compatible wire protocol (chat/completions, SSE streaming).

Covers OpenAI, OpenRouter, Groq, Together, DeepSeek, Mistral, xAI, Moonshot,
Zhipu, Google's OpenAI-compat endpoint, Fireworks, Cerebras, Perplexity,
Novita, Chutes, Ollama, LM Studio, vLLM and any custom base_url.
"""

from __future__ import annotations

import json
import urllib.error
from typing import Iterator

from marfim.providers.base import Provider, ProviderError, ToolSpec, Usage


class OpenAICompatProvider(Provider):
    name = "openai-compat"

    def stream(self, messages: list[dict], tools: list[ToolSpec],
               system: str = "") -> Iterator[dict]:
        def _call():
            return self._open(messages, tools, system)
        resp = _call()
        return self._consume(resp)

    # ------------------------------------------------------------------ wire

    def _open(self, messages: list[dict], tools: list[ToolSpec], system: str):
        url = self.config.base_url.rstrip("/") + "/chat/completions"
        wire_msgs: list[dict] = []
        if system:
            wire_msgs.append({"role": "system", "content": system})
        for msg in messages:
            wire_msgs.append(self._translate_out(msg))
        body: dict = {
            "model": self.model,
            "messages": wire_msgs,
            "stream": True,
            "stream_options": {"include_usage": True},
        }
        if tools:
            body["tools"] = [
                {"type": "function",
                 "function": {"name": t.name, "description": t.description,
                              "parameters": t.parameters or {"type": "object", "properties": {}}}}
                for t in tools
            ]
        headers = {
            "Content-Type": "application/json",
            "Authorization": f"Bearer {self._api_key()}",
            "User-Agent": "marfim/0.1",
        }
        # OpenRouter free-model attribution headers (optional, harmless elsewhere)
        if "openrouter" in self.config.base_url:
            headers["HTTP-Referer"] = "https://github.com/marfim"
            headers["X-Title"] = "Marfim"
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

    def _api_key(self) -> str:
        key = self.config.key()
        return key or "no-key"

    # ---------------------------------------------------------------- consume

    def _consume(self, resp) -> Iterator[dict]:
        tool_acc: dict[int, dict] = {}
        usage = Usage()
        stop_reason = ""
        try:
            for line in self._sse_lines(resp):
                if not line.startswith("data:"):
                    continue
                payload = line[5:].strip()
                if payload == "[DONE]":
                    break
                chunk = json.loads(payload)
                if "usage" in chunk and chunk["usage"]:
                    u = chunk["usage"]
                    usage = Usage(
                        input_tokens=int(u.get("prompt_tokens") or 0),
                        output_tokens=int(u.get("completion_tokens") or 0),
                        cached_tokens=int((u.get("prompt_tokens_details") or {}).get("cached_tokens") or 0),
                    )
                for choice in chunk.get("choices") or []:
                    delta = choice.get("delta") or {}
                    text = delta.get("content")
                    if text:
                        yield {"type": "text_delta", "text": text}
                    if delta.get("reasoning_content"):
                        yield {"type": "text_delta", "text": delta["reasoning_content"]}
                    for tc in delta.get("tool_calls") or []:
                        idx = int(tc.get("index") or 0)
                        acc = tool_acc.setdefault(idx, {"id": "", "name": "", "args": ""})
                        if tc.get("id"):
                            acc["id"] = tc["id"]
                        fn = tc.get("function") or {}
                        if fn.get("name"):
                            acc["name"] = fn["name"] if not acc["name"] else acc["name"]
                        if fn.get("arguments"):
                            acc["args"] += fn["arguments"]
                    if choice.get("finish_reason"):
                        stop_reason = choice["finish_reason"]
        finally:
            try:
                resp.close()
            except Exception:
                pass
        for idx in sorted(tool_acc):
            acc = tool_acc[idx]
            yield {"type": "tool_call",
                   "id": acc["id"] or f"call_{idx}",
                   "name": acc["name"],
                   "arguments": acc["args"] or "{}"}
        yield {"type": "done", "usage": usage, "stop_reason": stop_reason or "stop"}

    # -------------------------------------------------------------- translate

    def _translate_out(self, msg: dict) -> dict:
        """Internal message dict -> OpenAI wire format."""
        out = {"role": msg.get("role", "user")}
        content = msg.get("content")
        if content is not None:
            out["content"] = content
        if msg.get("tool_calls"):
            out["tool_calls"] = [
                {"id": tc.get("id", ""), "type": "function",
                 "function": {"name": tc["name"], "arguments": tc.get("arguments", "{}")}}
                for tc in msg["tool_calls"]
            ]
        if msg.get("role") == "tool":
            out = {"role": "tool", "tool_call_id": msg.get("tool_call_id", ""),
                   "content": content if isinstance(content, str) else json.dumps(content)}
        return out
