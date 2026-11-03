"""MCP tool bridge — wraps MCP servers' tools as native Marfim tools."""

from __future__ import annotations

from marfim.mcp.client import McpError, StdioMcpClient
from marfim.tools.base import Tool, ToolCtx, ToolResult


def build_mcp_tools(client: StdioMcpClient) -> list[Tool]:
    tools: list[Tool] = []
    try:
        entries = client.list_tools()
    except McpError:
        return tools
    for entry in entries:
        name = entry.get("name", "")
        if not name:
            continue
        tools.append(Tool(
            name=f"mcp__{client.name}__{name}",
            description=f"[MCP:{client.name}] {entry.get('description', '')[:400]}",
            parameters=entry.get("inputSchema") or {"type": "object", "properties": {}},
            readonly=False,
            handler=_make_handler(client, name),
        ))
    return tools


def _make_handler(client: StdioMcpClient, tool_name: str):
    def handler(args: dict, ctx: ToolCtx) -> ToolResult:
        try:
            text = client.call_tool(tool_name, args)
        except McpError as exc:
            return ToolResult(f"error: {exc}", is_error=True)
        return ToolResult(text)
    return handler
