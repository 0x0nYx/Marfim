#!/usr/bin/env bash
# Example PreToolUse hook (Claude Code hook contract, exit 2 = veto).
# Receives JSON on stdin: {"event":"PreToolUse","tool":"bash","input":{...}}
# This example blocks any bash command that mentions a probable secret literal.
import sys, json, re

def main() -> int:
    try:
        payload = json.load(sys.stdin)
    except json.JSONDecodeError:
        return 0
    tool = payload.get("tool", "")
    cmd = str((payload.get("input") or {}).get("command", "")) if tool == "bash" else ""
    if re.search(r"(sk-[A-Za-z0-9]{16,}|ghp_[A-Za-z0-9]{20,})", cmd):
        print("possible secret literal in command", file=sys.stderr)
        return 2
    return 0

if __name__ == "__main__":
    sys.exit(main())
