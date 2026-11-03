"""Tool base classes + execution context.

Tool result doctrine (REA synthesis): every result is summary-first, bounded,
and explicit about truncation — absence of output is never silently dropped.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING, Callable

from marfim.util import truncate_middle

if TYPE_CHECKING:
    from marfim.agent.loop import Agent
    from marfim.session.store import Session

MAX_OUTPUT = 30_000          # chars kept per tool result (head-biased)


@dataclass
class ToolResult:
    text: str
    meta: dict = field(default_factory=dict)
    is_error: bool = False

    def bounded(self, limit: int = MAX_OUTPUT) -> "ToolResult":
        if len(self.text) > limit:
            self.text = truncate_middle(self.text, limit)
            self.meta["truncated"] = True
        return self


@dataclass
class ToolCtx:
    cwd: Path
    session: "Session | None" = None
    agent: "Agent | None" = None
    files_read: set = field(default_factory=set)     # GateGuard evidence
    say: Callable[[str], None] = lambda msg: None    # UI callback
    depth: int = 0

    def note(self, msg: str) -> None:
        try:
            self.say(msg)
        except Exception:
            pass


@dataclass
class Tool:
    name: str
    description: str
    parameters: dict
    readonly: bool = False
    handler: Callable[[dict, ToolCtx], ToolResult] = None  # type: ignore[assignment]

    def run(self, args: dict, ctx: ToolCtx) -> ToolResult:
        try:
            result = self.handler(args or {}, ctx)
        except PermissionError as exc:
            result = ToolResult(f"error: {exc}", is_error=True)
        except FileNotFoundError as exc:
            result = ToolResult(f"error: not found: {exc}", is_error=True)
        except IsADirectoryError as exc:
            result = ToolResult(f"error: is a directory: {exc}", is_error=True)
        except Exception as exc:  # noqa: BLE001 — tools must never crash the loop
            result = ToolResult(f"error: {type(exc).__name__}: {exc}", is_error=True)
        return result.bounded()

    def spec(self):
        from marfim.providers.base import ToolSpec
        return ToolSpec(name=self.name, description=self.description,
                        parameters=self.parameters)
