"""Layered configuration: defaults < global TOML < project TOML < env < CLI.

Design provenance (clean-room synthesis, see docs/FEATURE-MATRIX.md):
- Codex: `model_providers.<id>` blocks with base_url/env_key, profiles
- opencode v2: provider/model ids of the form provider/model, layered JSON config
- Claude Code: permissions.{allow,ask,deny} rules with tool-specifier patterns
- Oh My Pi: model roles (default/smol/plan/task/commit), MCP servers in config
- ECC: hook graph + protected-config doctrine

Files:
  global   ~/.marfim/config.toml   (fully trusted: it is the user's own)
  project  ./.marfim/config.toml   (UNTRUSTED until sha256-registered in
           ~/.marfim/trust.json — only its deny/ask rules are applied; see
           marfim/trust.py and review finding 2)

A malformed config is a hard error (review finding 11): safety settings must
never be silently dropped because a table failed to parse.
"""

from __future__ import annotations

import os
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

try:  # Python 3.11+
    import tomllib as _toml
except ImportError:  # pragma: no cover
    _toml = None


MARFIM_DIR = ".marfim"


class ConfigError(Exception):
    """A config file exists but cannot be parsed — refuse to run (finding 11)."""


def marfim_home() -> Path:
    return Path(os.environ.get("MARFIM_HOME", Path.home() / ".marfim"))


def find_project_root(start: Path | None = None) -> Path:
    """Walk up to the git root (Codex-style root-marker walk)."""
    cur = (start or Path.cwd()).resolve()
    for candidate in [cur, *cur.parents]:
        if (candidate / ".git").exists():
            return candidate
    return cur


def project_config_path(root: Path | None = None) -> Path:
    return (root or find_project_root()) / MARFIM_DIR / "config.toml"


def global_config_path() -> Path:
    return marfim_home() / "config.toml"


@dataclass
class ProviderConfig:
    name: str = ""
    base_url: str = ""
    wire: str = "openai"            # openai | anthropic | mock
    api_key_env: str = ""           # env var holding the key
    api_key: str = ""               # direct key (discouraged; prefer env)
    default_model: str = ""

    def key(self) -> str:
        if self.api_key:
            return self.api_key
        if self.api_key_env:
            return os.environ.get(self.api_key_env, "")
        return os.environ.get("MARFIM_API_KEY", "")


@dataclass
class McpServerConfig:
    name: str
    command: str = ""
    args: list = field(default_factory=list)
    env: dict = field(default_factory=dict)
    url: str = ""                   # streamable-HTTP/SSE transport when set
    headers: dict = field(default_factory=dict)   # auth headers for URL servers


