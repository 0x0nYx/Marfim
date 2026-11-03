"""Skill tool — progressive disclosure (Claude Code / ECC / Oh My Pi doctrine).

Descriptions live in the system prompt; full bodies load on demand.
"""

from __future__ import annotations

from marfim.memory import Skill, discover_skills
from marfim.tools.base import Tool, ToolCtx, ToolResult


def _do_skill(args: dict, ctx: ToolCtx) -> ToolResult:
    name = str(args.get("name") or "")
    agent = ctx.agent
    skills: list[Skill] = agent.skills if agent is not None else discover_skills(
        agent.config if agent else None)  # type: ignore[arg-type]
    if not name:
        listing = "\n".join(f"- {s.name}: {s.description[:120]}" for s in skills) or "(no skills)"
        return ToolResult(f"skills installed:\n{listing}")
    for skill in skills:
        if skill.name == name:
            body = skill.body_without_frontmatter()
            return ToolResult(f"# skill: {skill.name}\n\n{body}")
    return ToolResult(f"error: skill '{name}' not found; available: "
                      f"{', '.join(s.name for s in skills)}", is_error=True)


def build_tool(skills: list[Skill]) -> Tool:
    names = ", ".join(s.name for s in skills) or "(none installed)"
    return Tool(
        name="skill",
        description=(f"Load a skill's full instructions on demand. Installed: {names}. "
                     "Load a skill BEFORE doing work it covers."),
        parameters={"type": "object", "properties": {"name": {"type": "string"}},
                    "required": ["name"]},
        readonly=True, handler=_do_skill)
