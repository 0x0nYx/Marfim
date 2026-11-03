"""Filesystem tools: read, write, edit, glob, grep.

Edit doctrine merged:
- Claude Code: exact-match replace with replace_all flag; file must be read first.
- Aider: fuzzy match ladder (exact -> rstrip -> outdent -> ellipsis) — the
  destructive `nearest` strategy was REMOVED (review finding 13); a near-miss
  is surfaced as a hint, never applied.
- ECC GateGuard: refusing to edit a file never read this session.
- Oh My Pi: stale-content rejection is structural, not hoped away (finding 16).

Integrity doctrine (review findings 15/17): writes are atomic (temp +
os.replace, mode preserved); overwriting an existing file requires read
evidence; files round-trip in their own encoding and newline style or the edit
is refused.
"""

from __future__ import annotations

import fnmatch
import os
import re
import subprocess
from pathlib import Path

from marfim.tools.base import Tool, ToolCtx, ToolResult
from marfim.util import (FileIntegrityError, atomic_write_text, fuzzy_replace,
                         nearest_hint, read_text_preserve)

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


def _load(path: Path):
    """Read with encoding/newline preservation (finding 17)."""
    return read_text_preserve(path)


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
        ctx.mark_read(path)
        return ToolResult(f"(binary file, {size} bytes — use bash to inspect)")
    try:
        tf = _load(path)
    except FileIntegrityError as exc:
        return ToolResult(f"error: {exc}", is_error=True)
    ctx.mark_read(path)
    offset = int(args.get("offset") or 0)
    limit = int(args.get("limit") or 2000)
    lines = tf.text.split("\n")
    total = len(lines)
    window = lines[offset: offset + limit]
    numbered = "\n".join(f"{offset + i + 1:6d}\t{line}" for i, line in enumerate(window))
    if total > offset + limit:
        numbered += f"\n… [{total - offset - limit} more lines (offset={offset + limit})]"
    enc = "" if tf.encoding == "utf-8" and tf.newline == "\n" else \
        f" [{tf.encoding}{'+CRLF' if tf.newline != chr(10) else ''}]"
    return ToolResult((numbered or "(empty file)") + enc)


# ----------------------------------------------------------------------- write

def _do_write(args: dict, ctx: ToolCtx) -> ToolResult:
    raw = str(args.get("file_path") or "")
    if not raw:
        return ToolResult("error: file_path is required", is_error=True)
    if "content" not in args or args.get("content") is None:
        return ToolResult("error: content is required (refusing to write an empty "
                          "file by accident — pass content explicitly)", is_error=True)
    content = str(args["content"])
    path = _resolve(ctx.cwd, raw)
    existed = path.exists()
    old_len = 0
    if existed:
        stale = ctx.check_fresh(path)
        if stale:
            return ToolResult(stale, is_error=True)
        if str(path) not in ctx.files_read:
            return ToolResult(
                "error: GateGuard — read the existing file with the read tool before "
                "overwriting it (evidence-first, ECC doctrine)", is_error=True)
        old_len = path.stat().st_size
    encoding, newline = "utf-8", "\n"
    if existed:
        try:
            tf = _load(path)
            encoding, newline = tf.encoding, tf.newline
        except FileIntegrityError as exc:
            return ToolResult(f"error: {exc}", is_error=True)
    atomic_write_text(path, content, encoding=encoding, newline=newline)
    ctx.mark_fresh(path)
    if not existed and ctx.session is not None and ctx.agent is not None:
        # track creations so /rewind can delete them (review finding 31)
        ctx.session.record_creation(path, ctx.agent.config.project_root)
    verb = "updated" if existed else "created"
    return ToolResult(f"{verb} {path} ({len(content.encode(encoding))} bytes)"
                      + (f", replaced {old_len} bytes" if existed else ""))


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
    stale = ctx.check_fresh(path)
    if stale:
        return ToolResult(stale, is_error=True)
    try:
        tf = _load(path)
    except FileIntegrityError as exc:
        return ToolResult(f"error: {exc}", is_error=True)
    text = tf.text
    if old not in text:
        if replace_all:
            return ToolResult("error: old_string not found (replace_all requested but "
                              "the exact string is absent; fuzzy application is disabled "
                              "with replace_all)", is_error=True)
        outcome = fuzzy_replace(text, old, new)
        if outcome is None:
            hint = nearest_hint(text, old)
            msg = ("error: old_string not found (no strategy of the ladder "
                   "exact/rstrip/outdent/ellipsis matched)")
            if hint:
                window, ratio = hint
                msg += (f"\nclosest match ({ratio:.2f}, NOT applied — review it and "
                        f"retry with the exact text):\n{window[:600]}")
            return ToolResult(msg, is_error=True)
        new_text, strategy = outcome
    else:
        count = text.count(old)
        if count > 1 and not replace_all:
            return ToolResult(
                f"error: old_string matches {count} locations; provide more surrounding "
                f"context or set replace_all=true", is_error=True)
        if replace_all:
            new_text = text.replace(old, new)
            strategy = f"exact, replace_all ({count})"
        else:
            new_text = text.replace(old, new, 1)
            strategy = "exact"
    if new_text == text:
        return ToolResult("error: edit produced no change", is_error=True)
    atomic_write_text(path, new_text, encoding=tf.encoding, newline=tf.newline)
    ctx.mark_fresh(path)
    return ToolResult(f"edited {path} [strategy: {strategy}]")