@dataclass
class Config:
    # model selection
    default_provider: str = "openai"
    default_model: str = "gpt-4.1-mini"
    model_roles: dict = field(default_factory=lambda: {
        # Oh My Pi style roles; values are "provider/model[,fallback,...]"
        "plan": "", "commit": "", "task": "", "smol": "", "vision": "",
    })
    # per-role dollar budgets (router refuses a role whose spend exceeded it)
    role_budgets: dict = field(default_factory=dict)

    # providers: name -> ProviderConfig
    providers: dict = field(default_factory=dict)

    # permissions (Claude Code rule syntax: "Tool" or "Tool(pattern)")
    mode: str = "default"           # default | plan | accept-edits | bypass
    allow: list = field(default_factory=list)
    ask: list = field(default_factory=list)
    deny: list = field(default_factory=list)
    protect_configs: bool = True    # ECC config-protection doctrine
    # explicit opt-out for the sandbox hard-fail policy (review finding 10)
    sandbox_required: bool = True

    # sandbox (Codex doctrine)
    sandbox_mode: str = "none"      # none | bubblewrap | seatbelt | auto
    writable_dirs: list = field(default_factory=list)
    network: bool = True

    # agent behaviour
    max_turns: int = 200
    compact_threshold: float = 0.80  # fraction of context window
    retry_max: int = 3
    reflection_cap: int = 3          # Aider doctrine
    max_subagent_depth: int = 1
    max_parallel_tasks: int = 4      # bounded thread pool (review §4.4)
    bash_timeout: int = 120
    max_output_tokens: int = 16384   # provider request cap (finding 24)

    # verification (review §4.5): commands the `verify` tool runs in-sandbox
    verify_commands: list = field(default_factory=list)

    # external agent backends (review §4.3): name -> {command, args, prompt_via,
    # format} — opencode / codex / claude-code / hermes as delegate backends
    external_agents: dict = field(default_factory=dict)

    # mcp servers
    mcp_servers: dict = field(default_factory=dict)   # name -> McpServerConfig

    # hooks: event -> list of shell commands (Claude Code event names)
    hooks: dict = field(default_factory=lambda: {
        "SessionStart": [], "PreToolUse": [], "PostToolUse": [],
        "PreCompact": [], "Stop": [],
    })

    # paths
    session_dir: str = ""           # default: ~/.marfim/sessions
    skills_dirs: list = field(default_factory=list)
    commands_dirs: list = field(default_factory=list)
    plugins_dirs: list = field(default_factory=list)

    # ui
    theme: str = "dark"
    verbose: bool = False

    # resolved at load
    project_root: Path = field(default_factory=Path.cwd)
    project_config_trusted: bool = True   # False → tightening-only merge happened
    untrusted_notice: str = ""

    # ------------------------------------------------------------------ load

    @staticmethod
    def load(cli_overrides: dict | None = None, project_root: Path | None = None) -> "Config":
        cfg = Config()
        cfg.project_root = (project_root or find_project_root()).resolve()
        # 1. global config — the user's own file, fully trusted
        gpath = global_config_path()
        if gpath.is_file():
            cfg._merge_toml(gpath, trusted=True)
        # 2. project config — untrusted until registered (review finding 2)
        ppath = project_config_path(cfg.project_root)
        if ppath.is_file():
            from marfim.trust import TrustStore, config_digest, untrusted_notice
            digest = config_digest(ppath)
            store = TrustStore()
            trusted = store.is_trusted(cfg.project_root, digest)
            cfg.project_config_trusted = trusted
            cfg._merge_toml(ppath, trusted=trusted)
            if not trusted:
                cfg.untrusted_notice = untrusted_notice(cfg.project_root, ppath.name)
        cfg._merge_env()
        if cli_overrides:
            cfg._merge_cli(cli_overrides)
        cfg._apply_defaults()
        return cfg

    # -------------------------------------------------------------- merging

    def _merge_toml(self, path: Path, trusted: bool = True) -> None:
        if _toml is None:
            return
        try:
            data = _toml.loads(path.read_text(encoding="utf-8"))
        except tomllib.TOMLDecodeError as exc:
            # finding 11: never silently drop the safety settings
            raise ConfigError(f"{path}: invalid TOML — refusing to run with "
                              f"unparsed safety config ({exc})") from exc
        except OSError as exc:
            raise ConfigError(f"{path}: cannot read config ({exc})") from exc
        self._merge_dict(data, trusted)

    def _merge_dict(self, data: dict, trusted: bool = True) -> None:
        """Merge one parsed config layer.  When `trusted` is False only the
        tightening subset is honoured (review finding 2)."""
        simple = {
            "default_provider": "default_provider", "default_model": "default_model",
            "mode": "mode", "sandbox_mode": "sandbox_mode", "max_turns": "max_turns",
            "compact_threshold": "compact_threshold", "retry_max": "retry_max",
            "bash_timeout": "bash_timeout", "session_dir": "session_dir",
            "theme": "theme", "verbose": "verbose", "max_subagent_depth": "max_subagent_depth",
            "max_parallel_tasks": "max_parallel_tasks", "max_output_tokens": "max_output_tokens",
            "protect_configs": "protect_configs", "network": "network",
            "sandbox_required": "sandbox_required",
        }
        if trusted:
            for key, attr in simple.items():
                if key in data:
                    setattr(self, attr, data[key])
            # permissions.mode may widen; the rest of permissions is merged below
        if isinstance(data.get("model_roles"), dict):
            if trusted:
                self.model_roles.update({k: v for k, v in data["model_roles"].items() if v})
        if isinstance(data.get("role_budgets"), dict) and trusted:
            self.role_budgets.update({k: float(v) for k, v in data["role_budgets"].items()
                                      if isinstance(v, (int, float))})
        if isinstance(data.get("permissions"), dict):
            perms = data["permissions"]
            # allow/ask/deny from any layer: allow may widen only when trusted
            if trusted:
                self.allow += [str(x) for x in perms.get("allow", [])]
            self.ask += [str(x) for x in perms.get("ask", [])]
            self.deny += [str(x) for x in perms.get("deny", [])]
            if trusted and "mode" in perms:
                self.mode = perms["mode"]
        if isinstance(data.get("sandbox"), dict):
            sb = data["sandbox"]
            if trusted:
                self.sandbox_mode = sb.get("mode", self.sandbox_mode)
                self.writable_dirs += [str(x) for x in sb.get("writable_dirs", [])]
                if "network" in sb:
                    self.network = bool(sb["network"])
                if "required" in sb:
                    self.sandbox_required = bool(sb["required"])
        if isinstance(data.get("agent"), dict) and trusted:
            ag = data["agent"]
            for key in ("max_turns", "compact_threshold", "retry_max", "reflection_cap",
                        "max_subagent_depth", "bash_timeout", "max_parallel_tasks",
                        "max_output_tokens"):
                if key in ag:
                    setattr(self, key, ag[key])
        if isinstance(data.get("verify"), dict) and trusted:
            vc = data["verify"].get("commands")
            if isinstance(vc, list):
                self.verify_commands += [str(c) for c in vc]
        if isinstance(data.get("external_agents"), dict) and trusted:
            for name, spec in data["external_agents"].items():
                if isinstance(spec, dict):
                    self.external_agents[str(name)] = {
                        "command": str(spec.get("command", "")),
                        "args": [str(a) for a in spec.get("args", [])],
                        "prompt_via": str(spec.get("prompt_via", "arg")),
                        "format": str(spec.get("format", "text")),
                    }
        if trusted:
            for name, prov in (data.get("providers") or {}).items():
                if not isinstance(prov, dict):
                    continue
                existing = self.providers.get(name) or ProviderConfig(name=name)
                existing.name = name
                existing.base_url = prov.get("base_url", existing.base_url)
                existing.wire = prov.get("wire", existing.wire)
                existing.api_key_env = prov.get("api_key_env", existing.api_key_env)
                existing.api_key = prov.get("api_key", existing.api_key)
                existing.default_model = prov.get("default_model", existing.default_model)
                self.providers[name] = existing
            for name, srv in (data.get("mcp_servers") or {}).items():
                if not isinstance(srv, dict):
                    continue
                self.mcp_servers[name] = McpServerConfig(
                    name=name, command=srv.get("command", ""), args=list(srv.get("args", [])),
                    env=dict(srv.get("env", {})), url=srv.get("url", ""),
                    headers=dict(srv.get("headers", {})))
            if isinstance(data.get("hooks"), dict):
                for event, cmds in data["hooks"].items():
                    if event in self.hooks and isinstance(cmds, list):
                        self.hooks[event] += [str(c) for c in cmds]
            for key in ("skills_dirs", "commands_dirs", "plugins_dirs", "writable_dirs"):
                if isinstance(data.get(key), list):
                    setattr(self, key, getattr(self, key) + [str(x) for x in data[key]])

    def _merge_env(self) -> None:
        mapping = {
            "MARFIM_PROVIDER": "default_provider", "MARFIM_MODEL": "default_model",
            "MARFIM_MODE": "mode", "MARFIM_SANDBOX": "sandbox_mode",
        }
        for env_key, attr in mapping.items():
            val = os.environ.get(env_key)
            if val:
                setattr(self, attr, val)

    def _merge_cli(self, overrides: dict) -> None:
        for key, value in overrides.items():
            if value is None:
                continue
            if key == "provider":
                self.default_provider = value
            elif key == "model":
                self.default_model = value
            elif key == "mode":
                self.mode = value
            elif key == "approve_all":
                # finding 1: False must not flip bypass on every invocation
                if value:
                    self.mode = "bypass"
            elif key == "max_turns":
                self.max_turns = int(value)
            elif key == "verbose":
                self.verbose = bool(value)

    def _apply_defaults(self) -> None:
        from marfim.providers.registry import get_provider_preset
        if self.default_provider not in self.providers:
            preset = get_provider_preset(self.default_provider)
            if preset:
                self.providers[self.default_provider] = ProviderConfig(**preset)
        # skill/command/plugin dirs.  Project-relative dirs (.marfim/*,
        # .claude/*) are only honoured for TRUSTED projects (finding 2);
        # the home dirs are always fine — they are the user's own.
        home = marfim_home()
        root = self.project_root
        self.skills_dirs += [str(home / "skills")]
        self.commands_dirs += [str(home / "commands")]
        self.plugins_dirs += [str(home / "plugins")]
        if self.project_config_trusted:
            self.skills_dirs += [str(root / MARFIM_DIR / "skills"), str(root / ".claude" / "skills")]
            self.commands_dirs += [str(root / MARFIM_DIR / "commands"), str(root / ".claude" / "commands")]
            self.plugins_dirs += [str(root / MARFIM_DIR / "plugins")]
        if not self.session_dir:
            self.session_dir = str(home / "sessions")

    # -------------------------------------------------------------- helpers

    def provider(self, name: str | None = None) -> ProviderConfig:
        name = name or self.default_provider
        prov = self.providers.get(name)
        if prov is None:
            from marfim.providers.registry import get_provider_preset
            preset = get_provider_preset(name)
            prov = ProviderConfig(**preset) if preset else ProviderConfig(name=name, wire="openai")
            self.providers[name] = prov
        return prov

    def ensure_dirs(self) -> None:
        Path(self.session_dir).mkdir(parents=True, exist_ok=True, mode=0o700)
        marfim_home().mkdir(parents=True, exist_ok=True, mode=0o700)
        (marfim_home() / "cache").mkdir(parents=True, exist_ok=True, mode=0o700)

    def effective_dirs(self) -> list[Path]:
        return [self.project_root] + [Path(d).expanduser() for d in self.writable_dirs]
