# Forensic Analysis: Aider (https://github.com/Aider-AI/aider)

**Analyzed commit:** `5dc9490bb35f9729ef2c95d00a19ccd30c26339c` — "Merge pull request #5173 … anthropic models list add opus 4.5", authored 2026-05-22. The clone is shallow (1 commit); `git ls-remote` at analysis time (2026-11-03) shows remote `HEAD` still equals this commit, so the clone is current with upstream.
**License:** Apache-2.0 (`LICENSE.txt`).
**Primary languages:** Python (application), SCSS/Markdown for the bundled Jekyll docs site (`aider/website/`, 142 `.md` files); tree-sitter `.scm` tag queries: 32 files under `aider/queries/tree-sitter-language-pack/` and 28 legacy under `aider/queries/tree-sitter-languages/`.
**Approx LOC (Python):**
- `aider/` package ≈ 20.3k (top-level modules 13.4k + `aider/coders/` 6.9k)
- tests ≈ 12.4k (`tests/`), benchmark harness ≈ 2.5k (`benchmark/`), scripts ≈ 3.2k (`scripts/`)
- requires Python >=3.10,<3.15 (`pyproject.toml:20`); version `0.86.3.dev` (`aider/__init__.py:3`)
- PyPI distribution `aider-chat`; console entry `aider = "aider.main:main"` (`pyproject.toml:26-28`)

**Current project status:** Dormant/stale — no commits since 2026-05-22, i.e. ~5.5 months of silence before the 2026-11-03 analysis date, and remote `HEAD` is unchanged. Formal "archived" flag could not be verified (GitHub API rate-limited; no archive banner found in an HTML fetch) — **unclear**. Version in-tree is a `.dev` build; README badges (6.8M installs, 15B tokens/week) are static strings in `README.md`, not live signals. HISTORICAL.md/website still present, no deprecation notice found in-repo.

---

**Method note:** all findings below were verified directly in the cloned tree at commit `5dc9490` (read-only; no files modified). Claims carry `file:line` citations; where the brief asserted something not present in the repo (e.g. `.aider.tags.yml`), the correction is flagged. Line numbers refer to the analyzed commit.

---

## 1. Identity & Purpose

Aider is Paul Gauthier's terminal-based AI pair-programming CLI ("AI Pair Programming in Your Terminal", `README.md`). Its core premise:

