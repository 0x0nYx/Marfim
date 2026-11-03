"""Provider abstraction: streaming chat with tool calls.

Event protocol (provider-neutral):
  {"type": "text_delta", "text": str}
  {"type": "tool_call",  "id": str, "name": str, "arguments": str}   # raw JSON
  {"type": "done", "usage": Usage, "stop_reason": str}
  {"type": "error", "message": str, "kind": str}

Message dicts follow the OpenAI shape internally and each provider adapter
translates on the wire (opencode's ~45-protocol insight, reduced to the two
wire protocols that cover the long tail + a mock for tests).
"""

from __future__ import annotations

import json
import time
import urllib.request
from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from typing import Iterator

from marfim.config import ProviderConfig
from marfim.util import estimate_tokens


@dataclass
class Usage:
    input_tokens: int = 0
    output_tokens: int = 0
    cached_tokens: int = 0

    @property
    def total(self) -> int:
        return self.input_tokens + self.output_tokens

    def add(self, other: "Usage") -> None:
        self.input_tokens += other.input_tokens
        self.output_tokens += other.output_tokens
        self.cached_tokens += other.cached_tokens


@dataclass
class ToolCall:
    id: str
    name: str
    arguments: str  # raw JSON string


@dataclass
class ToolSpec:
    name: str
    description: str
    parameters: dict  # JSON schema


class ProviderError(Exception):
    def __init__(self, message: str, kind: str = "provider", status: int = 0):
        super().__init__(message)
        self.kind = kind
        self.status = status


class Provider(ABC):
    name = "base"

    def __init__(self, config: ProviderConfig, model: str):
        self.config = config
        self.model = model

    @abstractmethod
    def stream(self, messages: list[dict], tools: list[ToolSpec],
               system: str = "") -> Iterator[dict]:
        """Yield events; raises ProviderError on transport failures."""

    # ------------------------------------------------------------------ util

    def _post(self, url: str, headers: dict, body: dict, timeout: int = 180):
        data = json.dumps(body).encode("utf-8")
        req = urllib.request.Request(url, data=data, headers=headers, method="POST")
        return urllib.request.urlopen(req, timeout=timeout)

    @staticmethod
    def _sse_lines(resp) -> Iterator[str]:
        for raw in resp:
            line = raw.decode("utf-8", "replace").rstrip("\n\r")
            yield line


def retry_iterator(fn, retry_max: int = 3, base_delay: float = 1.0):
    """Wrap a streaming call with exponential backoff (Codex retry doctrine).

    Retries transport-level errors and 429/5xx; never retries 4xx auth errors.
    """
    last_err: Exception | None = None
    for attempt in range(retry_max):
        try:
            return fn()
        except ProviderError as exc:
            last_err = exc
            if exc.status in (401, 403, 404, 422):
                raise
            if attempt < retry_max - 1:
                time.sleep(base_delay * (2 ** attempt))
        except (urllib.error.URLError, TimeoutError, ConnectionError, OSError) as exc:
            last_err = exc
            if attempt < retry_max - 1:
                time.sleep(base_delay * (2 ** attempt))
    raise ProviderError(f"provider failed after {retry_max} attempts: {last_err}",
                        kind="retry_exhausted")


def messages_tokens(messages: list[dict]) -> int:
    """Rough context estimate for compaction triggers."""
    total = 0
    for msg in messages:
        content = msg.get("content")
        if isinstance(content, str):
            total += estimate_tokens(content)
        elif isinstance(content, list):
            for block in content:
                if isinstance(block, dict):
                    total += estimate_tokens(json.dumps(block, default=str))
                else:
                    total += estimate_tokens(str(block))
        if msg.get("tool_calls"):
            total += estimate_tokens(json.dumps(msg["tool_calls"], default=str))
    return total


def build_provider(config: ProviderConfig, model: str) -> Provider:
    """Factory: wire protocol -> adapter (Codex wire_api doctrine)."""
    if config.wire == "anthropic":
        from marfim.providers.anthropic import AnthropicProvider
        return AnthropicProvider(config, model)
    if config.wire == "mock":
        from marfim.providers.mock import MockProvider
        return MockProvider(config, model)
    from marfim.providers.openai_compat import OpenAICompatProvider
    return OpenAICompatProvider(config, model)
