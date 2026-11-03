"""Small shared utilities: ANSI rendering, truncation, fuzzy matching ladder,
token estimation, ids.  Stdlib only.

The fuzzy-match ladder is a clean-room reimplementation of the *concept* proven
by Aider's editblock matcher (exact -> whitespace -> outdent -> ellipsis), see
docs/forensic/aider.md section 4.  The dangerous `nearest` strategy was removed
(review finding 13): a near-miss is now reported as a HINT, never applied.
"""

from __future__ import annotations

import difflib
import hashlib
import json
import os
import re
import subprocess  # noqa: F401  (re-exported for callers below)
import sys
import time
import uuid
from dataclasses import dataclass, field
from pathlib import Path


# --------------------------------------------------------------------------- #
# filesystem integrity helpers (review findings 15–17)
# --------------------------------------------------------------------------- #


class FileIntegrityError(Exception):
    """Raised when a file cannot be edited without corrupting it."""


@dataclass
class TextFile:
    """A file decoded with its original encoding + newline style preserved."""

    path: Path
    data: bytes
    text: str            # decoded, newlines normalised to \n
    encoding: str        # utf-8 | latin-1 (both round-trip)
    newline: str         # \\n | \\r\\n

    @property
    def sha256(self) -> str:
        return hashlib.sha256(self.data).hexdigest()

    @property
    def mtime_ns(self) -> int:
        try:
            return self.path.stat().st_mtime_ns
        except OSError:
            return 0

    def encode_back(self, text: str) -> bytes:
        """Re-apply the original newline style and encoding."""
        if self.newline == "\r\n":
            text = text.replace("\r\n", "\n").replace("\n", "\r\n")
        return text.encode(self.encoding)


def read_text_preserve(path: Path) -> TextFile:
    """Read bytes, detect encoding + newline style, never silently replace
    characters (review finding 17).  Refuses NUL-containing (binary) files
    and mixed line endings."""
    data = path.read_bytes()
    if b"\x00" in data:
        raise FileIntegrityError(f"{path} contains NUL bytes (binary); refusing to edit")
    try:
        text = data.decode("utf-8")
        encoding = "utf-8"
    except UnicodeDecodeError:
        try:
            text = data.decode("latin-1")   # latin-1 maps every byte: round-trips
            encoding = "latin-1"
        except UnicodeDecodeError as exc:   # pragma: no cover - latin-1 never fails
            raise FileIntegrityError(f"{path}: cannot decode: {exc}") from exc
    crlf = text.count("\r\n")
    lone_lf = text.replace("\r\n", "").count("\n")
    lone_cr = text.replace("\r\n", "").count("\r")
    if lone_cr and not crlf:
        raise FileIntegrityError(f"{path}: CR-only line endings; refusing to edit")
    if crlf and lone_lf:
        raise FileIntegrityError(
            f"{path}: mixed CRLF and LF line endings; refusing to edit "
            f"(normalise first to avoid corrupting the file)")
    newline = "\r\n" if crlf and not lone_lf else "\n"
    if newline == "\r\n":
        text = text.replace("\r\n", "\n")
    return TextFile(path=path, data=data, text=text, encoding=encoding, newline=newline)


def atomic_write_bytes(path: Path, data: bytes) -> None:
    """Write via temp file + os.replace, preserving the previous file mode
    (review finding 15)."""
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    mode = path.stat().st_mode & 0o777 if path.exists() else 0o644
    tmp = path.with_name(path.name + ".marfim-tmp")
    try:
        with open(tmp, "wb") as fh:
            fh.write(data)
            fh.flush()
            os.fsync(fh.fileno())
        os.chmod(tmp, mode)
        os.replace(tmp, path)
    finally:
        if tmp.exists():
            tmp.unlink(missing_ok=True)


def atomic_write_text(path: Path, text: str, encoding: str = "utf-8",
                      newline: str = "\n") -> None:
    if newline == "\r\n":
        text = text.replace("\r\n", "\n").replace("\n", "\r\n")
    atomic_write_bytes(path, text.encode(encoding))


def strict_secret_env(env: dict | None = None) -> dict:
    """A scrubbed environment: PATH/HOME/locale survive, every credential-ish
    variable is dropped (review findings 10 & 32: children must not inherit
    every API key)."""
    src = dict(os.environ if env is None else env)
    keep = {}
    for key, val in src.items():
        up = key.upper()
        if any(tag in up for tag in ("KEY", "TOKEN", "SECRET", "PASSWORD", "PASSWD",
                                     "CREDENTIAL", "AUTH", "COOKIE", "SSH")):
            continue
        if key in ("PATH", "HOME", "SHELL", "USER", "LOGNAME", "LANG", "TERM",
                   "TMPDIR", "PWD", "SHLVL", "LC_ALL", "LC_CTYPE", "NO_COLOR",
                   "MARFIM_EVENT", "MARFIM_TOOL", "MARFIM_HOME"):
            keep[key] = val
    return keep


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


