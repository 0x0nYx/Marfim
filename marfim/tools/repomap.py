"""Repo map tool — Aider doctrine, dependency-free edition.

A structural skeleton of the repository: files with their top-level symbols
(def/class/function/struct/impl/trait/...), budget-limited, biased toward
files whose paths match recent conversation keywords.  The full tree-sitter +
personalized-PageRank map is the documented upgrade path (docs/ROADMAP.md).
"""

from __future__ import annotations

import re
from pathlib import Path

from marfim.tools.base import Tool, ToolCtx, ToolResult
from marfim.tools.fs import BINARY_EXT, IGNORED_DIRS

SYMBOL_PATTERNS = {
    ".py": r"^(?:def|class)\s+(\w+)",
    ".ts": r"^(?:export\s+)?(?:async\s+)?(?:function|class|interface|type)\s+(\w+)",
    ".tsx": r"^(?:export\s+)?(?:function|class|interface|type)\s+(\w+)",
    ".js": r"^(?:export\s+)?(?:async\s+)?function\s+(\w+)|^(?:class|const)\s+(\w+)",
    ".rs": r"^(?:pub\s+)?(?:fn|struct|enum|trait|impl|mod)\s+(\w+)",
    ".go": r"^func\s+(?:\([^)]+\)\s+)?(\w+)|^type\s+(\w+)",
    ".java": r"^(?:public|private|protected)?\s*(?:final\s+)?(?:class|interface|void|"
             r"public|private)\s+\w*\s*(\w+)\s*[({]",
    ".c": r"^\w[\w\s\*]*\s+\**(\w+)\s*\(",
    ".h": r"^\w[\w\s\*]*\s+\**(\w+)\s*\(",
    ".cpp": r"^\w[\w\s\*:<>&]*\s+(\w+)\s*\(",
    ".md": r"^#{1,2}\s+(.+)",
}
BUDGET = 14_000


def _symbols(path: Path) -> list[str]:
    rx = SYMBOL_PATTERNS.get(path.suffix.lower())
    if not rx:
        return []
    out = []
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return []
    for line in text.splitlines()[:4000]:
        m = re.match(rx, line.strip()) if not line.startswith(("\t", " ")) else None
        if m:
            out.append(m.group(1))
        if len(out) >= 24:
            break
    return out


def build_map(root: Path, focus_terms: list[str] | None = None, budget: int = BUDGET) -> str:
    focus_terms = [t.lower() for t in (focus_terms or []) if len(t) > 2]
    entries: list[tuple[float, str, list[str]]] = []
    for path in root.rglob("*"):
        if not path.is_file():
            continue
        rel = path.relative_to(root)
        if any(part in IGNORED_DIRS for part in rel.parts) or rel.parts[0].startswith("."):
            continue
        if path.suffix.lower() in BINARY_EXT or path.suffix == "":
            continue
        score = 0.0
        rels = str(rel).lower()
        for term in focus_terms:
            if term in rels:
                score += 5.0
        syms = _symbols(path)
        for term in focus_terms:
            if any(term in s.lower() for s in syms):
                score += 2.0
        depth_penalty = len(rel.parts) * 0.1
        entries.append((score - depth_penalty, str(rel), syms))
        if len(entries) > 4000:
            break
    entries.sort(key=lambda e: (-e[0], e[1]))
    lines: list[str] = []
    used = 0
    for _, rel, syms in entries:
        line = f"{rel}" + (f"  ·  {', '.join(syms[:12])}" if syms else "")
        if used + len(line) > budget:
            lines.append(f"… [{budget} char budget reached; {len(entries) - len(lines)} files not shown]")
            break
        lines.append(line)
        used += len(line) + 1
    header = f"Repo map for {root.name} ({len(lines)} entries, relevance-ranked):"
    return header + "\n" + "\n".join(lines)


def _do_map(args: dict, ctx: ToolCtx) -> ToolResult:
    focus = args.get("focus") or []
    if isinstance(focus, str):
        focus = [focus]
    path = Path(args.get("path") or ctx.cwd)
    text = build_map(path, focus)
    return ToolResult(text)


def build_tools() -> list[Tool]:
    return [
        Tool(name="repomap",
             description=("Structural map of the repository: files with top-level symbols, "
                          "relevance-ranked against optional focus terms. Params: focus "
                          "(array of keywords), path."),
             parameters={"type": "object", "properties": {
                 "focus": {"type": "array", "items": {"type": "string"}},
                 "path": {"type": "string"}}},
             readonly=True, handler=_do_map),
    ]
