"""Todo tool — in-session task tracking (Claude Code TodoWrite + goose plans).

State is persisted into the session file so it survives resume.
"""

from __future__ import annotations

from marfim.tools.base import Tool, ToolCtx, ToolResult


def _valid(items) -> bool:
    return isinstance(items, list) and all(
        isinstance(i, dict) and {"content", "status"} <= set(i) for i in items)


def _render(items: list[dict]) -> str:
    if not items:
        return "(todo list is empty)"
    icons = {"pending": "☐", "in_progress": "◐", "completed": "☑"}
    return "\n".join(f"{icons.get(i.get('status'), '☐')} [{i.get('status')}] {i.get('content')}"
                     for i in items)


def _do_todo(args: dict, ctx: ToolCtx) -> ToolResult:
    if "items" in args:
        items = args["items"]
        if not _valid(items):
            return ToolResult("error: items must be [{content, status(pending|in_progress|"
                              "completed)}]", is_error=True)
        if ctx.session is not None:
            # memory and file together (review finding 26)
            ctx.session.set_todos(items)
        return ToolResult("todo list updated:\n" + _render(items))
    if ctx.session is not None:
        items = ctx.session.last_todo()
        return ToolResult(_render(items))
    return ToolResult(_render([]))


def build_tools() -> list[Tool]:
    return [
        Tool(name="todo",
             description=("Maintain the session task list. Params: items (optional) = "
                          "[{content, status: pending|in_progress|completed}]; omit items "
                          "to read the current list. Keep exactly one item in_progress."),
             parameters={"type": "object", "properties": {
                 "items": {"type": "array", "items": {
                     "type": "object", "properties": {
                         "content": {"type": "string"},
                         "status": {"type": "string", "enum": ["pending", "in_progress",
                                                               "completed"]},
                     }, "required": ["content", "status"]}}},
             },
             readonly=True, handler=_do_todo),
    ]
