"""Small shared utilities: ANSI rendering, truncation, fuzzy matching ladder,
token estimation, ids.  Stdlib only.

The fuzzy-match ladder is a clean-room reimplementation of the *concept* proven
by Aider's editblock matcher (exact -> whitespace -> outdent -> ellipsis), see
docs/forensic/aider.md section 4.
"""

from __future__ import annotations

import difflib
import hashlib
import json
import os
import re
import sys
import time
import uuid
from dataclasses import dataclass, field
from pathlib import Path


# --------------------------------------------------------------------------- #
# ANSI / theming
# --------------------------------------------------------------------------- #

class Ansi:
    """Minimal ANSI styling helper. Degrades gracefully when not a TTY."""

    def __init__(self, enabled: bool | None = None):
        if enabled is None:
            enabled = sys.stdout.isatty() and os.environ.get("NO_COLOR") is None
        self.enabled = enabled

    def _c(self, code: str, text: str) -> str:
        return f"\x1b[{code}m{text}\x1b[0m" if self.enabled else text

    def bold(self, t: str) -> str:
        return self._c("1", t)

    def dim(self, t: str) -> str:
        return self._c("2", t)

    def red(self, t: str) -> str:
        return self._c("31", t)

    def green(self, t: str) -> str:
        return self._c("32", t)

    def yellow(self, t: str) -> str:
        return self._c("33", t)

    def blue(self, t: str) -> str:
        return self._c("34", t)

    def magenta(self, t: str) -> str:
        return self._c("35", t)

    def cyan(self, t: str) -> str:
        return self._c("36", t)

    def gray(self, t: str) -> str:
        return self._c("90", t)


ANSI = Ansi()


def strip_ansi(text: str) -> str:
    return re.sub(r"\x1b\[[0-9;]*m", "", text)


# --------------------------------------------------------------------------- #
# ids / time / hashing
# --------------------------------------------------------------------------- #

def gen_id(prefix: str = "") -> str:
    raw = uuid.uuid4().hex[:12]
    return f"{prefix}_{raw}" if prefix else raw


def now_iso() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime()) + "Z"


def sha_short(data: str | bytes, n: int = 12) -> str:
    if isinstance(data, str):
        data = data.encode("utf-8", "replace")
    return hashlib.sha256(data).hexdigest()[:n]


# --------------------------------------------------------------------------- #
# token estimation and truncation
# --------------------------------------------------------------------------- #

