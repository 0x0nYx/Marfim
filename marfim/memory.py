"""Memory, skills and custom command discovery.

Merged doctrine:
- Codex: AGENTS.md discovery walks cwd -> git root, concatenated.
- Claude Code: CLAUDE.md hierarchy; SKILL.md progressive disclosure.
- Oh My Pi: cross-harness compatibility (.claude/, .codex/ dirs are honoured).
- ECC: skills-first context economics — only descriptions go in the system
  prompt; bodies load on demand via the skill tool.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from pathlib import Path

from marfim.config import Config, marfim_home
from marfim.util import truncate_tail

MEMORY_FILENAMES = ("MARFIM.md", "AGENTS.md", "CLAUDE.md")


@dataclass
class MemorySection:
    path: str
    content: str


def collect_memory(config: Config) -> list[MemorySection]:
    """Hierarchy: home MARFIM.md, then root -> cwd chain (dedup by path)."""
    sections: list[MemorySection] = []
    seen: set[str] = set()
    home_memory = marfim_home() / "MARFIM.md"
    candidates: list[Path] = [home_memory]
    cur = Path.cwd().resolve()
    chain = [cur, *cur.parents]
    root = config.project_root.resolve()
    ordered: list[Path] = []
    for p in reversed(chain):                      # topmost first (Codex doctrine)
        if p in ordered:
            continue
        ordered.append(p)
        if p == root:
            break
    for base in ordered:
        for name in MEMORY_FILENAMES:
            path = base / name
            if path.is_file() and str(path) not in seen:
                seen.add(str(path))
                try:
                    content = path.read_text(encoding="utf-8", errors="replace")
                except OSError:
                    continue
                if content.strip():
                    sections.append(MemorySection(str(path), content))
    return sections


@dataclass
class Skill:
    name: str
    description: str
    path: Path

    def body(self) -> str:
        try:
            return self.path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            return ""

    def body_without_frontmatter(self) -> str:
        text = self.body()
        return _strip_frontmatter(text)


def _strip_frontmatter(text: str) -> str:
    if text.startswith("---"):
        end = text.find("\n---", 3)
        if end != -1:
            return text[end + 4:].lstrip("\n")
    return text


def _parse_frontmatter(text: str) -> dict:
    if not text.startswith("---"):
        return {}
    end = text.find("\n---", 3)
    if end == -1:
        return {}
    meta: dict = {}
    for line in text[3:end].splitlines():
        if ":" in line:
            key, _, val = line.partition(":")
            meta[key.strip()] = val.strip().strip('"').strip("'")
    return meta


def discover_skills(config: Config) -> list[Skill]:
    skills: dict[str, Skill] = {}
    for d in config.skills_dirs:
        base = Path(d).expanduser()
        if not base.is_dir():
            continue
        for skill_md in sorted(base.glob("*/SKILL.md")):
            try:
                text = skill_md.read_text(encoding="utf-8", errors="replace")
            except OSError:
                continue
            meta = _parse_frontmatter(text)
            name = meta.get("name") or skill_md.parent.name
            description = meta.get("description", "")
            if name and name not in skills:
                skills[name] = Skill(name=name, description=description, path=skill_md)
    return list(skills.values())


def skills_prompt(skills: list[Skill]) -> str:
    if not skills:
        return ""
    lines = ["Available skills (load full instructions with the skill tool when relevant):"]
    for skill in skills:
        desc = truncate_tail(skill.description or "(no description)", 160)
        lines.append(f"- {skill.name}: {desc}")
    return "\n".join(lines)


@dataclass
class CustomCommand:
    name: str
    description: str
    path: Path
    body: str

    def render(self, arguments: str) -> str:
        text = self.body
        text = text.replace("$ARGUMENTS", arguments)
        text = re.sub(r"\$([1-9])", lambda m: (arguments.split()[int(m.group(1)) - 1]
                                               if len(arguments.split()) >= int(m.group(1)) else ""),
                      text)
        return text


def discover_commands(config: Config) -> list[CustomCommand]:
    commands: dict[str, CustomCommand] = {}
    for d in config.commands_dirs:
        base = Path(d).expanduser()
        if not base.is_dir():
            continue
        for md in sorted(base.glob("*.md")):
            try:
                text = md.read_text(encoding="utf-8", errors="replace")
            except OSError:
                continue
            meta = _parse_frontmatter(text)
            name = md.stem
            if name not in commands:
                commands[name] = CustomCommand(
                    name=name, description=meta.get("description", ""), path=md,
                    body=_strip_frontmatter(text))
    return list(commands.values())
