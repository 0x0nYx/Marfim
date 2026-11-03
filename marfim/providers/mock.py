"""Deterministic mock provider — the test/CI harness (opencode `simulation`
backend doctrine: scripted provider for deterministic tests).

Scripts are lists of event lists; each turn pops the next script entry.
"""

from __future__ import annotations

from typing import Iterator

from marfim.providers.base import Provider, ToolSpec, Usage


class MockProvider(Provider):
    name = "mock"

    def __init__(self, config, model: str, script: list[list[dict]] | None = None):
        super().__init__(config, model)
        self.script = script or []
        self.cursor = 0
        self.requests: list[dict] = []

    def stream(self, messages: list[dict], tools: list[ToolSpec],
               system: str = "") -> Iterator[dict]:
        self.requests.append({"messages": [dict(m) for m in messages], "system": system,
                              "tools": [t.name for t in tools]})
        if self.cursor < len(self.script):
            events = self.script[self.cursor]
            self.cursor += 1
        else:
            events = [{"type": "text_delta", "text": "(mock: no scripted turn left)"},
                      {"type": "done", "usage": Usage(10, 5), "stop_reason": "stop"}]
        yield from events
