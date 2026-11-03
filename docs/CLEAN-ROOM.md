# Clean-Room Methodology & Legal Notice

## How Marfim was built

1. **Forensic analysis (read-only).** Ten agent codebases were cloned and analyzed by
   independent agents that inspected README/docs/source and produced dossiers
   (`docs/forensic/*.md`, 3,084 lines) documenting architecture, tools, permissions,
   sessions, and extensibility **with file-path citations**. Repos were not modified.
2. **Synthesis.** Verified behaviors were distilled into the feature matrix
   (`docs/FEATURE-MATRIX.md`) — each capability tagged with its source doctrine.
3. **Clean-room implementation.** Marfim's code was written from the behavioral
   specifications, not from the analyzed source. No code, prompts, or assets were
   copied from any analyzed project. Where a mechanism is conceptually generic
   (permission rules, SSE parsing, JSONL sessions), the implementation is original.

## Sources analyzed (all open source except where noted)

| Project | Repo | License | Dossier |
|---|---|---|---|
| opencode v2 | github.com/sst/opencode | MIT | docs/forensic/opencode.md |
| Codex CLI | github.com/openai/codex | Apache-2.0 | docs/forensic/codex.md |
| Goose | github.com/block/goose | Apache-2.0 | docs/forensic/goose.md |
| Aider | github.com/Aider-AI/aider | Apache-2.0 | docs/forensic/aider.md |
| Claude Code | github.com/anthropics/claude-code + npm 2.1.317 | proprietary | docs/forensic/claude-code.md (behavioral only) |
| REA | github.com/morluto/rea | MIT | docs/forensic/rea.md |
| Hermes Agent | github.com/NousResearch/hermes-agent | MIT | docs/forensic/hermes-agent.md |
| Oh My Pi | github.com/can1357/oh-my-pi | MIT | docs/forensic/oh-my-pi.md |
| dsh-superagent | github.com/deepseek-ai/dsh-superagent-prototype | MIT | docs/forensic/dsh-superagent.md |
| ECC | github.com/affaan-m/ECC | MIT | docs/forensic/ecc.md |

## Claude Code handling (important)

Claude Code's npm distribution is proprietary Anthropic software. Marfim's dossier for
it contains **behavioral documentation only** (public README/CHANGELOG, observed CLI
surface via `--help`, documented settings keys). No decompiled or deobfuscated source
was extracted, stored, or incorporated. REA itself was analyzed as an open-source
project, and its prescribed evidence→clean-room workflow is exactly the process this
document describes: observe behavior, write a spec, implement independently.

The `docs/forensic/claude-code-REA-evidence.md` file contains the REA run log against
the installed Claude Code package: metadata and counts only (file inventory, AST node
counts, identifiers), never source text.

## Trademarks

All product names (Claude, Codex, Goose, Aider, Hermes, opencode, Oh My Pi, DeepSeek,
REA, ECC) are trademarks of their respective owners; Marfim is not affiliated with or
endorsed by any of them.
