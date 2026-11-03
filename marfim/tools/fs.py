"""Filesystem tools: read, write, edit, glob, grep.

Edit doctrine merged:
- Claude Code: exact-match replace with replace_all flag; file must be read first.
- Aider: fuzzy match ladder (rstrip -> outdent -> ellipsis -> nearest).
- ECC GateGuard: refusing to edit a file never read this session.
- Oh My Pi: stale-content rejection is structural, not hoped away.
"""

from __future__ import annotations

import fnmatch
import re
import subprocess
from pathlib import Path

from marfim.tools.base import Tool, ToolCtx, ToolResult
from marfim.util import fuzzy_replace

IGNORED_DIRS = {".git", "node_modules", "__pycache__", ".venv", "venv", "dist",
                "build", ".next", ".turbo", "target", ".mypy_cache", ".pytest_cache",
                ".ruff_cache", "coverage", ".idea", ".vscode"}
BINARY_EXT = {".png", ".jpg", ".jpeg", ".gif", ".webp", ".ico", ".pdf", ".zip",
              ".gz", ".tar", ".br", ".7z", ".rar", ".woff", ".woff2", ".ttf", ".otf",
              ".exe", ".dll", ".so", ".dylib", ".class", ".jar", ".wasm", ".bin",
              ".mp3", ".mp4", ".mov", ".avi", ".sqlite", ".db", ".pyc", ".lock"}

MAX_FILE = 256_000


def _resolve(cwd: Path, raw: str) -> Path:
    path = Path(raw).expanduser()
    if not path.is_absolute():
        path = cwd / path
    return path.resolve()


def _read_file(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8", errors="replace")
    except UnicodeDecodeError:
        return path.read_bytes().decode("latin-1")


# ------------------------------------------------------------------------ read

def _do_read(args: dict, ctx: ToolCtx) -> ToolResult:
    raw = str(args.get("file_path") or args.get("path") or "")
    if not raw:
        return ToolResult("error: file_path is required", is_error=True)
    path = _resolve(ctx.cwd, raw)
    if not path.exists():
        return ToolResult(f"error: file not found: {path}", is_error=True)
    if path.suffix.lower() in BINARY_EXT:
        size = path.stat().st_size
        ctx.files_read.add(str(path))
        return ToolResult(f"(binary file, {size} bytes — use bash to inspect)")
    text = _read_file(path)
    ctx.files_read.add(str(path))
    offset = int(args.get("offset") or 0)
    limit = int(args.get("limit") or 2000)
    lines = text.split("\n")
    total = len(lines)
    window = lines[offset: offset + limit]
    numbered = "\n".join(f"{offset + i + 1:6d}\t{line}" for i, line in enumerate(window))
    if total > offset + limit:
        numbered += f"\n… [{total - offset - limit} more lines (offset={offset + limit})]"
    return ToolResult(numbered or "(empty file)")


# ----------------------------------------------------------------------- write

def _do_write(args: dict, ctx: ToolCtx) -> ToolResult:
    raw = str(args.get("file_path") or "")
    content = str(args.get("content") or "")
    if not raw:
        return ToolResult("error: file_path is required", is_error=True)
    path = _resolve(ctx.cwd, raw)
    existed = path.exists()
    old = _read_file(path) if existed else ""
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")
    ctx.files_read.add(str(path))
    verb = "updated" if existed else "created"
    return ToolResult(f"{verb} {path} ({len(content)} bytes)"
                      + (f", replaced {len(old)} bytes" if existed else ""))


# ------------------------------------------------------------------------ edit

def _do_edit(args: dict, ctx: ToolCtx) -> ToolResult:
    raw = str(args.get("file_path") or "")
    old = str(args.get("old_string") or args.get("old_text") or "")
    new = str(args.get("new_string") or args.get("new_text") or "")
    replace_all = bool(args.get("replace_all"))
    if not raw:
        return ToolResult("error: file_path is required", is_error=True)
    if not old:
        return ToolResult("error: old_string is required (use write for new files)", is_error=True)
    path = _resolve(ctx.cwd, raw)
    if not path.exists():
        return ToolResult(f"error: file not found: {path}", is_error=True)
    if str(path) not in ctx.files_read:
        return ToolResult(
            "error: GateGuard — read the file with the read tool before editing "
            "(evidence-first editing, ECC doctrine)", is_error=True)
    text = _read_file(path)
    if old not in text:
        outcome = fuzzy_replace(text, old, new)
        if outcome is None:
            return ToolResult(
                "error: old_string not found (no strategy of the ladder "
                "exact/rstrip/outdent/ellipsis/nearest matched)", is_error=True)
        new_text, strategy = outcome
    else:
        if replace_all:
            new_text = text.replace(old, new)
            path.write_text(new_text, encoding="utf-8")
            return ToolResult(f"edited {path}: {text.count(old)} replacement(s) [exact, replace_all]")
        new_text = text.replace(old, new, 1)
        strategy = "exact"
    path.write_text(new_text, encoding="utf-8")
    return ToolResult(f"edited {path} [strategy: {strategy}]")


# ------------------------------------------------------------------------ glob

def _do_glob(args: dict, ctx: ToolCtx) -> ToolResult:
    pattern = str(args.get("pattern") or "**/*")
    base = _resolve(ctx.cwd, str(args.get("path") or "."))
    if not base.exists():
        return ToolResult(f"error: path not found: {base}", is_error=True)
    it = base.rglob(pattern) if not base.is_file() else [base]
    out: list[str] = []
    for path in it:
        if any(part in IGNORED_DIRS for part in path.parts):
            continue
        if path.is_file():
            out.append(str(path))
        if len(out) >= 500:
            out.append("… [truncated at 500 files]")
            break
    return ToolResult("\n".join(out) or "(no matches)")


# ------------------------------------------------------------------------ grep

def _do_grep(args: dict, ctx: ToolCtx) -> ToolResult:
    pattern = str(args.get("pattern") or "")
    if not pattern:
        return ToolResult("error: pattern is required", is_error=True)
    base = _resolve(ctx.cwd, str(args.get("path") or "."))
    glob_pat = str(args.get("glob") or "")
    ctx_path = args.get("path")
    include = ["--glob", glob_pat] if glob_pat else []
    if base.is_file():
        cmd = ["rg", "--no-heading", "-n", "-S", pattern, str(base)]
    elif shutil_which("rg"):
        cmd = ["rg", "--no-heading", "-n", "-S", *include,
               "--max-count", "50", pattern, str(base)]
    else:
        return _python_grep(pattern, base, glob_pat)
    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, timeout=30)
    except subprocess.TimeoutExpired:
        return ToolResult("error: grep timed out", is_error=True)
    out = proc.stdout
    if not out.strip():
        return ToolResult("(no matches)")
    lines = out.splitlines()
    if len(lines) > 300:
        out = "\n".join(lines[:300]) + f"\n… [{len(lines) - 300} more matches]"
    return ToolResult(out)


