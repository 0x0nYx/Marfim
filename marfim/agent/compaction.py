"""Compaction — layered context management (merged doctrine).

- goose/opencode: threshold-triggered summarization keeping a verbatim tail.
- hermes: deterministic pruning of old large tool results first, then summary.
- codex: pre-sampling compact + manual /compact share one implementation.

Budget estimate uses ~4 chars/token heuristic.
"""

from __future__ import annotations

from dataclasses import dataclass

from marfim.providers.base import Usage
from marfim.util import estimate_tokens, truncate_middle

SUMMARY_PROMPT = (
    "Compress this coding-agent conversation for continuation. Keep: the user's "
    "goals and constraints; every file path touched and how it changed; current "
    "task state (done/remaining); key decisions and their reasons; errors hit and "
    "their fixes. Be specific and terse. Output plain text."
)


@dataclass
class CompactionResult:
    messages: list
    summary: str | None
    tokens_before: int
    tokens_after: int


def prune_tool_results(messages: list, keep_last: int = 8,
                       keep_head: int = 400) -> list:
    """Layer 1 (deterministic): shrink old tool result payloads."""
    out = [dict(m) for m in messages]
    n = len(out)
    for i, msg in enumerate(out):
        if msg.get("role") == "tool" and i < n - keep_last:
            content = msg.get("content")
            if isinstance(content, str) and len(content) > keep_head * 3:
                msg["content"] = truncate_middle(content, keep_head,
                                                 marker="\n… [pruned by compaction]")
    return out


def context_window_for(model: str, default: int = 200_000) -> int:
    from marfim.providers.registry import list_catalog_models
    provider, _, model_id = model.partition("/")
    candidates = [m for m in list_catalog_models(provider) if m.model == model_id]
    if candidates and candidates[0].context_window:
        return candidates[0].context_window
    return default


def needs_compaction(messages: list, context_window: int, threshold: float) -> bool:
    est = sum(estimate_tokens(str(m.get("content", ""))) + estimate_tokens(str(m.get("tool_calls", "")))
              for m in messages)
    return est > int(context_window * threshold)


def compact_messages(messages: list, complete_fn, keep_tail: int = 12) -> tuple[list, str]:
    """Summarize the head of the conversation, keep the tail verbatim.

    complete_fn(messages) -> str must produce the summary (provider call).
    Returns (new_messages, summary_text).
    """
    if len(messages) <= keep_tail + 2:
        return messages, ""
    head = messages[:-keep_tail]
    tail = messages[-keep_tail:]
    # find the last user message in head to anchor the summary request
    transcript_parts = []
    for m in head:
        role = m.get("role", "?")
        content = m.get("content")
        if isinstance(content, list):
            content = str(content)
        if m.get("tool_calls"):
            calls = ", ".join(tc.get("name", "?") for tc in m["tool_calls"])
            transcript_parts.append(f"[{role}] (tool calls: {calls}) {content or ''}")
        else:
            transcript_parts.append(f"[{role}] {content or ''}")
        if role == "tool":
            transcript_parts[-1] = f"[tool] {truncate_middle(str(content), 1200)}"
    transcript = "\n".join(transcript_parts)[-100_000:]
    summary = complete_fn([
        {"role": "user", "content": SUMMARY_PROMPT + "\n\n<conversation>\n" + transcript +
         "\n</conversation>"}])
    summary_msg = {"role": "user",
                   "content": f"<compaction-summary>\n{summary}\n</compaction-summary>\n\n"
                              f"Continue from this summary. The messages above the summary "
                              f"were compressed away; the tail below is verbatim."}
    return [summary_msg] + tail, summary
