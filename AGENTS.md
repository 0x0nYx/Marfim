# AGENTS.md — instructions for coding agents working on Marfim

## Project
Marfim is a clean-room unified coding-agent harness (Python 3.11+, stdlib-only core).
Do not copy code from the projects analyzed in docs/forensic/ — implement from the
behavioral specs only.

## Commands
- Run tests: `python3 -m unittest discover -s tests`
- Run the CLI locally: `marfim --help`
- Compile check: `python3 -m compileall -q marfim/`

## Conventions
- Keep the core dependency-free; optional extras go in `[project.optional-dependencies]`.
- Every tool result must be bounded (see `tools/base.py` ToolResult.bounded).
- Every mutation must flow through `permissions.check()` — no direct tool execution.
- System prompt sections are byte-stable; do not reorder `agent/prompts.py` casually.
- Docs live in docs/; forensic dossiers are read-only historical records — do not edit.
