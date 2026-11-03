"""Marfim CLI — the single entry point.

Surfaces (Claude Code/omp doctrine): interactive REPL by default, headless
`run -p`, session management, model catalog, MCP config, doctor.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

from marfim import __version__


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="marfim",
        description="Marfim — unified coding-agent harness (opencode v2 · codex · goose · "
                    "aider · claude code · hermes · oh-my-pi · dsh · REA · ECC disciplines)")
    parser.add_argument("--version", action="version", version=f"marfim {__version__}")
    parser.add_argument("-C", "--cwd", help="run in this directory")
    parser.add_argument("-p", "--prompt", help="headless: run one prompt and exit")
    parser.add_argument("-y", "--approve-all", action="store_true",
                        help="approve all tool calls (bypass mode)")
    parser.add_argument("--mode", choices=["default", "plan", "accept-edits", "bypass"])
    parser.add_argument("--provider", help="provider name")
    parser.add_argument("--model", help="model id (provider/model or model)")
    parser.add_argument("--max-turns", type=int)
    parser.add_argument("--output-format", choices=["text", "json", "stream-json"],
                        default="text")
    parser.add_argument("--resume", help="resume a session (id, prefix, or 'last')")
    parser.add_argument("--session-name", help="title for a new session")
    parser.add_argument("--verbose", action="store_true")
    sub = parser.add_subparsers(dest="command")

    # session
    sp = sub.add_parser("session", help="session management")
    ssp = sp.add_subparsers(dest="session_cmd", required=True)
    ssp.add_parser("list")
    show = ssp.add_parser("show"); show.add_argument("ref")
    exp = ssp.add_parser("export"); exp.add_argument("ref"); exp.add_argument("path")
    rm = ssp.add_parser("rm"); rm.add_argument("ref")
    fork = ssp.add_parser("fork"); fork.add_argument("ref")

    # trust (review finding 2)
    tp = sub.add_parser("trust", help="project-config trust gate")
    tsp = tp.add_subparsers(dest="trust_cmd", required=True)
    tsp.add_parser("add", help="trust the current project's .marfim/config.toml")
    tsp.add_parser("list", help="list trusted projects")
    tsp.add_parser("revoke", help="revoke trust for the current project")

    # models
    mp = sub.add_parser("models", help="model catalog (models.dev)")
    msp = mp.add_subparsers(dest="models_cmd")
    msp.add_parser("list")
    msp.add_parser("free")
    msp.add_parser("sync")

    # provider
    pp = sub.add_parser("provider", help="provider info")
    psp = pp.add_subparsers(dest="provider_cmd")
    psp.add_parser("list")

    # mcp
    cp = sub.add_parser("mcp", help="MCP server management")
    csp = cp.add_subparsers(dest="mcp_cmd", required=True)
    csp.add_parser("list")
    add = csp.add_parser("add")
    add.add_argument("name")
    # finding 30: the positional must NOT overwrite ns.command (which carries
    # the subcommand name) — a REPL used to launch instead of adding
    add.add_argument("mcp_command")
    add.add_argument("mcp_args", nargs="*")
    add.add_argument("--url", default="", help="streamable-HTTP server URL instead of a command")
    test = csp.add_parser("test"); test.add_argument("name")

    # config / skills / tools / doctor / usage
    cfgp = sub.add_parser("config", help="config paths and values")
    cfgp.add_argument("what", nargs="?", choices=["path", "show"], default="path")
    sub.add_parser("skills", help="list installed skills")
    sub.add_parser("tools", help="list built-in tools")
    sub.add_parser("doctor", help="environment checks")
    sub.add_parser("usage", help="explain usage accounting")

    # map
    mapp = sub.add_parser("map", help="print the repo map")
    mapp.add_argument("focus", nargs="*")

    # chat (explicit interactive)
    sub.add_parser("chat", help="interactive REPL (same as default)")
    return parser


def main(argv: list[str] | None = None) -> int:
    try:
        return _main(argv)
    except BrokenPipeError:
        # graceful exit when the consumer closed the pipe (e.g. `marfim models free | head`)
        import os
        try:
            sys.stdout.close()
        except Exception:
            pass
        os._exit(0)


def _main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    ns = parser.parse_args(argv)
    if getattr(ns, "cwd", None):
        import os
        os.chdir(ns.cwd)

    from marfim.config import Config, ConfigError
    overrides = {"provider": ns.provider, "model": ns.model, "mode": ns.mode,
                 "approve_all": ns.approve_all, "max_turns": ns.max_turns,
                 "verbose": ns.verbose}
    try:
        config = Config.load(overrides)
    except ConfigError as exc:
        # finding 11: a broken config is a LOUD refusal, never a silent
        # run-with-default-safety
        print(f"✗ refusing to start: {exc}", file=sys.stderr)
        return 2
    if config.untrusted_notice and getattr(ns, "command", None) != "trust":
        print(f"warning: {config.untrusted_notice}", file=sys.stderr)

    if ns.command == "trust":
        return _cmd_trust(config, ns)
    if ns.command == "session":
        return _cmd_session(config, ns)
    if ns.command == "models":
        return _cmd_models(config, ns)
    if ns.command == "provider":
        return _cmd_provider(config)
    if ns.command == "mcp":
        return _cmd_mcp(config, ns)
    if ns.command == "config":
        return _cmd_config(config)
    if ns.command == "skills":
        from marfim.memory import discover_skills
        for skill in discover_skills(config):
            print(f"• {skill.name}: {skill.description[:100]}")
        return 0
    if ns.command == "tools":
        from marfim.tools.registry import default_tools
        for tool in default_tools():
            ro = " ro" if tool.readonly else "   "
            print(f" {ro} {tool.name:<24} {tool.description[:90]}")
        return 0
    if ns.command == "doctor":
        from marfim.doctor import run_doctor
        return run_doctor(config)
    if ns.command == "usage":
        print("usage is tracked per session in the JSONL store (records of type 'usage').")
        print("View with: marfim session show <ref>  or  /usage inside the REPL.")
        return 0
    if ns.command == "map":
        from marfim.tools.repomap import build_map
        print(build_map(config.project_root, ns.focus or []))
        return 0

    # interactive or headless
    if ns.prompt:
        from marfim.ui.headless import run_headless
        return run_headless(config, ns.prompt, output_format=ns.output_format,
                            resume=ns.resume, max_turns=ns.max_turns,
                            session_name=ns.session_name or "")

    from marfim.session.store import Session, find_session
    resume_session = None
    if ns.resume:
        resume_session = find_session(config.session_dir, ns.resume)
        if resume_session is None:
            print(f"session '{ns.resume}' not found", file=sys.stderr)
            return 1
    from marfim.ui.repl import Repl
    repl = Repl(config, resume_session=resume_session,
                session_name=ns.session_name or "")
    try:
        repl.loop()
    except KeyboardInterrupt:
        print()
    return 0


# ------------------------------------------------------------------ subcommands

def _cmd_trust(config, ns) -> int:
    """Trust gate CLI (review finding 2)."""
    from marfim.config import project_config_path
    from marfim.trust import TrustStore, config_digest
    store = TrustStore()
    cmd = ns.trust_cmd
    if cmd == "list":
        entries = store.list_trusted()
        if not entries:
            print("(no trusted projects)")
            return 0
        for path, meta in sorted(entries.items()):
            print(f"• {path}  digest={meta.get('digest', '')[:12]}…  "
                  f"trusted_at={meta.get('trusted_at', '?')}")
        return 0
    if cmd == "add":
        ppath = project_config_path(config.project_root)
        if not ppath.is_file():
            print(f"no project config at {ppath} — nothing to trust")
            return 1
        print(f"about to TRUST {ppath} for project {config.project_root}")
        print("This allows the file to set mode, hooks, MCP servers, providers, "
              "sandbox and skills dirs. Review it first:")
        print("---")
        print(ppath.read_text(encoding="utf-8", errors="replace")[:4000])
        print("---")
        try:
            answer = input("trust this config? [y/N]: ").strip().lower()
        except (EOFError, KeyboardInterrupt):
            answer = "n"
        if answer not in ("y", "yes"):
            print("not trusted")
            return 1
        store.trust(config.project_root, config_digest(ppath))
        print(f"trusted {config.project_root}")
        return 0
    if cmd == "revoke":
        if store.revoke(config.project_root):
            print(f"trust revoked for {config.project_root}")
            return 0
        print("project was not trusted")
        return 1
    return 0


def _cmd_session(config, ns) -> int:
    from marfim.session.store import find_session, list_sessions
    cmd = ns.session_cmd
    if cmd == "list":
        sessions = list_sessions(config.session_dir, str(config.project_root))
        if not sessions:
            print("(no sessions)")
            return 0
        for s in sessions:
            print(f"{s.sid}  {s.title[:60]:<60} {len(s.messages):>4} msgs")
        return 0
    target = find_session(config.session_dir, getattr(ns, "ref", "last"))
    if target is None:
        print("session not found")
        return 1
    if cmd == "show":
        for msg in target.messages:
            role = msg.get("role", "?")
            content = msg.get("content")
            if isinstance(content, list):
                content = str(content)
            tc = msg.get("tool_calls")
            line = (content or "")[:2000] if isinstance(content, str) else ""
            if tc:
                print(f"[{role}] tools: {', '.join(t['name'] for t in tc)}")
            else:
                print(f"[{role}] {line}")
        return 0
    if cmd == "export":
        Path(ns.path).write_text(target.export_markdown(), encoding="utf-8")
        print(f"exported → {ns.path}")
        return 0
    if cmd == "fork":
        forked = target.fork(config.session_dir)
        print(f"forked → {forked.sid}")
        return 0
    if cmd == "rm":
        target.path.unlink(missing_ok=True)
        # finding 31: session rm used to leave the .snaps directory behind
        import shutil as _shutil
        snaps = target.path.parent / f"{target.sid}.snaps"
        if snaps.is_dir():
            _shutil.rmtree(snaps, ignore_errors=True)
        print(f"removed {target.sid}")
        return 0
    return 0


def _cmd_models(config, ns) -> int:
    from marfim.providers.registry import fetch_catalog, free_models, list_catalog_models
    cmd = getattr(ns, "models_cmd", None) or "list"
    if cmd == "sync":
        catalog = fetch_catalog(force=True)
        if catalog:
            total = sum(len(p.get("models", {})) for p in catalog.values())
            print(f"synced models.dev catalog: {len(catalog)} providers, {total} models")
            return 0
        print("sync failed (offline?)")
        return 1
    if cmd == "free":
        models = free_models(50)
        for m in models:
            ctx = f" · {m.context_window // 1000}k" if m.context_window else ""
            print(f"{m.provider}/{m.model}  {m.name[:40]}{ctx}")
        return 0
    provider = ns.provider or config.default_provider
    models = list_catalog_models(provider)
    if not models:
        print(f"(no catalog entries for {provider} — run: marfim models sync)")
        for name in sorted(config.providers):
            print(f"configured: {name}")
        return 0
    for m in models:
        label = m.price_label()
        print(f"{m.provider}/{m.model}  [{label}]  ctx={m.context_window or '?'}")
    return 0


def _cmd_provider(config) -> int:
    from marfim.providers.registry import PRESETS, has_api_key
    for name in sorted(set(list(PRESETS) + list(config.providers))):
        prov = config.provider(name)
        mark = "✓" if has_api_key(name) else "·"
        print(f" {mark} {name:<12} {prov.base_url or '-':<55} "
              f"key={'yes' if has_api_key(name) else 'no'}")
    return 0


def _cmd_mcp(config, ns) -> int:
    from marfim.mcp.client import McpManager
    cmd = ns.mcp_cmd
    if cmd == "list":
        if not config.mcp_servers:
            print("(no MCP servers configured)")
            return 0
        for name, srv in config.mcp_servers.items():
            target = srv.url or (srv.command and f"{srv.command} {' '.join(srv.args)}") or "-"
            print(f"• {name}: {target}")
        return 0
    if cmd == "add":
        from marfim.util import toml_quote
        path = config.project_root / ".marfim" / "config.toml"
        path.parent.mkdir(parents=True, exist_ok=True)
        existing = path.read_text(encoding="utf-8") if path.exists() else ""
        # duplicate table would invalidate the file → finding 11 hard error.
        # refuse and point at the file instead.
        if f"[mcp_servers.{ns.name}]" in existing:
            print(f"error: server '{ns.name}' already exists in {path}; edit it there")
            return 1
        if not ns.url and not ns.mcp_command:
            print("error: pass a COMMAND or --url")
            return 1
        block = (f"\n[mcp_servers.{ns.name}]\n"
                 f"command = {toml_quote(ns.mcp_command or '')}\n"
                 f"args = {toml_quote(list(ns.mcp_args))}\n"
                 if not ns.url else
                 f"\n[mcp_servers.{ns.name}]\n"
                 f"url = {toml_quote(ns.url)}\n")
        path.write_text(existing + block, encoding="utf-8")
        # the USER made this edit via the CLI: register the new digest so the
        # gate does not immediately flag it (finding 2 flow)
        from marfim.trust import TrustStore, config_digest
        TrustStore().trust(config.project_root, config_digest(path))
        print(f"added mcp server '{ns.name}' to {path} (project config trusted)")
        return 0
    if cmd == "test":
        srv = config.mcp_servers.get(ns.name)
        if srv is None or (not srv.command and not srv.url):
            print(f"server '{ns.name}' not configured")
            return 1
        manager = McpManager()
        manager.register(ns.name, srv.command, srv.args, srv.env, config.project_root,
                         url=srv.url, headers=srv.headers)
        status = manager.start_all(config.project_root)
        print(status.get(ns.name, "?"))
        manager.stop_all()
        return 0
    return 0


def _cmd_config(config) -> int:
    from marfim.config import global_config_path, project_config_path
    print(f"global:  {global_config_path()}")
    print(f"project: {project_config_path(config.project_root)}")
    print(f"provider={config.default_provider} model={config.default_model} "
          f"mode={config.mode} sandbox={config.sandbox_mode} "
          f"protect_configs={config.protect_configs}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
