"""Layered configuration: defaults < global TOML < project TOML < env < CLI.

Design provenance (clean-room synthesis, see docs/FEATURE-MATRIX.md):
- Codex: `model_providers.<id>` blocks with base_url/env_key, profiles
- opencode v2: provider/model ids of the form provider/model, layered JSON config
- Claude Code: permissions.{allow,ask,deny} rules with tool-specifier patterns
- Oh My Pi: model roles (default/smol/plan/task/commit), MCP servers in config
- ECC: hook graph + protected-config doctrine

Files:
  global   ~/.marfim/config.toml
  project  ./.marfim/config.toml   (walks up to git root)
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
    wire: str = "openai"            # openai | anthropic
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
    url: str = ""                   # future: HTTP transport


@dataclass
class Config:
    # model selection
    default_provider: str = "openai"
    default_model: str = "gpt-4.1-mini"
    model_roles: dict = field(default_factory=lambda: {
        # Oh My Pi style roles; resolve to provider/model at runtime
        "plan": "", "commit": "", "task": "", "smol": "", "vision": "",
    })

    # providers: name -> ProviderConfig
    providers: dict = field(default_factory=dict)

    # permissions (Claude Code rule syntax: "Tool" or "Tool(pattern)")
    mode: str = "default"           # default | plan | accept-edits | bypass
    allow: list = field(default_factory=list)
    ask: list = field(default_factory=list)
    deny: list = field(default_factory=list)
    protect_configs: bool = True    # ECC config-protection doctrine

    # sandbox (Codex doctrine)
    sandbox_mode: str = "none"      # none | bubblewrap | seatbelt(auto on mac)
    writable_dirs: list = field(default_factory=list)
    network: bool = True

    # agent behaviour
    max_turns: int = 200
    compact_threshold: float = 0.80  # fraction of context window
    retry_max: int = 3
    reflection_cap: int = 3          # Aider doctrine
    max_subagent_depth: int = 1
    bash_timeout: int = 120

    # mcp servers
    mcp_servers: dict = field(default_factory=dict)   # name -> {command,args,env,url}

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

    # ------------------------------------------------------------------ load

    @staticmethod
    def load(cli_overrides: dict | None = None, project_root: Path | None = None) -> "Config":
        cfg = Config()
        cfg.project_root = (project_root or find_project_root()).resolve()
        for path in (global_config_path(), project_config_path(cfg.project_root)):
            if path.is_file():
                cfg._merge_toml(path)
        cfg._merge_env()
        if cli_overrides:
            cfg._merge_cli(cli_overrides)
        cfg._apply_defaults()
        return cfg

    # -------------------------------------------------------------- merging

    def _merge_toml(self, path: Path) -> None:
        if _toml is None:
            return
        try:
            data = _toml.loads(path.read_text(encoding="utf-8"))
        except (tomllib.TOMLDecodeError, OSError):
            return
        simple = {
            "default_provider": "default_provider", "default_model": "default_model",
            "mode": "mode", "sandbox_mode": "sandbox_mode", "max_turns": "max_turns",
            "compact_threshold": "compact_threshold", "retry_max": "retry_max",
            "bash_timeout": "bash_timeout", "session_dir": "session_dir",
            "theme": "theme", "verbose": "verbose", "max_subagent_depth": "max_subagent_depth",
            "protect_configs": "protect_configs", "network": "network",
        }
        for key, attr in simple.items():
            if key in data:
                setattr(self, attr, data[key])
        if isinstance(data.get("model_roles"), dict):
            self.model_roles.update({k: v for k, v in data["model_roles"].items() if v})
        if isinstance(data.get("permissions"), dict):
            perms = data["permissions"]
            self.allow += [str(x) for x in perms.get("allow", [])]
            self.ask += [str(x) for x in perms.get("ask", [])]
            self.deny += [str(x) for x in perms.get("deny", [])]
            if "mode" in perms:
                self.mode = perms["mode"]
        if isinstance(data.get("sandbox"), dict):
            sb = data["sandbox"]
            self.sandbox_mode = sb.get("mode", self.sandbox_mode)
            self.writable_dirs += [str(x) for x in sb.get("writable_dirs", [])]
            if "network" in sb:
                self.network = bool(sb["network"])
        if isinstance(data.get("agent"), dict):
            ag = data["agent"]
            for key in ("max_turns", "compact_threshold", "retry_max", "reflection_cap",
                        "max_subagent_depth", "bash_timeout"):
                if key in ag:
                    setattr(self, key, ag[key])
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
                env=dict(srv.get("env", {})), url=srv.get("url", ""))
        if isinstance(data.get("hooks"), dict):
            for event, cmds in data["hooks"].items():
                if event in self.hooks and isinstance(cmds, list):
                    self.hooks[event] += [str(c) for c in cmds]
        for key, attr in (("skills_dirs", "skills_dirs"), ("commands_dirs", "commands_dirs"),
                          ("plugins_dirs", "plugins_dirs"), ("writable_dirs", "writable_dirs")):
            if isinstance(data.get(key), list):
                setattr(self, attr, getattr(self, attr) + [str(x) for x in data[key]])

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
        # sensible skill/command/plugin dirs (Oh My Pi + Claude Code compatibility)
        home = marfim_home()
        root = self.project_root
        self.skills_dirs += [
            str(home / "skills"), str(root / MARFIM_DIR / "skills"), str(root / ".claude" / "skills"),
        ]
        self.commands_dirs += [
            str(home / "commands"), str(root / MARFIM_DIR / "commands"), str(root / ".claude" / "commands"),
        ]
        self.plugins_dirs += [str(home / "plugins"), str(root / MARFIM_DIR / "plugins")]
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
        Path(self.session_dir).mkdir(parents=True, exist_ok=True)
        marfim_home().mkdir(parents=True, exist_ok=True)
        (marfim_home() / "cache").mkdir(parents=True, exist_ok=True)

    def effective_dirs(self) -> list[Path]:
        return [self.project_root] + [Path(d).expanduser() for d in self.writable_dirs]