- The user adds files to a chat; the LLM proposes edits in a *structured text edit format* (not function calling).
- Aider parses, validates (dry-run), and applies the edits, then commits them as git commits attributed to `"<user> (aider)"` (`aider/repo.py:291-314`).
- Everything else orbits this loop: a tree-sitter + PageRank repository map for repo-scale context (`aider/repomap.py`), a model-settings/metadata federation over litellm (`aider/models.py`), in-chat slash commands (`aider/commands.py`), lint/test hooks (`aider/linter.py`), watch-files mode driven by `AI!`/`AI?` comments (`aider/watch.py`), voice input (`aider/voice.py`), a Streamlit browser GUI (`aider/gui.py`), and opt-in analytics (`aider/analytics.py`).
- The documentation site ships inside the wheel and doubles as the `/help` RAG corpus (`aider/help.py:33-39`, optional extras in `requirements/requirements-help.in`: llama-index + huggingface embeddings + torch).
- A self-benchmark harness (polyglot, SWE-bench variants, plot tooling) lives in `benchmark/benchmark.py` and is referenced throughout the website leaderboards.
- The git history of the repo itself demonstrates the dogfooding story: README advertises the "Singularity" metric (percentage of aider's own release code written by aider), and commit attribution features (`scripts/blame.py`) support it.

## 2. Architecture

**Package layout (flat package `aider/`):**
- Entry/config: `aider/main.py` (1,275 lines, CLI orchestration), `aider/args.py` (945 lines of configargparse flags), `aider/args_formatter.py` (markdown/YAML/dotenv help renderers), `aider/deprecated.py`, `aider/onboarding.py` (default-model selection + OpenRouter OAuth).
- I/O: `aider/io.py` (1,191 lines — prompt_toolkit REPL, confirm/prompt gates, colors, streaming markdown), `aider/mdstream.py` (live rich-Markdown renderer), `aider/waiting.py` (spinners), `aider/copypaste.py` (clipboard watcher).
- Cognition: `aider/coders/` (15 coder classes, one `*_prompts.py` per format), `aider/coders/chat_chunks.py`, `aider/history.py` (`ChatSummary`), `aider/repomap.py` (867 lines), `aider/sendchat.py` (role-alternation sanity helpers), `aider/reasoning_tags.py` (thinking-content stripping).
- World: `aider/repo.py` (`GitRepo`, 623 lines), `aider/linter.py`, `aider/watch.py`, `aider/voice.py`, `aider/scrape.py` (Playwright web→markdown), `aider/run_cmd.py` (subprocess runner with cwd/encoding handling), `aider/special.py` (important-files filter), `aider/urls.py` (all doc URLs centralized), `aider/analytics.py`, `aider/exceptions.py` (`LiteLLMExceptions` retry table), `aider/report.py` (GitHub issue auto-report), `aider/versioncheck.py`, `aider/editor.py` (external `$EDITOR` pipe).
- Bundled resources: `aider/resources/model-settings.yml` (357 model entries) and `aider/resources/model-metadata.json` (json5, ~700 lines of overrides; contains `//` comments, hence json5 loading).

```
aider/
├── main.py  args.py  args_formatter.py  deprecated.py  onboarding.py   # CLI & config
├── io.py  mdstream.py  waiting.py  copypaste.py                        # terminal I/O
├── coders/                                                            # the 15 coders
│   ├── base_coder.py  chat_chunks.py  base_prompts.py  shell.py
│   ├── {editblock,wholefile,udiff,udiff_simple,patch,...}_coder.py
│   └── *_prompts.py                    # one prompt class per format
├── models.py  llm.py  openrouter.py  reasoning_tags.py  sendchat.py    # provider layer
├── repo.py  repomap.py  special.py  queries/tree-sitter-*/             # codebase view
├── history.py  linter.py  watch.py  watch_prompts.py                   # session & quality
├── voice.py  scrape.py  run_cmd.py  help.py  gui.py                    # auxiliary UX
└── analytics.py  exceptions.py  urls.py  report.py  versioncheck.py    # ops
```

**Coder class hierarchy** (`aider/coders/__init__.py`; class attribute `edit_format` + `gpt_prompts` in each file):
- Base `Coder` (`aider/coders/base_coder.py`, 2,486 lines) owns the loop, git, history, cost, streaming; subclasses only add prompts + `get_edits()`/`apply_edits()`.
- Direct children: `EditBlockCoder` (`diff`), `EditBlockFencedCoder` (`diff-fenced`), `WholeFileCoder` (`whole`), `UnifiedDiffCoder` (`udiff`), `UnifiedDiffSimpleCoder` (`udiff-simple`), `PatchCoder` (`patch`), `AskCoder` (`ask`), `HelpCoder` (`help`), `ContextCoder` (`context`).
- Second level: `ArchitectCoder` extends `AskCoder`; `EditorEditBlockCoder` (`editor-diff`), `EditorWholeFileCoder` (`editor-whole`), `EditorDiffFencedCoder` (`editor-diff-fenced`) extend their base formats with stripped "editor" prompt sets.
- Legacy function-calling coder `single_wholefile_func_coder.py` (`edit_format="func"`) exists but is commented out of `__all__` — the tool-calling path was abandoned in favor of text formats.
- `Coder.create()` (`base_coder.py:124-201`) is the factory: dispatches on `edit_format`, carries state (fnames, done/cur messages, cost, watcher, commands) from a previous coder via `from_coder`, and re-summarizes done-messages when the edit format changes so stale formatting doesn't confuse the new prompt (`base_coder.py:156-168`).

**Data flow (one user turn):**

```
user input (io.get_input)                          aider/io.py, base_coder.run()
  └─► slash command? ──► Commands.run() ─────────► aider/commands.py (may raise SwitchCoder)
      file mentions / URL detection preproc        base_coder.preproc_user_input()
  └─► run_one() reflection loop (max 3)            base_coder.py:924-945
      └─► send_message()
          ├─ format_chat_chunks() → ChatChunks     base_coder.py:1226-1331, coders/chat_chunks.py
          ├─ Model.send_completion() → litellm     aider/models.py:985-1037
          ├─ stream render (mdstream or plain)     aider/mdstream.py, show_send_output_stream
          ├─ reply_completed() hook                e.g. architect_coder.py:11-48
          ├─ apply_updates(): get_edits → dry-run → prepare_to_edit (allowed_to_edit,
          │    check_for_dirty_commit) → apply_edits          base_coder.py:2296-2336, 2269-2294
          ├─ auto_commit(edited) → GitRepo.commit  base_coder.py:2375-2395, aider/repo.py:200-318
          ├─ lint_edited → commit "Ran the linter" base_coder.py:1599-1607, aider/linter.py
          ├─ run_shell_commands (confirm gate)     base_coder.py:2434-2470
          └─ auto_test → reflect failures on confirm          base_coder.py:1616-1623
```

## 3. Agent Loop

- **REPL loop:** `Coder.run()` (`base_coder.py:876-892`) reads input, handles EOF/KeyboardInterrupt, calls `run_one()`. `main.py:1159-1181` loops over `SwitchCoder` exceptions to hot-swap model/edit-format mid-session (raised by `/model`, `/chat-mode`, `/architect`, etc.).
- **Preprocessing:** `preproc_user_input` (`base_coder.py:912-922`) routes `/commands` first (`commands.is_command`), then auto-detects file mentions (`get_file_mentions`, `base_coder.py:1714-1782`) and URLs — URLs prompt "Add URL to the chat?" and are scraped to markdown (`base_coder.py:964-984`; `aider/scrape.py`, Playwright optional).
- **Prompt assembly** (`format_chat_chunks`, `base_coder.py:1226-1331`):
  - `system`: the coder's `gpt_prompts.main_system` decorated by `fmt_system_prompt` (`base_coder.py:1174-1224`) with: chosen fence, `final_reminders` (`lazy_prompt`/`overeager_prompt` switched on per-model flags, `base_prompts.py:12-20`), platform info (`get_platform_info`, `base_coder.py:1127-`), shell-command policy text (`coders/shell.py`), and the user's chat language.
  - `examples`: few-shot user/assistant pairs per format; for models with `examples_as_sys_msg` they're folded into the system text and terminated with a "I switched to a new code base…" divider pair (`base_coder.py:1232-1259`).
  - `done` (summarized history), `repo` (repo map), `readonly_files`, `chat_files` (contents + `files_content_prefix`), `cur` (current turn), `reminder`.
  - `system_reminder` (per-format rules restated, e.g. `editblock_prompts.py:120-159`) is injected either as a trailing system message (`reminder="sys"`) or appended into the final user message (`reminder="user"`), and only if the projected total still fits the context window (`base_coder.py:1297-1329`).
  - `use_system_prompt=False` models get the system text as a user/assistant pair instead (`base_coder.py:1266-1274`).
  - Fence is chosen per chat-file contents — quadruple backticks when files contain triple-backtick blocks (`choose_fence`, `base_coder.py:609-636`; known-bug comment at `base_coder.py:77-85`).
- **Send + retry:** `send_message` (`base_coder.py:1419-1624`) — via `LiteLLMExceptions` mapping (`aider/exceptions.py`): exponential backoff starting 0.125s, capped at `RETRY_TIMEOUT = 60` (`models.py:26`); context-window exhaustion → `show_exhausted_error` diagnostics (`base_coder.py:1628-1679`); output-limit + `supports_assistant_prefill` → continue by feeding the partial reply back as assistant prefill (`base_coder.py:1492-1505`).
- **Reply parsing pipeline:** `partial_response_content` → coder's `get_edits()` → `apply_edits_dry_run` (validate everything before touching disk) → `prepare_to_edit` (permission gate `allowed_to_edit`, per-file `check_for_dirty_commit`) → `apply_edits` (`base_coder.py:2296-2336`, `2269-2294`). Any parser `ValueError` becomes `reflected_message`.
- **Reflection/retry:** `run_one` loops while `reflected_message` is set, capped at `max_reflections = 3` (`base_coder.py:101`, `924-945`). Reflection sources: malformed edits (`apply_updates`), failed SEARCH-match errors with "Did you mean to match these actual lines…" hints from `find_similar_lines` (`aider/coders/editblock_coder.py:84-124`), newly-mentioned files to add (`check_for_file_mentions`, `base_coder.py:1560-1567`), lint errors and test failures (each gated by a user confirm, `base_coder.py:1599-1623`).
- **Auto-commit:** `auto_commit` (`base_coder.py:2375-2395`) commits exactly the edited files with chat context; the template "I committed the changes with git hash {hash} & commit msg: {message}" is injected as a user message via `move_back_cur_messages` (`base_coder.py:1036-1047`; template `base_prompts.py:4`). No valid edits → "I didn't see any properly formatted edits in your reply?!" (`base_prompts.py:8`).
- **Interrupt safety:** Ctrl-C mid-stream keeps partial content in the transcript with explicit "^C KeyboardInterrupt" markers, so the model can resume coherently (`base_coder.py:1489-1491`, `1575-1583`).
- **Cache warming:** background thread re-pings the provider every ~5 min with `max_tokens=1` over the stable prompt prefix to keep prompt caches hot (`base_coder.py:1340-1394`; `--cache-keepalive-pings`).
- **Session announcements:** `get_announcements` (`base_coder.py:207-298`) composes the banner: version, main/weak model names, edit format, repo map size, files in chat, confirmation of `auto-commits`/`dirty-commits`/lint/test settings.

## 4. Tool Set / Edit Formats

No native tool/function calling is active; "tools" are text protocols plus local executors.

| Edit format | Coder class / file | Prompts file | Parser & applier |
|---|---|---|---|
| `whole` | `WholeFileCoder` (`wholefile_coder.py`) | `wholefile_prompts.py` | filename line before a fenced block → whole-file replace (`get_edits`, lines 20-110); supports live diff rendering while streaming |
| `diff` | `EditBlockCoder` (`editblock_coder.py`) | `editblock_prompts.py` | `find_original_update_blocks` regex over `<<<<<<< SEARCH / ======= / >>>>>>> REPLACE`; apply via `replace_most_similar_chunk` |
| `diff-fenced` | `EditBlockFencedCoder` | `editblock_fenced_prompts.py` | same parser; four-backtick outer fences for models that echo fences |
| `udiff` | `UnifiedDiffCoder` (`udiff_coder.py`) | `udiff_prompts.py` | `find_diffs` hunks (`@@ … @@`), applied via `search_replace.py` flexible strategies; `SearchTextNotUnique` / no-match errors formatted for reflection (lines 16-118) |
| `udiff-simple` | `UnifiedDiffSimpleCoder` (`udiff_simple.py`) | `udiff_simple_prompts.py` | udiff without few-shot examples |
| `patch` | `PatchCoder` (`patch_coder.py`) | `patch_prompts.py` | OpenAI `apply_patch`-style V4A grammar: `Add/Delete/Update File` actions, context matching with fuzz levels 0/1/100 (exact → rstrip → strip; `find_context_core`, lines 59-80) |
| `ask` | `AskCoder` (`ask_coder.py`) | `ask_prompts.py` | no edits |
| `architect` | `ArchitectCoder` (`architect_coder.py`) | `architect_prompts.py` | two-model flow: planner reply → `Coder.create(editor_model, editor_edit_format, map_tokens=0, suggest_shell_commands=False)` re-applies it (`reply_completed`, lines 11-48); confirm "Edit the files?" unless auto-accepted |
| `context` | `ContextCoder` (`context_coder.py`) | `context_prompts.py` | model names files; auto-adds mentions and reflects "try again" until the mentioned set equals the added set; repo map enlarged ×`map_mul_no_files` (`__init__`, lines 12-21) |
| `help` | `HelpCoder` (`help_coder.py`) | `help_prompts.py` | no edits; llama-index RAG over bundled website (`aider/help.py`) |
| `editor-diff` / `editor-whole` / `editor-diff-fenced` | `aider/coders/editor_*_coder.py` | `editor_*_prompts.py` | "output only edits" variants; used for the architect's editor role and `--copy-paste` mode (`main.py:871-873`) |
| `func` (disabled) | `single_wholefile_func_coder.py` | `single_wholefile_func_prompts.py` | commented out of `aider/coders/__init__.py:16,28` |

Notes on the matcher stack (`aider/coders/editblock_coder.py:127-205`, `aider/coders/search_replace.py`):
- Matching ladder: perfect match → leading-whitespace-outdent-invariant match → drop-spurious-leading-blank-line → `...`-ellipsis chunked replacement (`try_dotdotdots`).
- A `SequenceMatcher` fuzzy matcher exists but is dead code behind an early `return` — deliberately disabled to avoid wrong-location edits.
- `search_replace.py` also provides the udiff applier's flexible strategies and diff-match-patch line operations (`flexible_search_and_replace`, `diff_lines`, `git_cherry_pick_*` functions, lines 193-580).

**Shell integration:** models suggest shell commands in fenced `bash/sh/cmd/powershell/…` blocks, extracted as `(None, cmd)` edits (`editblock_coder.py:33`, shell-prefix list ~lines 470-490); each run requires `confirm_ask(..., explicit_yes_required=True)` (`base_coder.py:2450-2463`); execution via `aider/run_cmd.py`; `/run` (alias `!`) runs user commands and offers to add output to chat, seeding a "What's wrong? Fix" placeholder on failure (`commands.py:1013-1055`).

**Lint/test hooks:**
- `Linter` (`aider/linter.py`): tree-sitter `TreeContext`-based `basic_lint` for any language; Python gets compile-check + flake8 (`py_lint`, lines 118-150); per-language overrides via `--lint-cmd "lang:cmd"`; output parsed for file:line refs to scope the reflection (`errors_to_lint_result`, lines 70-80).
- Auto-lint after edits defaults on (`auto_lint=True`, `base_coder.py:105`); successful lint gets its own commit "Ran the linter" (`base_coder.py:1599-1602`).
- `--test-cmd` + `--auto-test` (default off) runs once after edits; failures offered for reflection (`base_coder.py:1616-1623`; `commands.cmd_test`, `commands.py:993-1012`).

## 5. Model/Provider Layer

- **litellm is the only provider client** — pinned `litellm==1.82.3` (`requirements.txt:184`), behind the lazy shim `aider/llm.py`. Heavy imports (`litellm`, `networkx`, `numpy`, `httpx`) are deferred; on non-first runs they load in a background thread, tracked per (version, interpreter) in `~/.aider/installs.json` (`aider/main.py:1183-1269`).
- **`Model` class** (`aider/models.py:329-`): dataclass `ModelSettings` (`models.py:128-159`) holds behavior: `edit_format`, `weak_model_name`, `use_repo_map`, `lazy`, `overeager`, `reminder`, `examples_as_sys_msg`, `extra_params`, `cache_control`, `caches_by_default`, `use_system_prompt`, `use_temperature`, `streaming`, `editor_model_name`, `editor_edit_format`, `reasoning_tag` (deprecated alias `remove_reasoning`), `system_prompt_prefix`, `accepts_settings`. Per-model behavior is data — 357 entries in `aider/resources/model-settings.yml` (e.g. gpt-4o → `diff`, weak `gpt-4o-mini`, lazy; gemini-2.5-pro → `diff-fenced`, weak flash).
- **Metadata resolution chain** (`ModelInfoManager`, `models.py:161-290`): user-registered local metadata → cached litellm `model_prices_and_context_window.json` (24h TTL in `~/.aider/caches`) → live litellm API → OpenRouter API cache (`aider/openrouter.py`) → scraping the OpenRouter model page as last resort (`fetch_openrouter_model_info`, lines 276-290).
- **User extension:** `register_models` loads `.aider.model.settings.yml`, replacing entries by exact name (`models.py:1085-1110`); `register_litellm_models` loads `.aider.model.metadata.json` via `json5` (comments tolerated, `models.py:1112-1130`); files resolved CWD → git root → CLI arg (`main.py:305-410`).
- **Aliases & search:** `MODEL_ALIASES` built-ins plus `--alias name:model` (`models.py:~110-125`; `main.py:764-775`); `fuzzy_match_models` powers `/models` search and typo suggestions (`models.py:1227-`).
- **Weak/cheap model routing:** `Model.weak_model` (per-model default, `--weak-model` override) is used for commit messages and chat summarization; `commit_message_models()` returns `[weak_model, self]` so GitRepo falls back to the main model on failure (`models.py:622-623`; `repo.py:342-363`). Commit-message prompt: Conventional-Commits one-liner, ≤72 chars, imperative mood (`aider/prompts.py:8-27`), with optional `--commit-prompt` override and `--commit-language`.
- **Editor models:** `editor_model_name` + `editor_edit_format` route the architect editor role and copy-paste mode; `diff`/`whole`/`diff-fenced` are auto-prefixed with `editor-` (`models.py:625-645`; `main.py:871-873`).
- **Sizing heuristics:** `max_chat_history_tokens = clamp(max_input/16, 1024, 8192)` (`models.py:355-358`); repo-map tokens `clamp(max_input/8, 1024, 4096)` (`models.py:782-789`); image token cost via 512-tile math (`models.py:672-710`).
- **Sanity checks:** `sanity_check_models`/`sanity_check_model` warn on missing `max_input_tokens`, no streaming, unknown provider keys — with doc URL offered (`models.py:1146-1200`; invoked `main.py:890-901`, `--no-show-model-warnings` to skip). `validate_environment` checks provider env keys before launch, with a fast path that avoids importing litellm (`models.py:712-780`). `accepts_settings` gates `--reasoning-effort`/`--think-tokens` per model unless `--no-check-model-accepts-settings` (`main.py:836-869`).
- **Call plumbing:** `send_completion` (`models.py:985-1037`) injects `extra_params` (incl. `extra_body.reasoning_effort` / thinking budget), computes `num_ctx` for Ollama, adds GitHub-Copilot token-exchange headers, honors `request_timeout`; `simple_send_with_retries` (lines 1039-1082) is the non-streaming helper used by commit/summary flows; reasoning content stripped per `reasoning_tag` (`aider/reasoning_tags.py`); DeepSeek-R1 gets forced role alternation (`sendchat.py:30-55`).
- **Onboarding:** with no model/keys configured, aider probes env for known keys, checks OpenRouter key tier (free tier → `openrouter/deepseek/deepseek-r1:free`), and can run a full PKCE OAuth device flow against OpenRouter to mint an API key (`aider/onboarding.py:31-200`; wired at `main.py:777-820`).

## 6. Permissions & Safety

- **Central confirmation gate:** `InputOutput.confirm_ask` (`aider/io.py:806-925`): Yes/No/(A)ll/(S)kip-all/(D)on't-ask-again, `ConfirmGroup` batching (one decision for a class of prompts, e.g. all suggested shell commands), `never_prompts` per session; every Q&A is logged to the chat history file.
- **"Yolo" = `--yes-always`** (`aider/args.py:761-762`; no literal `--yolo` flag exists): sets `io.yes=True`, which auto-answers "y" for normal prompts but auto-answers **"n"** for `explicit_yes_required` prompts (`io.py:827-828`) — notably LLM-suggested shell commands (`base_coder.py:2456-2462`). Net effect: even in yolo mode aider never executes model-proposed shell commands unattended; yolo auto-accepts edits/architect/lint/test prompts only.
- **Edit permissioning:** only files added to the chat are writable; edits to other tracked files require per-file confirmation; gitignored paths refused outright; new files created only after confirm and `git add`-ed when auto-commits are on (`allowed_to_edit`, `base_coder.py:2191-2243`).
- **Git safeguards:**
  - `/undo` refuses unless HEAD was committed by aider *in this session* (`aider_commit_hashes`), refuses merge commits, refuses when any touched file is dirty, refuses when the commit is already pushed to `origin/<branch>` (`aider/commands.py:560-655`).
  - Dirty files under edit are committed *before* edits so undo stays possible (`check_for_dirty_commit` / `dirty_commit`, `base_coder.py:2175-2188`, `2411-2423`).
- **Attribution:** commits set `GIT_AUTHOR_NAME`/`GIT_COMMITTER_NAME` to `"<user> (aider)"` via temporary env context managers (`aider/repo.py:39-49`, `291-314`); optional `Co-authored-by: aider <...>` trailer via `attribute_co_authored_by` (`repo.py:248-259`); commit-message attribution toggles separately (`attribute_commit_message_author/committer`).
- **Analytics opt-in:** asked once on first run and persisted (`main.py:642-662`); `--analytics-disable` permanently disables; events are counters/model names — on-screen text promises no code, chat content, keys, or personal info (`main.py:644-648`); 10% default sampling by user-UUID hex prefix (`analytics.py:15`, `30-52`); PostHog/Mixpanel keys hardcoded (`analytics.py:55-57`); local JSONL via `--analytics-log`; test suite forces it off (`pytest.ini` env `AIDER_ANALYTICS=false`).
- **Filesystem/watch limits:** watch mode ignores files >1MB plus a built-in ignore list (`watch.py:15-62`, `108`); `.aiderignore` (gitignore syntax, resolved against git root) filters repo files (`repo.py:500-559`); URL scraping and URL-opening are separately confirmed (`base_coder.py:964-984`; `io.offer_url`).
- **Repo auto-init:** in a non-git directory, aider offers to create a git repo (default yes) and writes a `.gitignore` containing `.aider*` (`main.py:88-153`); `--no-git` runs without any git features (no auto-commit, no repo map from tracked files).
- **Hygiene:** auto-adds `.aider*`/`.env` to `.gitignore` with consent (`main.py:155-210`); pre-flight repo sanity check (`sanity_check_repo`, `main.py:412-449`); sensitive args scrubbed from logs (`scrub_sensitive_info`, `main.py:749-751`); `--no-verify-ssl` plumbs into litellm/httpx/model-info fetches (`main.py:519-527`).

## 7. Sessions, State & Context

- **History files** (defaults relative to git root, `aider/args.py:277-296`): `.aider.chat.history.md` (full human-readable transcript incl. confirmations), `.aider.input.history` (prompt_toolkit input history), optional `.aider.llm.history` (`--llm-history-file`) for raw LLM traffic. `--restore-chat-history` replays the previous transcript into context.
- **Session save/load:** `/save` writes a command script (`/add`, `/read-only`, …) that reconstructs the session; `/load` executes it (`aider/commands.py:1465-1523`).
- **Undo/commit state:** `commit_before_message` head-SHA stack powers the "You can use /undo…" hint (`base_coder.py:2405-2409`); `/diff` shows changes since the last aider commit (`commands.py:657-696`).
- **File sets:** `abs_fnames` (editable) vs `abs_read_only_fnames` (reference-only; `/read-only` accepts files and directories, `commands.py:1328-1417`); `/drop`, `/clear`, `/reset` manage them; addable files = git-tracked + staged minus in-chat (`get_addable_relative_files`, `base_coder.py:2169-2174`). Files the assistant names are auto-*added* only via reflection, not silently.
- **Context management:**
  - Pre-send guard `check_tokens` blocks sends exceeding `max_input_tokens` with reduction advice (`base_coder.py:1396-1418`).
  - History beyond `max_chat_history_tokens` goes through `ChatSummary.summarize`: split so the most recent ~half stays verbatim, the older head is summarized by the weak-model chain, recursing up to depth 3 (`aider/history.py:27-123`; template `aider/prompts.py:47-61` — user-first-person summary that must preserve filenames and identifiers, no fenced code). Summarization runs in a background worker while the user types (`summarize_worker`, `base_coder.py:1014-1035`).
  - Exhausted-context diagnostics with input/output/total estimates and remediation tips (`show_exhausted_error`, `base_coder.py:1628-1679`).
- **Repo map state:** tree-sitter tag cache = `diskcache` dir `.aider.tags.cache.v4` (v3 for the legacy pack) keyed by path+mtime (`aider/repomap.py:35-45`, `217-264`); rendered map memoized per (chat files, mentions, tokens) key; refresh policies `auto` (cache if last build >1s), `files`, `always`, `manual` (`repomap.py:599-627`); `/map` prints, `/map-refresh` forces (`commands.py:1418-1431`); `--cache-prompts` forces refresh to `files` for cache stability (`main.py:954-955`).
- **All on-disk `.aider*` artifacts** (grep-verified): `.aider.conf.yml`, `.aider.model.settings.yml`, `.aider.model.metadata.json`, `.aiderignore`, `.aider.chat.history.md`, `.aider.input.history`, `.aider.llm.history` (opt), `.aider.tags.cache.v4/` (dir), `~/.aider/caches/` and `~/.aider/installs.json` (home-level).
- **Watch mode:** `FileWatcher` thread (`watchfiles` library) scans saved files for `AI!`/`AI?` comments (regex `watch.py:69-71`), interrupts the input prompt (`handle_changes` → `io.interrupt_input`), auto-adds touched files to the chat, and assembles a prompt with the AI-comment lines re-rendered via `TreeContext` context windows (`watch.py:181-255`; prompt templates `aider/watch_prompts.py`).
- **Images & clipboard:** image files are base64-attached with size-aware token accounting (`get_images_message`, `base_coder.py:817-858`); `/paste` pulls image or text from clipboard (`commands.py:1278-1327`); copy-paste watcher mode auto-sends clipboard content (`aider/copypaste.py`; `--copy-paste` switches edit format to `editor-*`, `main.py:871-873`).

## 8. UI/UX

- **REPL:** prompt_toolkit with emacs (default) or vi (`--vim`) keybindings; multiline entry via `{tag}` open/close braces or `--multiline`; Ctrl-C-safe interruption semantics documented (`aider/website/docs/usage/commands.md:75-120`; implementation `aider/io.py:523-746`).
- **Complete slash-command list** (auto-discovered from `cmd_*` methods, `commands.py:1103-1119`; canonical generated table `aider/website/docs/usage/commands.md:22-67`):
  - Files/context: `/add`, `/drop`, `/read-only`, `/ls`, `/map`, `/map-refresh`, `/clear`, `/reset`, `/load`, `/save`, `/paste`
  - Modes/models: `/chat-mode`, `/architect`, `/ask`, `/code`, `/context`, `/model`, `/models`, `/editor-model`, `/weak-model`, `/reasoning-effort`, `/think-tokens`
  - Git/quality: `/commit`, `/undo`, `/diff`, `/lint`, `/test`, `/run` (alias `!`), `/git`
  - Meta/IO: `/copy`, `/copy-context`, `/settings`, `/tokens`, `/help`, `/voice`, `/web`, `/report`, `/editor` (`/edit`), `/multiline-mode`, `/ok`, `/exit` (`/quit`)
- **Autocomplete:** command names; file paths for `/add`/`/drop`/`/read-only`/`/load` (globbed, repo-filtered, incl. read-only set, `commands.py:702-765`, `905-911`, `1462-1496`); word completions from chat files; shell completions for bash/zsh/fish via shtab (`--shell-completions`, `main.py:506-510`).
- **Themes/colors:** `--dark-mode`/`--light-mode` presets and granular `--user-input-color`, `--tool-output/error/warning-color`, `--assistant-output-color`, completion-menu colors (`main.py:532-544`; style builder in `io.py`); pygments `--code-theme` for fenced code; streaming Markdown rendered live by `aider/mdstream.py`; non-Unicode terminals auto-degrade to plain output (`main.py:580-587`).
- **Voice:** `/voice` records via sounddevice with live RMS meter → wav/mp3/webm → litellm transcription → prompt placeholder (`aider/voice.py`; `commands.py:1252-1277`); flags `--voice-language`, `--voice-format`, `--voice-input-device`.
- **Browser GUI:** `--browser`/`--gui` launches a Streamlit app (`aider/gui.py`, `main.py:233-249`) that embeds the CLI by capturing `InputOutput` output lines (`CaptureIO`, `gui.py:20-33`); optional extra `requirements/requirements-browser.txt`.
- **Misc UX:** waiting spinner (`aider/waiting.py`), terminal bell/notifications (`--notifications`, `--notifications-command`), per-message + cumulative token/cost report (`base_coder.py:1994-2101`), `/tokens` per-chunk breakdown (`commands.py:445-552`), `/report` prefilled GitHub issue (`commands.py:1555-1567`), `/copy-context` exports the exact prompt context as markdown (`commands.py:1638-`), `/settings` prints resolved configuration.
- **Keybindings:** emacs default (Ctrl-P/N history, Ctrl-R reverse search, Ctrl-X Ctrl-E external editor) and vi via `--vim` (Esc/i/a/A/I navigation) — documented matrix in `aider/website/docs/usage/commands.md:83-134`; multiline toggling swaps Enter and Meta+Enter (`/multiline-mode`, `io.toggle_multiline_mode`).

## 9. Extensibility

- **Config hierarchy (search path)** for `.aider.conf.yml`: CWD → git root → `$HOME`, explicit `--config` (`aider/main.py:464-477`). Every flag is simultaneously an env var (prefix `AIDER_` via configargparse `auto_env_var_prefix`, `args.py:36-42`) and a YAML key. The parser is built and run **twice** so a `.env` chosen by args can influence the final parse (`main.py:479-504`); corrupt boolean values in configs are detected and reported specially (`main.py:480-486`).
- **`.env` files:** git-root `.env` plus `--env-file`; documented keys in `aider/website/assets/sample.env` (provider API keys; `AIDER_*` mirrors of every flag, e.g. `AIDER_MODEL`).
- **Model customization:** `.aider.model.settings.yml` (same schema as the shipped 357-entry file) and `.aider.model.metadata.json` (json5) — both user-overridable per model name (`models.py:1085-1130`).
- **Correction to brief:** there is **no `.aider.tags.yml`** anywhere in the repo (verified by grep). The tree-sitter tag cache is the directory `.aider.tags.cache.v4` (`repomap.py:43`); tag queries are packaged `.scm` files, not user config.
- **Prompts:** edit-format prompts are Python class attributes (`aider/coders/*_prompts.py`) with `{placeholder}` slots filled by `fmt_system_prompt`; overridable in code (subclass `Coder`, pass `gpt_prompts`), but **not** via a user-facing prompts directory. User prompt hooks: `--commit-prompt` only. `--show-prompts` dumps the assembled messages for inspection (`main.py:1044-1051`).
- **README/conventions:** respected by instruction only — docs advise `/read-only CONVENTIONS.md` (`aider/website/docs/usage/conventions.md`); nothing in code auto-loads README/CONVENTIONS (grep confirms).
- **Chat modes:** `/chat-mode` lists modes from registered coders' docstrings plus curated help/ask/code/architect descriptions (`commands.py:138-204`).
- **API surface / scripting:** `aider.coders.Coder.create(...)`, `aider.models.Model`, `GitRepo` form the de-facto API (used by `aider/gui.py`, `benchmark/benchmark.py`; documented in `aider/website/docs/scripting.md`); `main(..., return_coder=True)` returns a configured coder (`main.py:1018-1020`); non-interactive use via `--message`/`--message-file`/`--load`; `--apply` applies a pre-made LLM reply offline without any LLM call (`main.py:1082-1093`).

## 10. Configuration

Precedence (highest → lowest), verified across `args.py`, `main.py:464-510`, `models.py:1085-1130`:

1. **CLI flags** (incl. `--set-env NAME=value`, `--api-key provider=key`, `--model`).
2. **`AIDER_*` environment variables** (configargparse auto-mapping of every flag).
3. **`.aider.conf.yml`** — CWD, then git root, then `$HOME`; earlier files win; `--config` replaces the search path.
4. **`.env` files** (git-root `.env`, then `--env-file`) — loaded after the first parse so `AIDER_*` entries are honored on the second parse; bare provider keys (`OPENAI_API_KEY`, …) are consumed by litellm/models.
5. **`.aider.model.settings.yml` / `.aider.model.metadata.json`** override the bundled resources per model name (orthogonal axis: model data, not flag data).

Notable keys (full annotated list: `aider/website/assets/sample.aider.conf.yml`, 480 lines; help rendered by `aider/args_formatter.py`):
- Model: `model`, `edit-format`, `weak-model`, `editor-model`, `editor-edit-format`, `alias`, `reasoning-effort`, `thinking-tokens`, `check-model-accepts-settings`
- Git: `git`, `auto-commits`, `dirty-commits`, `attribute-author`, `attribute-committer`, `attribute-co-authored-by`, `attribute-commit-message-author/committer`, `commit-prompt`, `git-commit-verify`, `aiderignore`, `subtree-only`, `add-gitignore-files`
- Quality: `auto-lint`, `lint-cmd`, `auto-test`, `test-cmd`, `dry-run`
- Context: `map-tokens`, `map-multiplier-no-files`, `map-refresh`, `max-chat-history-tokens`, `cache-prompts`, `cache-keepalive-pings`, `read`, `watch-files`
- UX/IO: `yes-always`, `stream`, `pretty`, `dark-mode`, `light-mode`, `vim`, `multiline`, `code-theme`, `notifications(-command)`, `suggest-shell-commands`, `detect-urls`, `copy-paste`, `fancy-input`, `line-endings`, `encoding`
- Ops: `analytics(-disable/-log/...)`, `check-update`, `upgrade`, `show-model-warnings`, `skip-sanity-check-repo`, `verify-ssl`, `timeout`, `browser`/`gui`

Non-config facts worth knowing:
- Unknown args: `parse_known_args` is used for the first pass so `.env`-provided args can be merged; truly unknown flags error on the second parse (`main.py:481-504`).
- Every flag's help text is auto-rendered into docs via cog (`aider/website/docs/config/options.md` uses `cog` blocks like `commands.md`), keeping docs and `args.py` in lockstep.

## 11. Standout Features

1. **Repo map = tree-sitter tags + personalized PageRank.** Each file is parsed by a language-specific `.scm` query (`aider/queries/tree-sitter-language-pack/`), yielding `def`/`ref` tags captured from `name.definition.*` / `name.reference.*` capture groups (`repomap.py:279-336`).
2. **Def-only language fallback:** languages whose queries only emit definitions (e.g. C++) get references backfilled by a Pygments lexer token pass (`repomap.py:343-363`).
3. **Graph construction:** `networkx.MultiDiGraph` with referencer→definer edges; edge weight = multiplier × √(num_refs); multipliers ×10 for identifiers mentioned in chat or snake/kebab/camelCase ≥8 chars, ×0.1 for `_`-prefixed or defined in >5 files, ×50 when the referencer is a chat file; unreferenced definitions get 0.1-weight self-edges (`repomap.py:470-514`).
4. **Personalized PageRank** biases rank toward chat files and files whose path components/basename match mentioned identifiers (`personalize = 100/num_files`, `personalization` + `dangling` args) (`repomap.py:375-445`, `519-531`); rank is then redistributed along out-edges to (file, identifier) pairs for the final tag ordering (`repomap.py:533-558`).
5. **Budget-fit binary search:** number of included tags is binary-searched against `max_map_tokens` with 15% tolerance; rendered as `TreeContext` signature snippets per file, lines truncated to 100 chars (`repomap.py:666-706`, `710-784`); "important" files (README, lockfiles, via `aider/special.py`) force-included (`repomap.py:656-662`).
6. **Edit-format zoo with per-model binding** — 12+ formats; the format is *data* in `model-settings.yml`, so per-model behavior ships as config, not code.
7. **Dry-run validation before applying:** every format validates all edits (`apply_edits_dry_run`) before touching disk; partial failures produce precise reflection messages including nearest-match snippets (`base_coder.py:2296-2336`; `editblock_coder.py:84-124`).
8. **Flexible-but-safe SEARCH matcher:** exact → whitespace-outdent → leading-blank → `...`-ellipsis; fuzzy matching deliberately disabled (dead code) to prevent wrong-location damage (`editblock_coder.py:127-205`).
9. **Architect two-model pattern:** planner model converses; an `editor-*`-format coder applies edits with repo map disabled and history summarization off — decouples reasoning cost from edit cost (`architect_coder.py:11-48`).
10. **Git-native workflow:** per-turn auto-commits scoped to edited files, "(aider)" attribution, undo guarded against non-aider/merge/pushed commits, dirty-commit-before-edit for undoability (`repo.py:200-318`; `commands.py:560-655`).
11. **Weak-model delegation** for commit messages and history summarization with fallback to the main model (`models.py:622-623`; `repo.py:342-363`; `history.py:114-123`).
12. **Prompt caching:** cache-control breakpoints on stable chunk boundaries (examples-or-system → repo+readonly → chat_files) plus a keepalive thread re-pinging with `max_tokens=1` every ~5 min (`coders/chat_chunks.py:27-45`; `base_coder.py:1340-1394`).
13. **system_reminder placement strategy:** edit-format rules re-injected near the end of the prompt, as system or user message per model, omitted when the token budget is tight (`base_coder.py:1297-1329`).
14. **Watch-files ambient mode:** `AI!`/`AI?` comments in any watched file interrupt the REPL and assemble a prompt with AST-context around the comments (`aider/watch.py`).
15. **Model metadata federation:** bundled overrides → cached litellm DB (24h TTL) → litellm API → OpenRouter API → OpenRouter page scraping, behind one `get_model_info` (`models.py:161-290`).
16. **Startup-time engineering + bundled RAG docs:** lazy heavy imports keyed by (version, interpreter) installs.json (`main.py:1183-1269`); the website itself is the `/help` corpus (`aider/help.py:33-39`).

## 12. Lessons for Marfim

- **Adopt: text edit formats + reflection, not tool calling.** Structured text (`SEARCH/REPLACE`) with dry-run validation and a reflection cap (3) is provider-agnostic, streamable, cache-friendly, and debuggable; the abandoned function-calling coder (`single_wholefile_func_coder.py`, commented out of `__all__`) is field evidence. Keep parsers in independently testable modules like `search_replace.py`.
- **Adopt: separate "reasoner" and "editor" models.** The architect/editor split with `editor-edit-format`, plus a weak-model chain for commit messages/summaries, is a clean cost lever; make model *roles* (main/weak/editor) first-class rather than ad-hoc flags.
- **Adopt: config-as-data for models.** A shipped `model-settings.yml` keyed by model name (edit format, reminder placement, reasoning tags, cache flags) plus user-override files handles per-model quirks without code changes; mirror the metadata fallback chain (local → cached upstream DB → provider API → scrape) with TTL caching.
- **Adopt: git as the safety substrate.** Commit-before-edit, per-turn auto-commits scoped to edited files, attribution, and an undo that refuses merge/dirty/pushed states are cheap and trust-building; copy Aider's guard list (`commands.py:560-655`).
- **Adopt: the two-pass confirmation design** — one `confirm_ask` gate with All/Skip/Don't-ask-again plus ConfirmGroup batching — and the "yolo never auto-runs shell" nuance: auto-approve edits, but require explicit consent for side-effectful commands.
- **Adopt: chunked prompt assembly** (system / examples / done / repo / readonly / chat_files / cur / reminder) as a first-class structure — it makes caching breakpoints, reminder placement, and token accounting tractable (`coders/chat_chunks.py`).
- **Adopt: repo-map concept**, mindful of costs: heavy deps (tree-sitter pack, networkx, diskcache), mtime-cache complexity, per-language query maintenance. A Marfim equivalent could swap PageRank for cheaper heuristics while keeping the "binary-search tag count into a token budget" idea (`repomap.py:666-706`).
- **Avoid: god classes.** `base_coder.py` (2,486 lines) mixes UI, git, retry, parsing, cost display, and cache warming; `main.py`'s ~700-line `main()` resists extension. Split loop / assembly / persistence / UI from day one.
- **Avoid: Python-class prompt templates.** Prompts live as class attributes across 14 files with format holes; they can't be inspected or overridden without code. Ship prompts as data files with the same placeholder scheme, and keep introspection (`--show-prompts` equivalent).
- **Avoid: config-precedence cleverness.** Double-parsing argparse to fold `.env` into flags, three config locations, and 130+ flags produce subtle failure modes (the code even special-cases corrupt booleans in configs, `main.py:480-486`). Use fewer, layered, explicitly-ordered sources; provide a single resolved-settings dump (Aider's `/settings`, `commands.py:1432-1461`, is worth copying).
