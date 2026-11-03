"""Permission engine — layered safety synthesis.

Doctrine merged (see docs/FEATURE-MATRIX.md §Permissions):
- Claude Code: ordered allow/ask/deny rules with tool-specifier patterns
  ("Bash(git *)", "Edit(src/**)"); permission modes default/plan/acceptEdits/bypass.
- Codex: sandbox modes + approval policies; never auto-run what policy denies.
- Goose: undecidable requests default to "ask"; per-tool permission tiers.
- ECC: config-protection — the harness never lets the model weaken its own
  guardrails; read-before-edit evidence (GateGuard lite).
- Node-tree port (review §3): segment-aware bash rule matching with allow vs
  restrict semantics; exact-command "always" grants; path containment.

Evaluation order (fixed by review findings 8 & 9 — deny/protect run in EVERY
mode, including bypass):
  1. hard containment (secret-like paths; writes outside the workspace)
  2. deny rules (segment-aware for bash)
  3. ECC config protection (patch targets included)
  4. role tool allowlist (subagents cannot escape it via bypass)
  5. bypass mode
  6. session grants + allow rules (bash: EVERY segment must be covered)
  7. mode behaviour (plan / accept-edits)
  8. ask rules, read-only allowances, ask fallback
"""

from __future__ import annotations

import fnmatch
import re
import shlex
from dataclasses import dataclass, field
from enum import Enum
from pathlib import Path

from marfim.config import Config, MARFIM_DIR, marfim_home
from marfim.util import split_shell_segments


class Mode(str, Enum):
    DEFAULT = "default"          # ask for mutations, allow read-only
    PLAN = "plan"                # read-only analysis mode
    ACCEPT_EDITS = "accept-edits"  # auto-accept file edits, still gate bash
    BYPASS = "bypass"            # approve all (yolo) — still under deny/protect


class Decision(str, Enum):
    ALLOW = "allow"
    ASK = "ask"
    DENY = "deny"


@dataclass
class Ruling:
    decision: Decision
    reason: str = ""


READONLY_TOOLS = {"read", "grep", "glob", "todo", "repomap", "skill", "usage_report"}

# never persist an "always" grant for these (review finding 6): their exact
# form can still pivot (interpreters run anything; git touches the VCS)
NEVER_GRANT_BASH = {
    "sh", "bash", "zsh", "dash", "env", "xargs", "sudo", "doas", "su",
    "curl", "wget", "git", "python", "python3", "python3.11", "python3.12",
    "node", "npm", "npx", "pnpm", "yarn", "deno", "bun", "perl", "ruby",
    "php", "lua", "eval", "exec", "source", "find", "awk", "sed", "tee",
    "mkfifo", "nc", "ncat", "socat", "ssh", "scp", "docker", "kubectl",
}

MUTATION_TOOLS = ("write", "edit", "patch", "multi_edit", "apply_patch")

# Secret-like targets are denied in EVERY mode (review finding 9).
SECRET_DIR_NAMES = {".ssh", ".aws", ".gnupg", ".gpg", ".kube", ".docker", ".password-store"}
SECRET_FILE_NAMES = {
    ".env", ".netrc", ".pgpass", ".npmrc", ".pypirc", "id_rsa", "id_dsa",
    "id_ecdsa", "id_ed25519", ".git-credentials", "credentials.json",
    ".htpasswd", ".gitconfig.local",
}
SECRET_REL_PATTERNS = (".git/config", ".ssh/*", ".aws/*", ".env.*", "*.pem", "*.key")

PROTECTED_PATTERNS = [
    f"{MARFIM_DIR}/config.toml", ".marfim/config.toml", "MARFIM.md",
    ".marfim/hooks/*", ".git/*", ".gitignore",
]
# absolute, home-relative protections (the global config lives outside the
# project, so the relative patterns above can never match it — finding 9)
HOME_PROTECTED = ["config.toml", "trust.json", "MARFIM.md", "hooks/*"]


def _flag_split(token: str) -> str:
    """Normalize `--flag=value` to `--flag` for allowlist checks."""
    return token.split("=", 1)[0] if token.startswith("--") and "=" in token else token


def _composite_ok(token: str, allowed: set[str]) -> bool:
    """`-la` style bundles are OK when every letter is an allowed flag."""
    if not token.startswith("-") or token.startswith("--") or len(token) < 3:
        return False
    singles = {t[1:] for t in allowed if len(t) == 2 and t.startswith("-")}
    return set(token[1:]) <= singles


