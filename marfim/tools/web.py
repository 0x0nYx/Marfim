"""Web tools: webfetch (URL -> text) and websearch (DuckDuckGo HTML scraping).

ECC research-first doctrine: the system prompt steers the agent to fetch real
sources before writing new code. Results are bounded, summary-first (REA).
"""

from __future__ import annotations

import html as html_mod
import re
import urllib.request
from urllib.parse import quote_plus, urlparse

from marfim.tools.base import Tool, ToolCtx, ToolResult
from marfim.util import truncate_middle

UA = "Mozilla/5.0 (compatible; MarfimAgent/0.1; +https://github.com/)"
BLOCKED_SCHEMES = {"file", "ftp", "gopher"}


def _http_get(url: str, timeout: int = 20) -> str:
    parsed = urlparse(url)
    if parsed.scheme not in ("http", "https"):
        raise ValueError(f"scheme not allowed: {parsed.scheme or '(none)'}")
    req = urllib.request.Request(url, headers={"User-Agent": UA, "Accept": "text/html,*/*"})
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        charset = resp.headers.get_content_charset() or "utf-8"
        return resp.read(2_000_000).decode(charset, "replace")


def html_to_text(raw: str) -> str:
    raw = re.sub(r"(?is)<(script|style|noscript|svg|template).*?>.*?</\1>", "", raw)
    raw = re.sub(r"(?i)<br\s*/?>", "\n", raw)
    raw = re.sub(r"(?i)</(p|div|li|h[1-6]|tr)>", "\n", raw)
    raw = re.sub(r"(?is)<title>(.*?)</title>", r"\n# TITLE: \1\n", raw)
    text = re.sub(r"<[^>]+>", " ", raw)
    text = html_mod.unescape(text)
    text = re.sub(r"[ \t]+", " ", text)
    text = re.sub(r"\n\s*\n+", "\n\n", text)
    return text.strip()


def _do_webfetch(args: dict, ctx: ToolCtx) -> ToolResult:
    url = str(args.get("url") or "")
    max_chars = int(args.get("max_chars") or 20_000)
    if not url:
        return ToolResult("error: url is required", is_error=True)
    try:
        raw = _http_get(url)
    except Exception as exc:  # noqa: BLE001
        return ToolResult(f"error: fetch failed: {exc}", is_error=True)
    ctype_is_html = "html" in raw[:200].lower() or "<html" in raw[:1000].lower()
    text = html_to_text(raw) if ctype_is_html else raw
    return ToolResult(truncate_middle(text, max_chars))


def _do_websearch(args: dict, ctx: ToolCtx) -> ToolResult:
    query = str(args.get("query") or "")
    if not query:
        return ToolResult("error: query is required", is_error=True)
    try:
        raw = _http_get("https://duckduckgo.com/html/?q=" + quote_plus(query))
    except Exception as exc:  # noqa: BLE001
        return ToolResult(f"error: search failed: {exc}", is_error=True)
    results: list[str] = []
    for m in re.finditer(
            r'<a[^>]+class="result__a"[^>]*href="([^"]+)"[^>]*>(.*?)</a>', raw, re.S):
        url = m.group(1)
        # DDG wraps URLs in a redirect: //duckduckgo.com/l/?uddg=<encoded>
        inner = re.search(r"uddg=([^&]+)", url)
        if inner:
            from urllib.parse import unquote
            url = unquote(inner.group(1))
        title = html_mod.unescape(re.sub(r"<[^>]+>", "", m.group(2))).strip()
        results.append(f"- {title}\n  {url}")
        if len(results) >= 8:
            break
    if not results:
        return ToolResult("(no results parsed — the search page format may have "
                          "changed; use webfetch on a search engine URL instead)")
    return ToolResult("\n".join(results))


def build_tools() -> list[Tool]:
    return [
        Tool(name="webfetch",
             description=("Fetch a URL and return readable text (HTML is stripped). "
                          "Params: url (required), max_chars."),
             parameters={"type": "object", "properties": {
                 "url": {"type": "string"}, "max_chars": {"type": "integer"}},
                 "required": ["url"]},
             readonly=True, handler=_do_webfetch),
        Tool(name="websearch",
             description=("Web search returning title+URL results. Params: query."),
             parameters={"type": "object", "properties": {"query": {"type": "string"}},
                         "required": ["query"]},
             readonly=True, handler=_do_websearch),
    ]
