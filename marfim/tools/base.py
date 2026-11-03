"""Tool base classes + execution context.

Tool result doctrine (REA synthesis): every result is summary-first, bounded,
and explicit about truncation — absence of output is never silently dropped.

Review finding 15: tool arguments are validated against the tool's JSON schema
before dispatch, and finding 16: every read records (mtime_ns, sha256) so a
stale edit is structurally rejected (Oh My Pi doctrine).
"""

from __future__ import annotations

import hashlib
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
    files_read: dict = field(default_factory=dict)   # path -> (mtime_ns, sha256)
    say: Callable[[str], None] = lambda msg: None    # UI callback
    depth: int = 0

    def note(self, msg: str) -> None:
        try:
            self.say(msg)
        except Exception:
            pass

    # ------------------------------------------------------- read evidence
    # (GateGuard + stale-read protection, review findings 15/16)

    def mark_read(self, path: Path) -> None:
        try:
            data = Path(path).read_bytes()
        except OSError:
            self.files_read[str(path)] = (0, "")
            return
        self.files_read[str(path)] = (
            Path(path).stat().st_mtime_ns, hashlib.sha256(data).hexdigest())

    def mark_fresh(self, path: Path) -> None:
        """After WE wrote the file, record the new state as fresh evidence."""
        self.mark_read(path)

    def check_fresh(self, path: Path) -> str | None:
        """Return an error string when the file changed on disk since it was
        last read (stale-read rejection is structural, not hoped away)."""
        state = self.files_read.get(str(path))
        if state is None:
            return None
        mtime_ns, sha = state
        try:
            data = Path(path).read_bytes()
        except OSError:
            return f"error: {path} vanished since it was read; re-read it"
        current = (Path(path).stat().st_mtime_ns, hashlib.sha256(data).hexdigest())
        if sha and current[1] != sha:
            return ("error: stale read — {p} changed on disk since it was last read "
                    "(external modification); re-read it before editing".replace("{p}", str(path)))
        if current[0] != mtime_ns and sha:
            # same bytes, new mtime: still stale by contract
            return (f"error: stale read — {path} was touched on disk since it was "
                    f"last read; re-read it before editing")
        return None


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


_TYPE_MAP = {
    "string": str, "integer": int, "number": (int, float),
    "boolean": bool, "array": list, "object": dict,
}


def validate_args(tool: Tool, args: dict) -> str | None:
    """Minimal JSON-schema validation: required keys + declared types + enums
    (review finding 15: there was NO validation, so `write` without `content`
    zeroed a file).  Returns an error string or None."""
    schema = tool.parameters or {}
    if schema.get("type") != "object":
        return None
    props = schema.get("properties") or {}
    for req in schema.get("required") or []:
        if req not in args or args[req] is None:
            return f"missing required argument '{req}' for tool '{tool.name}'"
    for key, val in (args or {}).items():
        if key not in props:
            continue
        want = props[key].get("type")
        if not want:
            continue
        py = _TYPE_MAP.get(want)
        if py is None:
            continue
        if want == "integer" and isinstance(val, bool):
            return f"argument '{key}' must be an integer"
        if not isinstance(val, py):
            return f"argument '{key}' must be {want}, got {type(val).__name__}"
        enum = props[key].get("enum")
        if enum and val not in enum:
            return f"argument '{key}' must be one of {enum}"
    return None
