"""Compaction — layered context management (merged doctrine).

- goose/opencode: threshold-triggered summarization keeping a verbatim tail.
- hermes: deterministic pruning of old large tool results first, then summary.
- codex: pre-sampling compact + manual /compact share one implementation.

Review finding 22: the tail always starts on a turn boundary — never on a
`tool` message, so a cut can never orphan tool results.
Review finding 23: context windows come from the provider-qualified model id
against a cached catalog; no network fetch happens inside the loop.
"""

from __future__ import annotations

from dataclasses import dataclass

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
    """Context window for `provider/model` or a bare model id.

    Uses the cached catalog only — never a network fetch inside the loop
    (review finding 23)."""
    try:
        from marfim.providers.registry import list_catalog_models
        provider, _, model_id = model.partition("/")
        candidates: list = []
        if provider and model_id:
            candidates = list_catalog_models(provider, fetch=False)
            candidates = [m for m in candidates if m.model == model_id]
        if not candidates:
            # bare model id: search every cached provider
            for m in list_catalog_models(fetch=False):
                if m.model == (model_id or model):
                    candidates = [m]
                    break
        if candidates and candidates[0].context_window:
            return candidates[0].context_window
    except Exception:
        pass
    return default


def needs_compaction(messages: list, context_window: int, threshold: float) -> bool:
    est = sum(estimate_tokens(str(m.get("content", ""))) + estimate_tokens(str(m.get("tool_calls", "")))
              for m in messages)
    return est > int(context_window * threshold)


def compact_messages(messages: list, complete_fn, keep_tail: int = 12) -> tuple[list, str]:
    """Summarize the head of the conversation, keep the tail verbatim.

    complete_fn(messages) -> str must produce the summary (provider call); it
    may raise ProviderError — callers must treat that as a FAILED compaction
    and keep the history (review finding 22).
    Returns (new_messages, summary_text).
    """
    if len(messages) <= keep_tail + 2:
        return messages, ""
    start = len(messages) - keep_tail
    # turn-boundary guarantee: the verbatim tail never begins on a tool
    # message (a cut there orphans the tool result from its tool_call)
    while start < len(messages) and messages[start].get("role") == "tool":
        start += 1
    head = messages[:start]
    tail = messages[start:]
    if not head:
        return messages, ""
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
    if not summary or not summary.strip() or summary.strip() == "(empty summary)":
        raise ValueError("compaction produced an empty summary — history kept")
    summary_msg = {"role": "user",
                   "content": f"<compaction-summary>\n{summary}\n</compaction-summary>\n\n"
                              f"Continue from this summary. The messages above the summary "
                              f"were compressed away; the tail below is verbatim."}
    return [summary_msg] + tail, summary
