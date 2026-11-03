"""Permission engine — layered safety synthesis.

Doctrine merged (see docs/FEATURE-MATRIX.md §Permissions):
- Claude Code: ordered allow/ask/deny rules with tool-specifier patterns
  ("Bash(git *)", "Edit(src/**)"); permission modes default/plan/acceptEdits/bypass.
- Codex: sandbox modes + approval policies; never auto-run what policy denies.
- Goose: undecidable requests default to "ask"; per-tool permission tiers.
- ECC: config-protection — the harness never lets the model weaken its own
  guardrails; read-before-edit evidence (GateGuard lite).
"""

from __future__ import annotations

import fnmatch
import os
import shlex
from dataclasses import dataclass, field
from enum import Enum
from pathlib import Path

from marfim.config import Config, MARFIM_DIR


class Mode(str, Enum):
    DEFAULT = "default"          # ask for mutations, allow read-only
    PLAN = "plan"                # read-only analysis mode
    ACCEPT_EDITS = "accept-edits"  # auto-accept file edits, still gate bash
    BYPASS = "bypass"            # approve all (yolo)


class Decision(str, Enum):
    ALLOW = "allow"
    ASK = "ask"
    DENY = "deny"


@dataclass
class Ruling:
    decision: Decision
    reason: str = ""


READONLY_TOOLS = {"read", "grep", "glob", "todo", "repomap", "skill", "usage_report"}

# bash command prefixes considered read-only (plan mode allowlist)
READONLY_BASH_PREFIXES = (
    "ls", "cat", "head", "tail", "grep", "rg", "find", "pwd", "echo", "wc",
    "git status", "git log", "git diff", "git show", "git branch", "git rev-parse",
    "python -c", "python3 -c", "which", "whoami", "date", "du", "df", "file",
)

PROTECTED_PATTERNS = [
    f"{MARFIM_DIR}/config.toml", ".marfim/config.toml", "MARFIM.md",
    ".marfim/hooks/*", ".git/*", ".gitignore",
]


@dataclass
class PermissionEngine:
    config: Config
    mode: Mode = Mode.DEFAULT
    session_grants: list = field(default_factory=list)  # saved "always" rules

    @classmethod
    def from_config(cls, config: Config) -> "PermissionEngine":
        try:
            mode = Mode(config.mode)
        except ValueError:
            mode = Mode.DEFAULT
        return cls(config=config, mode=mode)

    # ------------------------------------------------------------------ core

    def check(self, tool_name: str, args: dict, project_root: Path) -> Ruling:
        """Ordered evaluation: deny > protect > session-grant > allow > ask-fallback."""
        if self.mode is Mode.BYPASS:
            return Ruling(Decision.ALLOW, "bypass mode")

        patterns_ask = list(self.config.ask)
        patterns_allow = list(self.config.allow) + list(self.session_grants)
        patterns_deny = list(self.config.deny)

        target = self._rule_target(tool_name, args)

        for rule in patterns_deny:
            if self._match(rule, tool_name, target):
                return Ruling(Decision.DENY, f"denied by rule: {rule}")

        if self.config.protect_configs:
            hit = self._protected_target(tool_name, args, project_root)
            if hit:
                return Ruling(Decision.DENY,
                              f"ECC config-protection: {hit} is a harness-control file; "
                              f"the model cannot edit it (unset permissions.protect_configs or "
                              f"use /permissions bypass to override)")

        for rule in patterns_allow:
            if self._match(rule, tool_name, target):
                return Ruling(Decision.ALLOW, f"allowed by rule: {rule}")

        # mode-level behavior
        readonly = self._is_readonly(tool_name, args)
        if self.mode is Mode.PLAN:
            if readonly:
                return Ruling(Decision.ALLOW, "plan mode allows read-only")
            return Ruling(Decision.DENY, "plan mode is read-only (run /permissions accept-edits "
                                          "or exit plan to mutate)")
        if tool_name in ("write", "edit", "patch", "multi_edit"):
            if self.mode is Mode.ACCEPT_EDITS:
                return Ruling(Decision.ALLOW, "accept-edits mode")
            return Ruling(Decision.ASK, "file mutation requires approval")
        if tool_name == "bash":
            return Ruling(Decision.ASK, "shell command requires approval")
        if tool_name == "task":
            return Ruling(Decision.ALLOW, "subagents permitted")
        if tool_name.startswith("mcp__"):
            return Ruling(Decision.ASK, "MCP tool requires approval")

        for rule in patterns_ask:
            if self._match(rule, tool_name, target):
                return Ruling(Decision.ASK, f"ask by rule: {rule}")

        if readonly:
            return Ruling(Decision.ALLOW, "read-only tool")
        return Ruling(Decision.ASK, "undecidable — defaulting to ask (goose doctrine)")

    # ---------------------------------------------------------------- helpers

    @staticmethod
    def _rule_target(tool_name: str, args: dict) -> str:
        if tool_name == "bash":
            return str(args.get("command", ""))
        for key in ("file_path", "path", "notebook_path"):
            if key in args:
                return str(args[key])
        if "url" in args:
            return str(args["url"])
        if "query" in args:
            return str(args["query"])
        return ""

    @staticmethod
    def _match(rule: str, tool_name: str, target: str) -> bool:
        rule = rule.strip()
        if "(" in rule and rule.endswith(")"):
            name, inner = rule.split("(", 1)
            name = name.strip()
            inner = inner[:-1].strip()
        else:
            name, inner = rule, "*"
        if name != "*" and name.lower() != tool_name.lower():
            return False
        if not target:
            return name.lower() == tool_name.lower() or name == "*"
        return fnmatch.fnmatch(target, inner) or fnmatch.fnmatch(target, inner.rstrip("*") + "*")

    def _protected_target(self, tool_name: str, args: dict, project_root: Path) -> str | None:
        if tool_name not in ("write", "edit", "patch"):
            return None
        raw = args.get("file_path") or args.get("path") or ""
        if not raw:
            return None
        try:
            rel = str(Path(raw).resolve().relative_to(project_root.resolve()))
        except ValueError:
            rel = Path(raw).name
        for pattern in PROTECTED_PATTERNS:
            if fnmatch.fnmatch(rel, pattern) or fnmatch.fnmatch(rel, f"**/{pattern}"):
                return rel
        return None

    @staticmethod
    def _is_readonly(tool_name: str, args: dict) -> bool:
        if tool_name in READONLY_TOOLS:
            return True
        if tool_name == "bash":
            try:
                parts = shlex.split(str(args.get("command", "")))
            except ValueError:
                parts = str(args.get("command", "")).split()
            if not parts:
                return True
            cmd = " ".join(parts[:2]) if parts[0] == "git" else parts[0]
            return any(cmd == p or cmd.startswith(p) for p in READONLY_BASH_PREFIXES)
        return False

    def grant_always(self, tool_name: str, args: dict) -> str:
        """Persist an 'always allow' grant for this invocation shape."""
        target = self._rule_target(tool_name, args)
        if tool_name == "bash" and target:
            try:
                parts = shlex.split(target)
                base = " ".join(parts[:2]) if parts and parts[0] == "git" else (parts[0] if parts else "*")
            except ValueError:
                base = target.split()[0] if target.split() else "*"
            rule = f"Bash({base} *)"
        elif tool_name in ("write", "edit", "patch"):
            rule = f"{tool_name.capitalize()}(*)"
        else:
            rule = f"{tool_name}(*)"
        if rule not in self.session_grants:
            self.session_grants.append(rule)
        return rule
