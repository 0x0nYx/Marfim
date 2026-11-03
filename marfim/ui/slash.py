"""Slash commands — the REPL command surface (~30 commands).

Merged surface: Claude Code (/model /compact /rewind /permissions /mcp /usage),
aider (/run /add /map /undo), opencode (/share /stats), hermes (/doctor /learn),
Oh My Pi (/settings /sessions /skills).
"""

from __future__ import annotations

import json
import shlex
import subprocess
from pathlib import Path

from marfim import __version__
from marfim.permissions import Mode
from marfim.util import estimate_tokens, truncate_tail


class SlashContext:
    """Bundle the REPL needs so commands can act on live state."""

    def __init__(self, repl):
        self.repl = repl

    @property
    def agent(self):
        return self.repl.agent

    @property
    def config(self):
        return self.repl.config

    @property
    def session(self):
        return self.repl.session


COMMANDS: dict[str, dict] = {}


def command(name: str, help_text: str):
    def deco(fn):
        COMMANDS[name] = {"fn": fn, "help": help_text}
        return fn
    return deco


def _out(msg: str) -> None:
    print(msg)


# --------------------------------------------------------------------- model

@command("model", "show or set the model: /model [provider/model]")
def cmd_model(sc: SlashContext, args: str) -> None:
    if not args.strip():
        a = sc.agent
        _out(f"provider: {a.provider.config.name}  model: {a.provider.model}")
        return
    spec = args.strip()
    provider_name, _, model = spec.partition("/")
    if not model:
        model = provider_name
        provider_name = sc.config.default_provider
    from marfim.providers.base import build_provider
    sc.agent.provider = build_provider(sc.config.provider(provider_name), model)
    sc.config.default_provider = provider_name
    sc.config.default_model = model
    _out(f"switched to {provider_name}/{model}")


@command("provider", "list providers and key status")
def cmd_provider(sc: SlashContext, args: str) -> None:
    from marfim.providers.registry import PRESETS, has_api_key
    for name in sorted(set(list(PRESETS) + list(sc.config.providers))):
        mark = "✓" if has_api_key(name) else "·"
        preset = sc.config.provider(name)
        _out(f" {mark} {name:<12} {preset.base_url or '-'}")


@command("permissions", "show/set permission mode: /permissions [default|plan|accept-edits|bypass]")
def cmd_permissions(sc: SlashContext, args: str) -> None:
    if not args.strip():
        p = sc.agent.permissions
        _out(f"mode: {p.mode.value}")
        _out(f"allow rules: {sc.config.allow + p.session_grants}")
        _out(f"ask rules: {sc.config.ask}")
        _out(f"deny rules: {sc.config.deny}")
        return
    try:
        mode = Mode(args.strip().lower().replace("_", "-"))
    except ValueError:
        _out(f"unknown mode; use one of: {', '.join(m.value for m in Mode)}")
        return
    sc.agent.permissions.mode = mode
    sc.config.mode = mode.value
    _out(f"permission mode → {mode.value}")


# ------------------------------------------------------------------ sessions

@command("sessions", "list saved sessions")
def cmd_sessions(sc: SlashContext, args: str) -> None:
    from marfim.session.store import list_sessions
    sessions = list_sessions(sc.config.session_dir, limit=10)
    if not sessions:
        _out("(no sessions yet)")
        return
    for s in sessions:
        mark = "*" if s.sid == sc.session.sid else " "
        _out(f" {mark} {s.sid}  {truncate_tail(s.title or '(untitled)', 60)}")


@command("resume", "resume a session: /resume <id|last>")
def cmd_resume(sc: SlashContext, args: str) -> None:
    from marfim.session.store import find_session
    ref = args.strip() or "last"
    target = find_session(sc.config.session_dir, ref)
    if target is None:
        _out(f"session '{ref}' not found")
        return
    if target.sid == sc.session.sid:
        _out("already in that session")
        return
    sc.repl.adopt_session(target)
    _out(f"resumed {target.sid} ({len(target.messages)} messages)")


@command("fork", "fork the current session and continue there")
def cmd_fork(sc: SlashContext, args: str) -> None:
    new = sc.session.fork(sc.config.session_dir)
    sc.repl.adopt_session(new)
    _out(f"forked → {new.sid}")