def estimate_tokens(text: str) -> int:
    """Cheap heuristic: ~4 chars per token (mixed code/prose)."""
    if not text:
        return 0
    return max(1, len(text) // 4)


def truncate_middle(text: str, limit: int, marker: str = "\n… [truncated {n} chars]") -> str:
    """Keep head and tail, elide the middle — REA-style summary-first output."""
    if len(text) <= limit:
        return text
    head = int(limit * 0.7)
    tail = limit - head
    omitted = len(text) - head - tail
    return text[:head] + marker.format(n=omitted) + text[-tail:]


def truncate_tail(text: str, limit: int, marker: str = " … [+{n} more chars]") -> str:
    if len(text) <= limit:
        return text
    return text[:limit] + marker.format(n=len(text) - limit)


def count_lines(text: str) -> int:
    return text.count("\n") + (0 if text.endswith("\n") or not text else 1)


# --------------------------------------------------------------------------- #
# Aider-style fuzzy search/replace ladder (clean-room)
# --------------------------------------------------------------------------- #

def _normalize_ws(block: str) -> str:
    return "\n".join(line.rstrip() for line in block.splitlines())


def _try_outdent(haystack: str, needle: str) -> str | None:
    """Find needle ignoring leading indentation, preserving relative indent."""
    hay_lines = haystack.split("\n")
    needle_lines = needle.split("\n")
    n = len(needle_lines)
    stripped = [ln.lstrip() for ln in needle_lines]
    leading = [len(needle_lines[i]) - len(stripped[i]) for i in range(n)]
    base = min(leading) if leading else 0
    for start in range(len(hay_lines) - n + 1):
        window = hay_lines[start : start + n]
        win_stripped = [ln.lstrip() for ln in window]
        if win_stripped != stripped:
            continue
        # reconstruct with the window's own indentation profile
        win_leading = [len(window[i]) - len(win_stripped[i]) for i in range(n)]
        shift = (min(win_leading) if win_leading else 0) - base
        rebuilt = []
        for i in range(n):
            pad = max(0, leading[i] + shift)
            rebuilt.append(" " * pad + stripped[i])
        return "\n".join(hay_lines[:start] + rebuilt + hay_lines[start + n :])
    return None


def _try_ellipsis(haystack: str, needle: str, replacement: str) -> str | None:
    """Support '...' inside the needle: the full matched region is replaced."""
    parts = needle.split("\n...\n")
    if len(parts) < 2:
        return None
    idx = haystack.find(parts[0])
    if idx == -1:
        return None
    rest_start = idx + len(parts[0])
    tail_idx = haystack.find(parts[-1], rest_start)
    if tail_idx == -1:
        return None
    return haystack[:idx] + replacement + haystack[tail_idx + len(parts[-1]) :]


def fuzzy_replace(haystack: str, needle: str, replacement: str) -> tuple[str, str] | None:
    """Apply one search/replace with an escalating match ladder.

    Returns (new_text, strategy) or None when nothing matches.
    Strategies: exact, rstrip, outdent, ellipsis, nearest (difflib >=0.85).
    """
    if needle in haystack:
        return haystack.replace(needle, replacement, 1), "exact"

    ws_hay, ws_needle = _normalize_ws(haystack), _normalize_ws(needle)
    if ws_needle in ws_hay:
        # replace in normalized space by locating line range
        hay_lines = haystack.split("\n")
        needle_lines = ws_needle.split("\n")
        hay_norm = [ln.rstrip() for ln in hay_lines]
        n = len(needle_lines)
        for start in range(len(hay_norm) - n + 1):
            if hay_norm[start : start + n] == needle_lines:
                new_lines = hay_lines[:start] + replacement.split("\n") + hay_lines[start + n :]
                return "\n".join(new_lines), "rstrip"

    out = _try_outdent(haystack, needle)
    if out is not None:
        return out, "outdent"

    ell = _try_ellipsis(haystack, needle, replacement)
    if ell is not None:
        return ell, "ellipsis"

    # nearest-match fallback (Aider ships this as a hint; we apply + report)
    matches = difflib.get_close_matches(needle, [haystack[max(0, i - len(needle) - 40) : i + len(needle) + 40]
                                                 for i in range(0, len(haystack), 200)] or [haystack],
                                        n=1, cutoff=0.85)
    if matches:
        candidate = matches[0]
        ratio = difflib.SequenceMatcher(None, needle, candidate).ratio()
        if ratio >= 0.85 and candidate in haystack:
            return haystack.replace(candidate, replacement, 1), f"nearest({ratio:.2f})"
    return None


# --------------------------------------------------------------------------- #
# JSON safety
# --------------------------------------------------------------------------- #

def json_dumps(obj) -> str:
    return json.dumps(obj, ensure_ascii=False, separators=(",", ":"), default=str)


def json_loads(text: str, default=None):
    try:
        return json.loads(text)
    except (json.JSONDecodeError, TypeError):
        return default


def repair_json_args(text: str) -> dict:
    """Best-effort parse of model-produced tool arguments.

    Handles raw dicts, JSON with trailing prose, and single quotes.
    """
    if isinstance(text, dict):
        return text
    if not text or not text.strip():
        return {}
    text = text.strip()
    try:
        parsed = json.loads(text)
        return parsed if isinstance(parsed, dict) else {"value": parsed}
    except json.JSONDecodeError:
        pass
    # strip markdown fences
    fence = re.search(r"```(?:json)?\s*(.*?)```", text, re.S)
    if fence:
        try:
            parsed = json.loads(fence.group(1).strip())
            return parsed if isinstance(parsed, dict) else {"value": parsed}
        except json.JSONDecodeError:
            pass
    # first { to last }
    start, end = text.find("{"), text.rfind("}")
    if start != -1 and end > start:
        try:
            parsed = json.loads(text[start : end + 1])
            return parsed if isinstance(parsed, dict) else {"value": parsed}
        except json.JSONDecodeError:
            pass
    return {"raw": text}