def _try_outdent(haystack: str, needle: str, replacement: str) -> str | None:
    """Find needle ignoring leading indentation; write `replacement` re-based to
    the window's indentation (review finding 12: this strategy previously
    returned the haystack unchanged and reported success)."""
    hay_lines = haystack.split("\n")
    needle_lines = needle.split("\n")
    repl_lines = replacement.split("\n")
    n = len(needle_lines)
    stripped = [ln.lstrip() for ln in needle_lines]
    leading = [len(needle_lines[i]) - len(stripped[i]) for i in range(n)]
    base = min(leading) if leading else 0
    for start in range(len(hay_lines) - n + 1):
        window = hay_lines[start : start + n]
        win_stripped = [ln.lstrip() for ln in window]
        if win_stripped != stripped:
            continue
        # re-base the replacement to the window's own base indentation
        win_leading = [len(window[i]) - len(win_stripped[i]) for i in range(n)]
        shift = (min(win_leading) if win_leading else 0) - base
        repl_stripped = [ln.lstrip() for ln in repl_lines]
        repl_leading = [len(repl_lines[i]) - len(repl_stripped[i]) for i in range(len(repl_lines))]
        rebased = []
        for i, line in enumerate(repl_stripped):
            pad = max(0, repl_leading[i] + shift)
            rebased.append(" " * pad + line)
        return "\n".join(hay_lines[:start] + rebased + hay_lines[start + n :])
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
    Strategies: exact, rstrip, outdent, ellipsis.  The old `nearest` strategy
    was REMOVED (review finding 13): it replaced an arbitrary window and ate
    neighbouring code.  Use `nearest_hint` to surface a suggestion instead.
    """
    if needle in haystack:
        out = haystack.replace(needle, replacement, 1)
        return (out, "exact") if out != haystack else None

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
                out = "\n".join(new_lines)
                return (out, "rstrip") if out != haystack else None

    out = _try_outdent(haystack, needle, replacement)
    if out is not None and out != haystack:
        return out, "outdent"

    ell = _try_ellipsis(haystack, needle, replacement)
    if ell is not None and ell != haystack:
        return ell, "ellipsis"

    return None


def nearest_hint(haystack: str, needle: str) -> tuple[str, float] | None:
    """Locate the closest whole-line window for a failed needle and return it
    as a HINT (never applied — review finding 13)."""
    if not needle or not haystack:
        return None
    hay_lines = haystack.split("\n")
    n = max(1, len(needle.split("\n")))
    best, best_ratio = None, 0.0
    sm = difflib.SequenceMatcher(None, needle, "")
    for start in range(0, max(1, len(hay_lines) - n + 1)):
        window = "\n".join(hay_lines[start : start + n])
        sm.set_seq2(window)
        ratio = sm.ratio()
        if ratio > best_ratio:
            best, best_ratio = window, ratio
    if best is not None and best_ratio >= 0.6:
        return best, best_ratio
    return None


# --------------------------------------------------------------------------- #
# shell segmentation + TOML writing
# --------------------------------------------------------------------------- #

SHELL_OPERATORS = ("&&", "||", ";", "|")


def split_shell_segments(command: str) -> list[str]:
    """Split a shell command into segments on ; | && || (quote-aware, review
    finding 6: an 'always' grant must cover EVERY segment, not a prefix)."""
    segments: list[str] = []
    buf: list[str] = []
    quote: str = ""
    i = 0
    while i < len(command):
        ch = command[i]
        if quote:
            buf.append(ch)
            if ch == quote:
                quote = ""
            i += 1
            continue
        if ch in ("'", '"'):
            quote = ch
            buf.append(ch)
            i += 1
            continue
        if ch == "\\" and i + 1 < len(command):
            buf.append(command[i:i + 2])
            i += 2
            continue
        two = command[i:i + 2]
        if two in ("&&", "||"):
            segments.append("".join(buf))
            buf = []
            i += 2
            continue
        if ch in ";|":
            segments.append("".join(buf))
            buf = []
            i += 1
            continue
        buf.append(ch)
        i += 1
    segments.append("".join(buf))
    return [s.strip() for s in segments if s.strip()]


def toml_quote(value: str) -> str:
    """Escape a string as a TOML basic string (JSON escaping is TOML-valid):
    review finding 30 — f-string interpolation broke on quotes."""
    return json.dumps(value, ensure_ascii=False)


def toml_basic_value(value) -> str:
    if isinstance(value, str):
        return toml_quote(value)
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (int, float)):
        return str(value)
    if isinstance(value, list):
        return "[" + ", ".join(toml_basic_value(v) for v in value) + "]"
    return toml_quote(str(value))


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


def _json_loads_lenient(text: str):
    """json.loads with strict=False (raw control chars inside strings are common
    with open models — review finding 19) plus trailing-comma tolerance."""
    try:
        return json.loads(text, strict=False)
    except json.JSONDecodeError:
        pass
    repaired = re.sub(r",(\s*[}\]])", r"\1", text)
    if repaired != text:
        try:
            return json.loads(repaired, strict=False)
        except json.JSONDecodeError:
            pass
    raise json.JSONDecodeError("invalid JSON", text, 0)


def repair_json_args(text: str) -> dict:
    """Best-effort parse of model-produced tool arguments.

    Handles raw dicts, JSON with trailing prose, raw newlines inside strings
    (strict=False), single quotes, and markdown fences.  Raises nothing:
    unparseable input comes back as {"raw": text} so callers can surface an
    honest parse error to the model (review finding 19).
    """
    if isinstance(text, dict):
        return text
    if not text or not text.strip():
        return {}
    text = text.strip()
    try:
        parsed = _json_loads_lenient(text)
        return parsed if isinstance(parsed, dict) else {"value": parsed}
    except (json.JSONDecodeError, ValueError):
        pass
    # strip markdown fences
    fence = re.search(r"```(?:json)?\s*(.*?)```", text, re.S)
    if fence:
        try:
            parsed = _json_loads_lenient(fence.group(1).strip())
            return parsed if isinstance(parsed, dict) else {"value": parsed}
        except (json.JSONDecodeError, ValueError):
            pass
    # first { to last }
    start, end = text.find("{"), text.rfind("}")
    if start != -1 and end > start:
        try:
            parsed = _json_loads_lenient(text[start : end + 1])
            return parsed if isinstance(parsed, dict) else {"value": parsed}
        except (json.JSONDecodeError, ValueError):
            pass
    return {"raw": text}