@command("export", "export the session as markdown: /export [path]")
def cmd_export(sc: SlashContext, args: str) -> None:
    target = args.strip() or f"session-{sc.session.sid}.md"
    Path(target).write_text(sc.session.export_markdown(), encoding="utf-8")
    _out(f"exported → {target}")


@command("rewind", "restore files from the last checkpoint: /rewind [files=1]")
def cmd_rewind(sc: SlashContext, args: str) -> None:
    steps = 1
    if args.strip().isdigit():
        steps = int(args.strip())
    restored = sc.session.rewind(sc.config.project_root, steps)
    if restored:
        _out("restored: " + ", ".join(restored))
    else:
        _out("(no snapshots to restore)")


# ------------------------------------------------------------ context control

@command("compact", "compact the conversation now")
def cmd_compact(sc: SlashContext, args: str) -> None:
    from marfim.agent.compaction import compact_messages
    messages = list(sc.session.messages)

    def _complete(msgs):
        text = ""
        for event in sc.agent.provider.stream(msgs, [], system="You compress conversations."):
            if event.get("type") == "text_delta":
                text += event.get("text", "")
        return text or "(empty summary)"

    before = len(messages)
    new_messages, summary = compact_messages(messages, _complete)
    sc.session.record("summary", {"summary": summary})
    sc.session.messages = new_messages
    sc.session.record("truncate", {"keep": len(new_messages)})
    _out(f"compacted {before} → {len(new_messages)} messages "
         f"(summary {len(summary)} chars)")


@command("clear", "start a fresh conversation (session history cleared)")
def cmd_clear(sc: SlashContext, args: str) -> None:
    sc.session.messages = sc.session.messages[:0]
    sc.session.record("truncate", {"keep": 0})
    _out("conversation cleared (same session id; history reset)")


@command("context", "show context usage estimate")
def cmd_context(sc: SlashContext, args: str) -> None:
    from marfim.agent.compaction import context_window_for
    tokens = sum(estimate_tokens(str(m.get("content", ""))) +
                 estimate_tokens(str(m.get("tool_calls", "")))
                 for m in sc.session.messages)
    window = context_window_for(sc.agent.provider.model)
    from marfim.ui.render import render_usage_bar
    _out(render_usage_bar(tokens, window))


# ------------------------------------------------------------------- inspection

@command("memory", "show loaded memory files")
def cmd_memory(sc: SlashContext, args: str) -> None:
    sections = sc.agent.memory
    if not sections:
        _out("(no memory files found — run /init to scaffold MARFIM.md)")
        return
    for s in sections:
        _out(f"• {s.path} ({len(s.content)} bytes)")


@command("init", "scaffold a MARFIM.md memory file for this project")
def cmd_init(sc: SlashContext, args: str) -> None:
    target = sc.config.project_root / "MARFIM.md"
    if target.exists():
        _out("MARFIM.md already exists")
        return
    template = (f"# {sc.config.project_root.name}\n\n"
                "## Project notes for Marfim\n\n- Describe build/test commands here\n"
                "- Describe conventions the agent must follow\n")
    target.write_text(template, encoding="utf-8")
    _out(f"created {target}")


@command("skills", "list skills or load one: /skills [name]")
def cmd_skills(sc: SlashContext, args: str) -> None:
    if not args.strip():
        for skill in sc.agent.skills:
            _out(f"• {skill.name}: {truncate_tail(skill.description, 100)}")
        if not sc.agent.skills:
            _out("(no skills — add .marfim/skills/<name>/SKILL.md)")
        return
    result = next((s for s in sc.agent.skills if s.name == args.strip()), None)
    if result is None:
        _out(f"skill not found: {args.strip()}")
        return
    _out(result.body())


@command("tools", "list registered tools")
def cmd_tools(sc: SlashContext, args: str) -> None:
    tools = sc.agent.build_toolset()
    for t in tools:
        ro = " ro" if t.readonly else "   "
        _out(f" {ro} {t.name:<28} {t.description[:80]}")


@command("map", "print the repo map")
def cmd_map(sc: SlashContext, args: str) -> None:
    from marfim.tools.repomap import build_map
    terms = shlex.split(args) if args else []
    _out(build_map(sc.config.project_root, terms, budget=6000))