# plan-mode allowlist: command -> allowed flags (review finding 4: real flag
# sets, no `find`/`git branch`/`echo`/`python -c`, operators rejected outright)
READONLY_BASH: dict[str, set[str]] = {
    "ls": {"-a", "-l", "-h", "-R", "-r", "-t", "-S", "-1", "-d", "-F", "-i",
           "--all", "--almost-all", "--long", "--human-readable", "--color",
           "--reverse", "--sort", "--time", "--indicator-style"},
    "cat": {"-n", "-b", "-A", "-v", "-e", "-t", "-s", "-E", "-T"},
    "head": {"-n", "-c", "-q", "-v", "--lines", "--bytes"},
    "tail": {"-n", "-c", "-q", "-v", "--lines", "--bytes"},
    "grep": {"-n", "-i", "-r", "-R", "-l", "-c", "-v", "-e", "-E", "-F", "-w",
             "-x", "-A", "-B", "-C", "-H", "-h", "-o", "-q", "-s", "--include",
             "--exclude", "--exclude-dir", "--count", "--files-with-matches"},
    "rg": {"-n", "-i", "-l", "-c", "-v", "-e", "-F", "-w", "-x", "-A", "-B",
           "-C", "-H", "-o", "-q", "-s", "-S", "-M", "-m", "-t", "-T",
           "--glob", "--iglob", "--max-count", "--no-heading", "--type",
           "--type-not", "--sort", "--files", "--hidden", "--no-ignore-vcs"},
    "pwd": set(), "which": set(), "whoami": set(), "hostname": set(),
    "date": {"-u", "-R", "-I", "-d", "--date", "--iso-8601", "--rfc-3339"},
    "wc": {"-l", "-w", "-c", "-m", "--lines", "--words", "--chars"},
    "du": {"-h", "-s", "-a", "-d", "--max-depth", "--summarize"},
    "df": {"-h", "-k", "-m", "-T", "--output"},
    "file": {"-b", "-i", "-L", "--mime-type"},
    "git status": {"-s", "-b", "-sb", "--short", "--branch", "--porcelain", "--long"},
    "git log": {"--oneline", "-n", "--format", "--graph", "--all", "--stat",
                "--decorate", "--follow", "--max-count", "--since", "--until",
                "--author", "--date"},
    "git diff": {"--stat", "--cached", "--name-only", "--name-status", "-U",
                 "--unified", "--shortstat", "--numstat", "--no-color", "--staged"},
    "git show": {"--stat", "--name-only", "--format", "--no-color", "-s"},
    "git rev-parse": {"--show-toplevel", "--git-dir", "--short", "--verify", "--is-inside-work-tree"},
}

BASH_OPERATOR_CHARS = set(";|&><`$\n\r(")


def command_is_readonly(command: str) -> tuple[bool, str]:
    """Real shell-lexer check for plan mode (review finding 4).  Returns
    (is_readonly, reason)."""
    raw = command.strip()
    if not raw:
        return True, "empty"
    if any(ch in BASH_OPERATOR_CHARS for ch in raw):
        return False, "shell operator or substitution present"
    if raw.startswith(("$", "{")):
        return False, "expansion"
    try:
        parts = shlex.split(raw)
    except ValueError:
        return False, "unparseable quoting"
    if not parts:
        return True, "empty"
    # look up `git <subcommand>` as a unit
    key = " ".join(parts[:2]) if parts[0] == "git" and len(parts) > 1 else parts[0]
    allowed = READONLY_BASH.get(key)
    if allowed is None:
        if parts[0] == "git":
            return False, f"git subcommand '{key[4:]}' is not read-only"
        return False, f"command '{parts[0]}' is not on the read-only list"
    for tok in parts[1:]:
        if not tok.startswith("-"):
            continue
        if re.fullmatch(r"-\d+", tok):
            continue    # numeric count args like `git log -5`
        flag = _flag_split(tok)
        if flag in allowed or tok in allowed or _composite_ok(tok, allowed):
            continue
        return False, f"flag '{tok}' is not allowlisted for '{key}'"
    return True, "allowlisted read-only command"


