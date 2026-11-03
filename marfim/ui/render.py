"""Terminal rendering — streaming text, tool banners, markdown-lite, diffs."""

from __future__ import annotations

import re
import sys

from marfim.util import ANSI, truncate_tail


def render_stream(text: str) -> None:
    sys.stdout.write(text)
    sys.stdout.flush()


def end_stream() -> None:
    sys.stdout.write("\n")
    sys.stdout.flush()


def render_tool_start(name: str, args: dict) -> None:
    shown: list[str] = []
    for key, value in list(args.items())[:3]:
        text = str(value).replace("\n", " ")[:60]
        shown.append(f"{key}={text}")
    label = ", ".join(shown)
    print(f"{ANSI.magenta('●')} {ANSI.bold(name)}{ANSI.gray(f'({label})')}")


def render_tool_result(name: str, is_error: bool) -> None:
    icon = ANSI.red("✗") if is_error else ANSI.green("✓")
    print(f"  {icon} {name} done")


def render_markdown(text: str) -> None:
    """Markdown-lite renderer: headings, bold, inline code, fenced blocks."""
    lines = text.split("\n")
    in_fence = False
    out: list[str] = []
    for line in lines:
        if line.strip().startswith("```"):
            in_fence = not in_fence
            out.append(ANSI.gray("┄" * 4) if in_fence else ANSI.gray("┄" * 4))
            continue
        if in_fence:
            out.append(ANSI.gray(line))
            continue
        if line.startswith("### "):
            out.append(ANSI.cyan(ANSI.bold(line[4:])))
        elif line.startswith("## "):
            out.append(ANSI.cyan(ANSI.bold(line[3:])))
        elif line.startswith("# "):
            out.append(ANSI.bold(line[2:]))
        elif re.match(r"^\s*[-*] ", line):
            out.append(re.sub(r"^(\s*)[-*] ", r"\1" + ANSI.gray("· "), line))
        elif re.match(r"^\s*\d+\. ", line):
            out.append(line)
        else:
            line = re.sub(r"\*\*([^*]+)\*\*", lambda m: ANSI.bold(m.group(1)), line)
            line = re.sub(r"`([^`]+)`", lambda m: ANSI.cyan(m.group(1)), line)
            out.append(line)
    print("\n".join(out))


def render_banner() -> None:
    from marfim import __version__
    print(ANSI.cyan(ANSI.bold(f"◆ Marfim v{__version__}")) + ANSI.gray(
        " — one harness, every discipline"))
    print(ANSI.gray("Type /help for commands · Ctrl-C interrupts · Ctrl-D exits"))


def render_permission_prompt(tool: str, args: dict, reason: str) -> str:
    detail = ""
    if tool == "bash":
        detail = str(args.get("command", ""))[:200]
    elif tool in ("write", "edit", "patch"):
        detail = str(args.get("file_path") or args.get("path") or "")[:200]
    print(f"{ANSI.yellow('⏵ permission requested')}: {ANSI.bold(tool)}"
          + (f" → {ANSI.cyan(detail)}" if detail else ""))
    print(ANSI.gray(f"  reason: {reason}"))
    try:
        answer = input(f"{ANSI.bold('[y]es / [n]o / [a]lways')}: ").strip().lower()
    except (EOFError, KeyboardInterrupt):
        return "deny"
    if answer in ("a", "always"):
        return "always"
    return "approve" if answer in ("y", "yes", "") else "deny"


def render_info(text: str) -> None:
    print(ANSI.gray(text))


def render_error(text: str) -> None:
    print(ANSI.red(f"✗ {text}"))


def render_success(text: str) -> None:
    print(ANSI.green(f"✓ {text}"))


def render_usage_bar(used: int, window: int) -> str:
    if window <= 0:
        return f"{used} tokens"
    pct = min(100, int(used * 100 / window))
    filled = pct // 10
    bar = "█" * filled + "░" * (10 - filled)
    color = ANSI.green if pct < 60 else (ANSI.yellow if pct < 85 else ANSI.red)
    return f"{color(bar)} {pct}% of {window // 1000}k"