@command("mcp", "MCP servers status")
def cmd_mcp(sc: SlashContext, args: str) -> None:
    manager = sc.repl.mcp
    if not manager.clients:
        _out("(no MCP servers configured — add [mcp_servers.name] to config)")
        return
    for name, client in manager.clients.items():
        state = "connected" if client._alive else "down"
        info = f" {client.server_info.get('name', '')} {client.server_info.get('version', '')}"
        _out(f"• {name}: {state}{info}")


@command("usage", "token + cost accounting for this session")
def cmd_usage(sc: SlashContext, args: str) -> None:
    from marfim.providers.registry import model_price
    cost_in, cost_out = model_price(sc.agent.provider.config.name, sc.agent.provider.model)
    per_m = 1_000_000
    cost = (sc.session.usage_in * cost_in + sc.session.usage_out * cost_out) / per_m
    _out(f"tokens: {sc.session.usage_in} in / {sc.session.usage_out} out   "
         f"estimated cost: ${cost:.4f}")


# ------------------------------------------------------------------- actions

@command("run", "run a shell command without the agent: /run <cmd>")
def cmd_run(sc: SlashContext, args: str) -> None:
    if not args.strip():
        _out("usage: /run <command>")
        return
    try:
        proc = subprocess.run(args, shell=True, capture_output=True, text=True,
                              timeout=120, cwd=str(sc.config.project_root))
    except subprocess.TimeoutExpired:
        _out("(timed out)")
        return
    if proc.stdout:
        _out(proc.stdout.rstrip()[:4000])
    if proc.stderr:
        _out("stderr: " + proc.stderr.rstrip()[:2000])
    _out(f"[exit {proc.returncode}]")


@command("undo", "git reset the last commit created by the agent")
def cmd_undo(sc: SlashContext, args: str) -> None:
    try:
        proc = subprocess.run(["git", "log", "-1", "--format=%H %an %s"],
                              cwd=str(sc.config.project_root), capture_output=True,
                              text=True, timeout=10)
        line = proc.stdout.strip()
        if "Marfim" not in line and "marfim" not in line:
            _out("refusing: last commit was not created by Marfim (aider doctrine)")
            return
        subprocess.run(["git", "reset", "--soft", "HEAD~1"], cwd=str(sc.config.project_root),
                       check=True, timeout=10)
        _out("undid last commit (changes staged)")
    except (subprocess.SubprocessError, OSError) as exc:
        _out(f"undo failed: {exc}")


@command("config", "show effective config path + key values")
def cmd_config(sc: SlashContext, args: str) -> None:
    from marfim.config import global_config_path, project_config_path
    _out(f"global:  {global_config_path()}")
    _out(f"project: {project_config_path(sc.config.project_root)}")
    _out(f"provider={sc.config.default_provider} model={sc.config.default_model} "
         f"mode={sc.config.mode} sandbox={sc.config.sandbox_mode}")


@command("doctor", "run environment checks")
def cmd_doctor(sc: SlashContext, args: str) -> None:
    from marfim.doctor import run_doctor
    run_doctor(sc.config)


@command("help", "show slash commands")
def cmd_help(sc: SlashContext, args: str) -> None:
    _out(f"Marfim v{__version__} — slash commands:")
    for name, meta in sorted(COMMANDS.items()):
        _out(f"  /{name:<12} {meta['help']}")
    _out("anything else is sent to the model. /permissions <mode> switches safety.")


@command("exit", "exit Marfim")
def cmd_exit(sc: SlashContext, args: str) -> None:
    raise SystemExit(0)


ALIASES = {"q": "exit", "quit": "exit", "?": "help", "mcp-refresh": "mcp"}


def dispatch(repl, text: str) -> bool:
    """Returns True if the text was a slash command."""
    if not text.startswith("/"):
        return False
    name, _, rest = text[1:].partition(" ")
    name = ALIASES.get(name.strip(), name.strip())
    entry = COMMANDS.get(name)
    if entry is None:
        print(f"unknown command: /{name} (try /help)")
        return True
    try:
        entry["fn"](SlashContext(repl), rest)
    except SystemExit:
        raise
    except Exception as exc:  # noqa: BLE001
        print(f"command error: {type(exc).__name__}: {exc}")
    return True
