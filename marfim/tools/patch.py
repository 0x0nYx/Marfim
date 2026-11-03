"""apply_patch tool — Codex-style heredoc patch DSL (clean-room parser).

Format:
*** Begin Patch
*** Add File: path
+line
*** Update File: path
@@ def old_context
-old line
+new line
*** Delete File: path
*** End Patch

Multi-hunk updates are supported; @@ headers fuzzy-locate their anchor line.

Integrity doctrine (review findings 3/15/16/17): every touched path is
evaluated by the permission engine BEFORE the patch runs; writes are atomic;
files round-trip in their own encoding; updating a file that changed on disk
since it was read is refused.
"""

from __future__ import annotations

import difflib
from pathlib import Path

from marfim.tools.base import Tool, ToolCtx, ToolResult
from marfim.util import (FileIntegrityError, atomic_write_text,
                         read_text_preserve)


class PatchError(Exception):
    pass


def parse_patch(text: str) -> list[dict]:
    if "*** Begin Patch" not in text:
        raise PatchError("missing '*** Begin Patch' header")
    ops: list[dict] = []
    current: dict | None = None
    section = None  # None | 'add' | 'update' | 'delete'
    for raw in text.splitlines():
        line = raw.rstrip("\n")
        if line.startswith("*** Begin Patch"):
            continue
        if line.startswith("*** End Patch"):
            break
        if line.startswith("*** Add File:"):
            section = "add"
            current = {"op": "add", "path": line.split(":", 1)[1].strip(), "lines": []}
            ops.append(current)
        elif line.startswith("*** Update File:"):
            section = "update"
            current = {"op": "update", "path": line.split(":", 1)[1].strip(), "hunks": []}
            ops.append(current)
        elif line.startswith("*** Delete File:"):
            section = "delete"
            current = {"op": "delete", "path": line.split(":", 1)[1].strip()}
            ops.append(current)
        elif current is None:
            continue
        elif section == "add":
            if line.startswith("+"):
                current["lines"].append(line[1:])
            elif line == "":
                current["lines"].append("")
        elif section == "update":
            if line.startswith("@@"):
                current["hunks"].append({"anchor": line[2:].strip(), "changes": []})
            elif current["hunks"]:
                if line.startswith("+") or line.startswith("-") or line == "":
                    current["hunks"][-1]["changes"].append(line)
        # delete: nothing else to collect
    if not ops:
        raise PatchError("no file operations found in patch")
    return ops


def patch_paths(patch: str) -> list[str]:
    """Every path a patch touches — used by the permission engine (finding 3)."""
    try:
        return [op["path"] for op in parse_patch(patch)]
    except PatchError:
        return []


def apply_patch_text(patch: str, cwd: Path, ctx: ToolCtx) -> str:
    ops = parse_patch(patch)
    report: list[str] = []
    for op in ops:
        path = (cwd / op["path"]).resolve() if not Path(op["path"]).is_absolute() \
            else Path(op["path"])
        if op["op"] == "add":
            if path.exists():
                raise PatchError(f"Add File: {op['path']} already exists")
            atomic_write_text(path, "\n".join(op["lines"]) + "\n")
            ctx.mark_fresh(path)
            report.append(f"added {op['path']} ({len(op['lines'])} lines)")
        elif op["op"] == "delete":
            if not path.exists():
                raise PatchError(f"Delete File: {op['path']} not found")
            if str(path) in ctx.files_read:
                stale = ctx.check_fresh(path)
                if stale:
                    raise PatchError(stale.replace("error: ", ""))
            path.unlink()
            ctx.files_read.pop(str(path), None)
            report.append(f"deleted {op['path']}")
        else:
            if not path.exists():
                raise PatchError(f"Update File: {op['path']} not found")
            if str(path) not in ctx.files_read:
                raise PatchError(
                    f"GateGuard: read {op['path']} with the read tool before patching")
            stale = ctx.check_fresh(path)
            if stale:
                raise PatchError(stale.replace("error: ", ""))
            try:
                tf = read_text_preserve(path)
            except FileIntegrityError as exc:
                raise PatchError(str(exc)) from exc
            new_text = _apply_hunks(tf.text, op["hunks"], op["path"])
            if new_text == tf.text:
                raise PatchError(f"Update File {op['path']}: patch produced no change")
            atomic_write_text(path, new_text, encoding=tf.encoding, newline=tf.newline)
            ctx.mark_fresh(path)
            added = sum(1 for h in op["hunks"] for c in h["changes"] if c.startswith("+"))
            removed = sum(1 for h in op["hunks"] for c in h["changes"] if c.startswith("-"))
            report.append(f"updated {op['path']} (+{added} -{removed})")
    return "\n".join(report)


def _apply_hunks(text: str, hunks: list[dict], name: str) -> str:
    lines = text.split("\n")
    for hunk in hunks:
        anchor = hunk.get("anchor", "")
        removals = [c[1:] for c in hunk["changes"] if c.startswith("-")]
        additions = [c[1:] for c in hunk["changes"] if c.startswith("+")]
        # locate anchor context
        start = 0
        if anchor:
            anchor_idx = _find_anchor(lines, anchor)
            if anchor_idx is None:
                raise PatchError(f"Update File {name}: anchor '@@ {anchor}' not found")
            start = anchor_idx
        # find the removal block starting at/after anchor
        pos = _find_block(lines, removals, start) if removals else start
        if removals and pos is None:
            raise PatchError(f"Update File {name}: '-' block not found near '@@ {anchor}'")
        if removals:
            lines[pos:pos + len(removals)] = additions
        else:
            lines[pos:pos] = additions
    return "\n".join(lines)


def _find_anchor(lines: list[str], anchor: str) -> int | None:
    a = anchor.strip()
    for i, line in enumerate(lines):
        if a and (a in line or line.strip().startswith(a)):
            return i
    return None


def _find_block(lines: list[str], removals: list[str], start: int) -> int | None:
    n = len(removals)
    for i in range(max(0, start - 20), min(len(lines) - n + 1, start + 40)):
        if lines[i:i + n] == removals:
            return i
    # fuzzy: allow ±1 line drift using difflib on windows
    best, best_ratio = None, 0.0
    window = max(n + 4, 6)
    for i in range(max(0, start - 20), min(len(lines) - window + 1, start + 40)):
        candidate = "\n".join(lines[i:i + max(n, 1)])
        ratio = difflib.SequenceMatcher(None, "\n".join(removals), candidate).ratio()
        if ratio > best_ratio:
            best, best_ratio = i, ratio
    return best if best is not None and best_ratio >= 0.6 else None


def _do_patch(args: dict, ctx: ToolCtx) -> ToolResult:
    patch = str(args.get("patch") or "")
    if not patch:
        return ToolResult("error: patch is required", is_error=True)
    try:
        report = apply_patch_text(patch, ctx.cwd, ctx)
    except PatchError as exc:
        return ToolResult(f"error: {exc}", is_error=True)
    return ToolResult(report or "(patch applied)")


def build_tools() -> list[Tool]:
    return [
        Tool(name="patch",
             description=("Apply a multi-file patch in the apply_patch DSL. Operations: "
                          "*** Add File:, *** Update File: with @@ context anchors and "
                          "-/+ line changes, *** Delete File:. Read files before updating."),
             parameters={"type": "object", "properties": {"patch": {"type": "string"}},
                         "required": ["patch"]},
             handler=_do_patch),
    ]
