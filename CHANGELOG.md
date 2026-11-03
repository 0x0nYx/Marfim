# Changelog

## 0.2.0 — the safety wave (2026-11-03)

Full 34-finding security & correctness review executed. Every finding has a
regression test (`python3 -m unittest discover -s tests`, 115+ tests).

### P0 — the safety system now works
1. **CLI bypass fixed**: `approve_all: False` no longer flips bypass on every
   invocation; `--mode` and `-y` are honored independently.
2. **Project-config trust gate** (`marfim trust add|list|revoke`): an
   untrusted repo's `.marfim/config.toml` may only TIGHTEN (deny/ask rules);
   its hooks, mcp_servers, providers, mode, allow, sandbox, protect_configs
   and project skill dirs are ignored until the user trusts the file
   (sha256-keyed trust store at `~/.marfim/trust.json`). Ported from the
   archived Node tree's `trust.js` design.
3. **patch/multi_edit under protection**: paths are parsed out of the patch
   DSL and every target is permission-checked; prompts show every path.
4. **Plan-mode bash is actually read-only**: real shell-lexer check, shell
   operators and substitutions rejected, per-command flag allowlists,
   `find`/`git branch`/`echo`/`python -c` removed.
5. **Background bash** goes through execpolicy + sandbox, is capped (8 jobs),
   and logs to 0600 files.
6. **"Always allow" grants are exact commands** (no prefix globs), compound
   commands must match EVERY segment, and interpreters/shells/sudo/curl/git
   are never auto-granted.
7. **grep option injection closed**: `rg -e <pattern> -- <path>`; rg errors
   (exit 2) surface instead of "(no matches)".
8. **bypass respects deny rules and config protection**.
9. **Path containment**: secret-like paths (.ssh, .env, .aws, .git/config,
   the Marfim home, the session dir) are denied in every mode; writes must
   stay inside the workspace; the global Marfim config is protected.
10. **Sandbox fails closed**: a requested-but-missing backend is a hard error
    (`sandbox_required=false` is the explicit opt-out); self-defense ro-binds
    for `.git/hooks`, `.git/config`, `.marfim/`, `MARFIM.md`; children run
    with a scrubbed environment; the `network` config key is wired.
11. **Malformed configs refuse to run** (loud error, exit 2) instead of
    silently dropping the safety settings.

### P1 — data loss, silent edits, loop & session
12. outdent strategy applies the replacement (re-based indentation).
13. the destructive `nearest` fuzzy strategy is REMOVED — near-misses are
    reported as hints, never applied; no fuzzy application with replace_all.
14. ambiguous `old_string` errors with the match count.
15. `write` validates arguments against the tool schema, requires read
    evidence for existing files, and is atomic (temp + os.replace, mode kept).
16. stale-read rejection is real: (mtime_ns, sha256) evidence per file.
17. encoding/newline round-trip: CRLF stays CRLF, latin-1 files survive
    byte-for-byte, mixed-endings files are refused.
18. glob/repomap prune ignored dirs during the walk (a project path
    containing `build` no longer returns "(no matches)").
19. tool-call JSON parses leniently (raw newlines, trailing commas); parse
    failures surface as parse errors.
20. interrupts leave NO dangling tool calls (synthetic results on every exit
    path).
21. compaction and /clear survive resume (single `compact` record, replayed).
22. compaction cuts on turn boundaries; failed/empty summaries keep history.
23. context windows are provider-qualified, cached, network-free in the loop.
24. provider layer: `reasoning_content` separated from answers, mid-stream
    errors raise, no-index tool-call deltas keyed by id, transport errors
    wrapped, Anthropic prompt-cache breakpoints + cache-write accounting,
    configurable max_tokens, volatile env moved out of the system prompt.
25. subagents: spawn cwd honored, worktrees live outside the repo and are
    removed with a mergeable diff, role prompts + memory injected,
    role-restricted permission engines (bypass cannot leak), shared
    stop_event.
26. todo state updates memory and file together.
27. repomap: no None-group crashes, indented symbols visible, .kt/.dart/
    .cc/.cxx/.hpp/.mjs supported.
28. REPL has ONE input owner; five prompts sent 1s apart in a real pty get
    five replies (regression-tested).
29. mid-turn steering and Ctrl-C actually work (turns run in a worker thread;
    SIGINT sets stop_event instead of killing the REPL).
30. `marfim mcp add` works (TOML-escaped, duplicate-safe, auto-trusts the
    user's own edit).
31. dead bits revived: SessionStart/Stop hooks fire, home MARFIM.md is used,
    --session-name honored, exit code 2 returned on permission denial,
    Tool.readonly drives the engine, /undo matches only Marfim-authored
    commits, /model only treats known providers as providers, session rm
    cleans snapshots, rewind handles absolute paths and DELETES files the
    agent created, session files are 0600.

### P2 — MCP, hooks, web
32. MCP: server-initiated requests answered (ping/roots/list), stderr drained
    in a thread, dead servers marked dead, pagination followed with a
    loop-guard, tools/list_changed honored, parallel startup, per-call
    timeouts, provider-safe tool names, structuredContent preserved, scrubbed
    child env, thread-safe pending map. **New: streamable-HTTP transport**
    with per-server auth headers.
33. PreToolUse hooks fail CLOSED (timeout ⇒ deny) and run with scrubbed env.
34. webfetch: SSRF guard (private/loopback/link-local/metadata ranges denied,
    re-checked per redirect hop, credentials in URLs refused).

### §4 — unified intelligence
- **Model router**: role → ordered provider/model candidates with 429/5xx
  failover, per-role dollar budgets, and a capabilities record (tools are
  never sent to a model that cannot take them).
- **MCP as universal adapter**: HTTP transport + readOnlyHint-driven
  permission tier.
- **External agents as backends**: `[external_agents]` runs opencode / codex
  / claude-code / hermes headlessly inside their own worktree with a
  stream-json normalizer; the Marfim loop stays the control plane.
- **Parallel subagents**: consecutive `task` calls run in a bounded thread
  pool with cancellation.
- **Verify tool**: `verify.commands` from the trusted config run in the
  sandbox; failures feed a reflection loop bounded by `reflection_cap`.

## 0.1.0 — initial clean-room release

Unified harness synthesized from forensic analysis of opencode v2, Codex,
Goose, Aider, Claude Code, Hermes, Oh My Pi, dsh-superagent, REA and ECC.
Python 3.11+, stdlib-only core.
