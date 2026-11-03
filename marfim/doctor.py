"""Doctor — environment diagnostics (hermes/claude-code doctrine)."""

from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path

from marfim import __version__
from marfim.config import Config
from marfim.sandbox import sandbox_available


def run_doctor(config: Config) -> int:
    checks: list[tuple[str, str, str]] = []  # (status, name, detail)

    def ok(name, detail=""):
        checks.append(("ok", name, detail))

    def warn(name, detail=""):
        checks.append(("warn", name, detail))

    def fail(name, detail=""):
        checks.append(("fail", name, detail))

    version = f"{sys.version_info.major}.{sys.version_info.minor}.{sys.version_info.micro}"
    if sys.version_info >= (3, 11):
        ok("python", version)
    else:
        fail("python", f"{version} (need 3.11+)")

    for tool in ("git", "rg", "bash", "curl"):
        path = shutil.which(tool)
        (ok if path else warn)(tool, path or "not found (degraded)")

    from marfim.providers.registry import fetch_catalog, has_api_key
    catalog = fetch_catalog(timeout=8)
    if catalog:
        providers_n = len(catalog)
        ok("models.dev catalog", f"{providers_n} providers cached")
    else:
        warn("models.dev catalog", "unreachable — offline mode, presets only")

    from marfim.providers.registry import PRESETS
    for name in sorted(set(list(PRESETS) + list(config.providers))):
        if name in ("mock", "ollama", "lmstudio", "vllm"):
            continue
        if has_api_key(name):
            ok(f"key:{name}")
    configured = config.default_provider
    if configured not in ("mock",) and not has_api_key(configured):
        warn(f"key:{configured}", f"default provider has no key "
                                  f"({(PRESETS.get(configured) or {}).get('api_key_env', '?')})")

    if sandbox_available(config.sandbox_mode):
        ok("sandbox", config.sandbox_mode)
    elif config.sandbox_mode == "auto":
        warn("sandbox", "no backend found — commands will run UNSANDBOXED (auto mode)")
    elif config.sandbox_mode != "none":
        warn("sandbox", f"{config.sandbox_mode} requested but unavailable — commands "
                        f"will FAIL CLOSED (set sandbox_required=false to opt out explicitly)")

    if config.mcp_servers:
        from marfim.mcp.client import McpManager
        manager = McpManager()
        for name, srv in config.mcp_servers.items():
            if not srv.command:
                continue
            manager.register(name, srv.command, srv.args, srv.env, config.project_root)
        status = manager.start_all(config.project_root)
        for name, state in status.items():
            (ok if state.startswith("ok") else warn)(f"mcp:{name}", state)
        manager.stop_all()
    else:
        ok("mcp", "no servers configured")

    session_dir = Path(config.session_dir)
    try:
        session_dir.mkdir(parents=True, exist_ok=True)
        probe = session_dir / ".doctor-probe"
        probe.write_text("x", encoding="utf-8")
        probe.unlink()
        ok("session store", str(session_dir))
    except OSError as exc:
        fail("session store", str(exc))

    memory_files = list(config.project_root.glob("MARFIM.md")) + \
        list(config.project_root.glob("AGENTS.md")) + list(config.project_root.glob("CLAUDE.md"))
    if memory_files:
        ok("memory", ", ".join(p.name for p in memory_files))
    else:
        warn("memory", "no MARFIM.md/AGENTS.md/CLAUDE.md (run /init)")

    icon = {"ok": "✓", "warn": "·", "fail": "✗"}
    print(f"Marfim {__version__} doctor")
    for status, name, detail in checks:
        line = f" {icon[status]} {name}"
        if detail:
            line += f" — {detail}"
        print(line)
    fails = sum(1 for s, _, _ in checks if s == "fail")
    warns = sum(1 for s, _, _ in checks if s == "warn")
    print(f"\n{len(checks) - fails - warns} ok · {warns} warnings · {fails} failures")
    return 1 if fails else 0