# ------------------------------------------------------------------------ glob

def _glob_regex(pattern: str) -> re.Pattern:
    """Translate a glob with `**` support into a regex (fnmatch can't)."""
    i, out = 0, []
    while i < len(pattern):
        ch = pattern[i]
        if ch == "*":
            if pattern[i:i + 2] == "**":
                # `**/` matches any depth including none; bare `**` matches all
                if pattern[i:i + 3] == "**/":
                    out.append("(?:.*/)?")
                    i += 3
                else:
                    out.append(".*")
                    i += 2
            else:
                out.append("[^/]*")
                i += 1
        elif ch == "?":
            out.append("[^/]")
            i += 1
        else:
            out.append(re.escape(ch))
            i += 1
    return re.compile("^" + "".join(out) + "$")


def _do_glob(args: dict, ctx: ToolCtx) -> ToolResult:
    pattern = str(args.get("pattern") or "**/*")
    base = _resolve(ctx.cwd, str(args.get("path") or "."))
    if not base.exists():
        return ToolResult(f"error: path not found: {base}", is_error=True)
    if base.is_file():
        return ToolResult(str(base))
    rx = _glob_regex(pattern)
    out: list[str] = []
    truncated = False
    # os.walk with in-place pruning: an ignored dir name anywhere in the base
    # path no longer poisons relative matching, and node_modules is never
    # entered at all (review finding 18)
    for dirpath, dirnames, filenames in os.walk(base):
        rel_dir = Path(dirpath).relative_to(base)
        dirnames[:] = sorted(d for d in dirnames
                             if d not in IGNORED_DIRS and not d.endswith(".marfim-tmp"))
        for name in sorted(filenames):
            rel = (rel_dir / name) if str(rel_dir) != "." else Path(name)
            if not rx.match(rel.as_posix()):
                continue
            out.append(str(base / rel))
            if len(out) >= 500:
                truncated = True
                break
        if truncated:
            break
    if truncated:
        out.append("… [truncated at 500 files]")
    return ToolResult("\n".join(out) or "(no matches)")


# ------------------------------------------------------------------------ grep

def _do_grep(args: dict, ctx: ToolCtx) -> ToolResult:
    pattern = str(args.get("pattern") or "")
    if not pattern:
        return ToolResult("error: pattern is required", is_error=True)
    base = _resolve(ctx.cwd, str(args.get("path") or "."))
    glob_pat = str(args.get("glob") or "")
    include = ["--glob", glob_pat] if glob_pat else []
    if base.is_file():
        cmd = ["rg", "--no-heading", "-n", "-S", "-e", pattern, "--", str(base)]
    elif shutil_which("rg"):
        # `-e pattern -- path` — the model-controlled pattern can never be
        # mistaken for a flag (review finding 7: `--pre=sh` code execution)
        cmd = ["rg", "--no-heading", "-n", "-S", *include,
               "--max-count", "50", "-e", pattern, "--", str(base)]
    else:
        return _python_grep(pattern, base, glob_pat)
    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, timeout=30)
    except subprocess.TimeoutExpired:
        return ToolResult("error: grep timed out", is_error=True)
    if proc.returncode == 2:
        # rg uses 2 for errors: surface them instead of reporting "(no matches)"
        return ToolResult(f"error: rg failed: {(proc.stderr or '').strip()[:400]}",
                          is_error=True)
    out = proc.stdout
    if not out.strip():
        return ToolResult("(no matches)")
    lines = out.splitlines()
    if len(lines) > 300:
        out = "\n".join(lines[:300]) + f"\n… [{len(lines) - 300} more matches]"
    if proc.stderr.strip():
        out += "\n[rg stderr] " + proc.stderr.strip()[:400]
    return ToolResult(out)


