"""Tool registry — assembles the full toolset for a turn.

Tool planning is a pure function of config + context (Codex spec_plan doctrine).
"""

from __future__ import annotations

from marfim.memory import Skill
from marfim.mcp.client import McpManager
from marfim.tools.base import Tool, ToolCtx
from marfim.tools import fs, bash as bash_tools, web as web_tools, todo as todo_tools, \
    patch as patch_tools, repomap as repomap_tools, skill as skill_tools, task as task_tools


def default_tools(skills: list[Skill] | None = None, mcp: McpManager | None = None) -> list[Tool]:
    tools: list[Tool] = []
    tools += fs.build_tools()
    tools += bash_tools.build_tools()
    tools += web_tools.build_tools()
    tools += todo_tools.build_tools()
    tools += patch_tools.build_tools()
    tools += repomap_tools.build_tools()
    tools += task_tools.build_tools()
    tools += [skill_tools.build_tool(skills or [])]
    if mcp is not None:
        for client in mcp.clients.values():
            if client._alive:
                from marfim.tools.mcp_bridge import build_mcp_tools
                tools += build_mcp_tools(client)
    return tools


def get_tool(tools: list[Tool], name: str) -> Tool | None:
    for tool in tools:
        if tool.name == name:
            return tool
    return None
