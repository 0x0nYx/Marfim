"""MCP tool bridge — wraps MCP servers' tools as native Marfim tools.

Review finding 32: tool names are sanitized to provider limits; §4.2: the
`readOnlyHint` annotation drives the permission tier, so read-only MCP tools
do not prompt in default mode and are allowed in plan mode.
"""

from __future__ import annotations

from marfim.mcp.client import BaseMcpClient, McpError, sanitize_tool_name
from marfim.tools.base import Tool, ToolCtx, ToolResult


def build_mcp_tools(client: BaseMcpClient) -> list[Tool]:
    tools: list[Tool] = []
    try:
        entries = client.list_tools()
    except McpError:
        return tools
    for entry in entries:
        name = entry.get("name", "")
        if not name:
            continue
        annotations = entry.get("annotations") or {}
        readonly = bool(annotations.get("readOnlyHint"))
        tools.append(Tool(
            name=sanitize_tool_name(client.name, name),
            description=f"[MCP:{client.name}] {entry.get('description', '')[:400]}",
            parameters=entry.get("inputSchema") or {"type": "object", "properties": {}},
            readonly=readonly,
            handler=_make_handler(client, name),
        ))
    return tools


def _make_handler(client: BaseMcpClient, tool_name: str):
    def handler(args: dict, ctx: ToolCtx) -> ToolResult:
        try:
            text = client.call_tool(tool_name, args)
        except McpError as exc:
            return ToolResult(f"error: {exc}", is_error=True)
        return ToolResult(text)
    return handler