def _python_grep(pattern: str, base: Path, glob_pat: str) -> ToolResult:
    try:
        rx = re.compile(pattern)
    except re.error as exc:
        return ToolResult(f"error: invalid regex: {exc}", is_error=True)
    out: list[str] = []
    root = base if base.is_dir() else base.parent
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [d for d in dirnames if d not in IGNORED_DIRS]
        for name in filenames:
            path = Path(dirpath) / name
            if base.is_file() and path != base:
                continue
            if glob_pat and not fnmatch.fnmatch(path.name, glob_pat):
                continue
            if path.suffix.lower() in BINARY_EXT:
                continue
            try:
                for i, line in enumerate(read_text_preserve(path).text.splitlines(), 1):
                    if rx.search(line):
                        out.append(f"{path}:{i}:{line.strip()[:200]}")
                        if len(out) >= 300:
                            out.append("… [truncated at 300 matches]")
                            return ToolResult("\n".join(out))
            except (OSError, FileIntegrityError):
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
             description=("Create or overwrite a file (existing files must be read first; "
                          "writes are atomic). Params: file_path (required), "
                          "content (required). Parent dirs are created."),
             parameters={"type": "object", "properties": {
                 "file_path": {"type": "string"}, "content": {"type": "string"}},
                 "required": ["file_path", "content"]},
             handler=_do_write),
        Tool(name="edit",
             description=("Exact string replacement in a file (must read first). Falls back "
                          "through a match ladder: exact > rstrip > outdent > ellipsis; a "
                          "near-miss is reported as a hint, never applied. Params: "
                          "file_path, old_string, new_string, replace_all."),
             parameters={"type": "object", "properties": {
                 "file_path": {"type": "string"}, "old_string": {"type": "string"},
                 "new_string": {"type": "string"}, "replace_all": {"type": "boolean"}},
                 "required": ["file_path", "old_string", "new_string"]},
             handler=_do_edit),
        Tool(name="multi_edit",
             description=("Apply several exact edits to one file in order (must read "
                          "first). Params: file_path, edits=[{old_string, new_string}]."),
             parameters={"type": "object", "properties": {
                 "file_path": {"type": "string"},
                 "edits": {"type": "array", "items": {"type": "object", "properties": {
                     "old_string": {"type": "string"}, "new_string": {"type": "string"}},
                     "required": ["old_string", "new_string"]}}},
                 "required": ["file_path", "edits"]},
             handler=_do_multi_edit),
        Tool(name="glob",
             description=("Find files by wildcard pattern (supports **). Params: pattern "
                          "(e.g. 'src/**/*.ts'), path (base dir, default cwd)."),
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


def _do_multi_edit(args: dict, ctx: ToolCtx) -> ToolResult:
    """Sequential exact edits on one file (review finding 3 adds it to the
    protected set; here: GateGuard + stale checks per step)."""
    raw = str(args.get("file_path") or "")
    edits = args.get("edits") or []
    if not raw:
        return ToolResult("error: file_path is required", is_error=True)
    if not isinstance(edits, list) or not edits:
        return ToolResult("error: edits must be a non-empty array", is_error=True)
    path = _resolve(ctx.cwd, raw)
    if not path.exists():
        return ToolResult(f"error: file not found: {path}", is_error=True)
    if str(path) not in ctx.files_read:
        return ToolResult("error: GateGuard — read the file before multi_edit", is_error=True)
    stale = ctx.check_fresh(path)
    if stale:
        return ToolResult(stale, is_error=True)
    try:
        tf = _load(path)
    except FileIntegrityError as exc:
        return ToolResult(f"error: {exc}", is_error=True)
    text = tf.text
    applied = 0
    for i, step in enumerate(edits, 1):
        old = str(step.get("old_string") or "")
        new = str(step.get("new_string") or "")
        if not old:
            return ToolResult(f"error: edits[{i}].old_string is required", is_error=True)
        count = text.count(old)
        if count != 1:
            return ToolResult(f"error: edits[{i}].old_string matches {count} locations "
                              f"(need exactly 1)", is_error=True)
        text = text.replace(old, new, 1)
        applied += 1
    if text == tf.text:
        return ToolResult("error: multi_edit produced no change", is_error=True)
    atomic_write_text(path, text, encoding=tf.encoding, newline=tf.newline)
    ctx.mark_fresh(path)
    return ToolResult(f"multi_edit {path}: {applied} edit(s) applied")