@dataclass
class PermissionEngine:
    config: Config
    mode: Mode = Mode.DEFAULT
    session_grants: list = field(default_factory=list)  # saved "always" rules
    readonly_tools: set = field(default_factory=set)    # wired from the live toolset
    role_tool_allow: list | None = None                 # subagent role allowlist

    @classmethod
    def from_config(cls, config: Config) -> "PermissionEngine":
        try:
            mode = Mode(config.mode)
        except ValueError:
            mode = Mode.DEFAULT
        return cls(config=config, mode=mode)

    def child_for_role(self, role: str, role_tools: list[str] | None) -> "PermissionEngine":
        """Engine for a subagent: same config/mode, but restricted to the role
        toolset (review finding 25 — bypass must not leak into explore/plan)."""
        return PermissionEngine(config=self.config, mode=self.mode,
                                role_tool_allow=list(role_tools) if role_tools else None)

    # ------------------------------------------------------------------ core

    def check(self, tool_name: str, args: dict, project_root: Path) -> Ruling:
        """Ordered evaluation — see module docstring."""
        # 1. hard containment, every mode
        hard = self._containment(tool_name, args, project_root)
        if hard is not None:
            return hard

        patterns_ask = list(self.config.ask)
        patterns_allow = list(self.config.allow) + list(self.session_grants)
        patterns_deny = list(self.config.deny)
        target = self._rule_target(tool_name, args)
        segments = split_shell_segments(target) if tool_name == "bash" else None

        # 2. deny rules — every mode, bypass included (finding 8); a compound
        # command is denied when ANY segment is denied
        for rule in patterns_deny:
            if segments is not None:
                if any(self._single_match(rule, tool_name, seg) for seg in segments):
                    return Ruling(Decision.DENY, f"denied by rule: {rule}")
            elif self._single_match(rule, tool_name, target):
                return Ruling(Decision.DENY, f"denied by rule: {rule}")

        # 3. ECC config protection — every mode (finding 8); patch-aware (finding 3)
        hit = self._protected_target(tool_name, args, project_root)
        if hit:
            return Ruling(Decision.DENY,
                          f"ECC config-protection: {hit} is a harness-control file; "
                          f"the model cannot edit it (unset permissions.protect_configs "
                          f"in the GLOBAL config to override)")

        # 4. role allowlist — subagents stay restricted even in bypass (finding 25)
        if self.role_tool_allow is not None and tool_name not in self.role_tool_allow:
            return Ruling(Decision.DENY,
                          f"role allowlist: tool '{tool_name}' is not permitted for this "
                          f"subagent role (allowed: {', '.join(self.role_tool_allow)})")

        # 5. bypass
        if self.mode is Mode.BYPASS:
            return Ruling(Decision.ALLOW, "bypass mode")

        # 6. session grants + config allow rules.  Bash is segment-aware
        # (finding 6): EVERY segment of the command must be covered by SOME
        # allow rule, so `cat a && curl evil` can never ride a `cat` grant.
        if segments is not None and patterns_allow:
            if all(any(self._single_match(rule, tool_name, seg) for rule in patterns_allow)
                   for seg in segments):
                return Ruling(Decision.ALLOW, "allowed by rule(s)")
        else:
            for rule in patterns_allow:
                if self._single_match(rule, tool_name, target):
                    return Ruling(Decision.ALLOW, f"allowed by rule: {rule}")

        # 7. mode-level behavior
        readonly = self._is_readonly(tool_name, args)
        if self.mode is Mode.PLAN:
            if readonly:
                return Ruling(Decision.ALLOW, "plan mode allows read-only")
            return Ruling(Decision.DENY, "plan mode is read-only (run /permissions accept-edits "
                                         "or exit plan to mutate)")
        if tool_name in MUTATION_TOOLS:
            if self.mode is Mode.ACCEPT_EDITS:
                hard_write = self._containment(tool_name, args, project_root)
                if hard_write is not None:
                    return hard_write
                return Ruling(Decision.ALLOW, "accept-edits mode")
            return Ruling(Decision.ASK, "file mutation requires approval")
        if tool_name == "bash":
            return Ruling(Decision.ASK, "shell command requires approval")
        if tool_name == "task":
            return Ruling(Decision.ALLOW, "subagents permitted")
        if tool_name == "verify":
            return Ruling(Decision.ALLOW, "verification is read-only by contract")
        if tool_name.startswith("mcp__"):
            return Ruling(Decision.ASK, "MCP tool requires approval")

        # 8. ask rules, then read-only allowance, then goose fallback
        for rule in patterns_ask:
            if self._single_match(rule, tool_name, target):
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

    def _tool_paths(self, tool_name: str, args: dict, project_root: Path) -> list[Path]:
        """Every filesystem path a tool call touches (finding 3: patch carries
        its paths inside the patch text)."""
        paths: list[str] = []
        if tool_name == "bash":
            return []
        if tool_name in ("patch", "apply_patch"):
            try:
                from marfim.tools.patch import parse_patch
                for op in parse_patch(str(args.get("patch") or "")):
                    paths.append(op["path"])
            except Exception:
                raw = str(args.get("patch") or "")
                for line in raw.splitlines():
                    if line.startswith("*** ") and ":" in line:
                        paths.append(line.split(":", 1)[1].strip())
            for p in paths:
                yield_path = Path(p).expanduser()
                if not yield_path.is_absolute():
                    yield_path = project_root / yield_path
                yield yield_path.resolve()
            return
        for key in ("file_path", "path", "notebook_path"):
            if key in args and args[key]:
                raw = str(args[key])
                p = Path(raw).expanduser()
                if not p.is_absolute():
                    p = project_root / raw
                yield p.resolve()

    def _containment(self, tool_name: str, args: dict, project_root: Path) -> Ruling | None:
        """Hard denials for every mode: secret-like targets and writes outside
        the workspace (review finding 9)."""
        if tool_name == "bash":
            return None
        roots = [project_root.resolve()]
        for d in self.config.effective_dirs():
            try:
                roots.append(Path(d).expanduser().resolve())
            except OSError:
                continue
        home = marfim_home().resolve()
        session_dir = Path(self.config.session_dir).expanduser().resolve() \
            if self.config.session_dir else None
        for path in self._tool_paths(tool_name, args, project_root):
            spath = str(path)
            # secret-like: anywhere, any mode
            parts = set(path.parts)
            if parts & SECRET_DIR_NAMES or path.name in SECRET_FILE_NAMES:
                return Ruling(Decision.DENY,
                              f"refusing secret-like path: {spath} (harness doctrine: "
                              f"credentials never cross a tool boundary)")
            rel = _rel_or_none(path, project_root)
            if rel is not None:
                for pat in SECRET_REL_PATTERNS:
                    if fnmatch.fnmatch(rel, pat) or fnmatch.fnmatch(rel, f"**/{pat}"):
                        return Ruling(Decision.DENY, f"refusing secret-like path: {spath}")
            if path == home or home in path.parents:
                if tool_name in MUTATION_TOOLS or tool_name in ("read",):
                    name_rel = path.relative_to(home) if path != home else Path(".")
                    for pat in HOME_PROTECTED:
                        if fnmatch.fnmatch(str(name_rel), pat):
                            return Ruling(Decision.DENY,
                                          f"refusing: {spath} is a Marfim control file")
            if session_dir and (path == session_dir or session_dir in path.parents):
                return Ruling(Decision.DENY,
                              f"refusing: {spath} is inside the session store")
            # workspace containment: mutations must land inside the sandbox fence
            if tool_name in MUTATION_TOOLS:
                if not any(path == r or r in path.parents for r in roots):
                    return Ruling(Decision.DENY,
                                  f"refusing: {spath} is outside the workspace "
                                  f"(project root + writable_dirs); add it to "
                                  f"sandbox.writable_dirs if you really mean it")
        return None

    def _single_match(self, rule: str, tool_name: str, target: str) -> bool:
        """One rule against one target string (glob semantics)."""
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
        if tool_name not in MUTATION_TOOLS:
            return None
        if not self.config.protect_configs:
            return None
        for path in self._tool_paths(tool_name, args, project_root):
            try:
                rel = str(path.relative_to(project_root.resolve()))
            except ValueError:
                rel = None
            if rel is not None:
                for pattern in PROTECTED_PATTERNS:
                    if fnmatch.fnmatch(rel, pattern) or fnmatch.fnmatch(rel, f"**/{pattern}"):
                        return rel
            # absolute protection for the Marfim home (finding 9: the global
            # config is not under the project root)
            try:
                home_rel = str(path.relative_to(marfim_home().resolve()))
                for pattern in HOME_PROTECTED:
                    if fnmatch.fnmatch(home_rel, pattern):
                        return f"~/.marfim/{home_rel}"
            except ValueError:
                continue
        return None

    def _is_readonly(self, tool_name: str, args: dict) -> bool:
        if tool_name in READONLY_TOOLS or tool_name in self.readonly_tools:
            return True
        if tool_name == "bash":
            ok, _why = command_is_readonly(str(args.get("command", "")))
            return ok
        return False

    def grant_always(self, tool_name: str, args: dict) -> str | None:
        """Persist an 'always allow' grant for this invocation shape.

        Review finding 6: grants are EXACT commands, never prefix globs, and
        interpreters/shells/sudo/curl/git are never auto-granted at all.
        Returns the rule, or None when nothing may be persisted."""
        target = self._rule_target(tool_name, args)
        if tool_name == "bash" and target:
            rules: list[str] = []
            for seg in split_shell_segments(target):
                try:
                    parts = shlex.split(seg)
                except ValueError:
                    return None
                if not parts or parts[0] in NEVER_GRANT_BASH:
                    return None
                rule = f"Bash({seg})"
                if rule not in rules:
                    rules.append(rule)
            for rule in rules:
                if rule not in self.session_grants:
                    self.session_grants.append(rule)
            return " && ".join(rules) if len(rules) > 1 else rules[0]
        if tool_name in ("write", "edit", "patch", "multi_edit"):
            rule = f"{tool_name.capitalize()}(*)"
        else:
            rule = f"{tool_name}(*)"
        if rule not in self.session_grants:
            self.session_grants.append(rule)
        return rule


def _rel_or_none(path: Path, root: Path) -> str | None:
    try:
        return str(path.relative_to(Path(root).resolve()))
    except ValueError:
        return None