def _python_grep(pattern: str, base: Path, glob_pat: str) -> ToolResult:
    rx = re.compile(pattern)
    out: list[str] = []
    root = base if base.is_dir() else base.parent
    files = [base] if base.is_file() else [p for p in root.rglob("*") if p.is_file()]
    for path in files:
        if any(part in IGNORED_DIRS for part in path.parts):
            continue
        if glob_pat and not fnmatch.fnmatch(path.name, glob_pat):
            continue
        if path.suffix.lower() in BINARY_EXT:
            continue
        try:
            for i, line in enumerate(_read_file(path).splitlines(), 1):
                if rx.search(line):
                    out.append(f"{path}:{i}:{line.strip()[:200]}")
                    if len(out) >= 300:
                        out.append("… [truncated at 300 matches]")
                        return ToolResult("\n".join(out))
        except OSError:
            continue
    return ToolResult("\n".join(out) or "(no matches)")


def shutil_which(name: str) -> str | None:
    import shutil
    return shutil.which(name)


def build_tools() -> list[Tool]:
    return [
        Tool(name="read",
             description=("Read a text file with line numbers. Params: file_path (required), "
                          "offset, limit. Must be called before editing a file."),
             parameters={"type": "object", "properties": {
                 "file_path": {"type": "string"}, "offset": {"type": "integer"},
                 "limit": {"type": "integer"}}, "required": ["file_path"]},
             readonly=True, handler=_do_read),
        Tool(name="write",
             description=("Create or overwrite a file. Params: file_path (required), "
                          "content (required). Parent dirs are created."),
             parameters={"type": "object", "properties": {
                 "file_path": {"type": "string"}, "content": {"type": "string"}},
                 "required": ["file_path", "content"]},
             handler=_do_write),
        Tool(name="edit",
             description=("Exact string replacement in a file (must read first). Falls back "
                          "through a match ladder: exact > rstrip > outdent > ellipsis > "
                          "nearest(>=0.85). Params: file_path, old_string, new_string, "
                          "replace_all."),
             parameters={"type": "object", "properties": {
                 "file_path": {"type": "string"}, "old_string": {"type": "string"},
                 "new_string": {"type": "string"}, "replace_all": {"type": "boolean"}},
                 "required": ["file_path", "old_string", "new_string"]},
             handler=_do_edit),
        Tool(name="glob",
             description=("Find files by wildcard pattern. Params: pattern (e.g. "
                          "'src/**/*.ts'), path (base dir, default cwd)."),
             parameters={"type": "object", "properties": {
                 "pattern": {"type": "string"}, "path": {"type": "string"}},
                 "required": ["pattern"]},
             readonly=True, handler=_do_glob),
        Tool(name="grep",
             description=("Regex content search (ripgrep-backed). Params: pattern, path, "
                          "glob filter."),
             parameters={"type": "object", "properties": {
                 "pattern": {"type": "string"}, "path": {"type": "string"},
                 "glob": {"type": "string"}}, "required": ["pattern"]},
             readonly=True, handler=_do_grep),
    ]
