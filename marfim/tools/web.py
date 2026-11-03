"""Web tools: webfetch (URL -> text) and websearch (DuckDuckGo HTML scraping).

ECC research-first doctrine: the system prompt steers the agent to fetch real
sources before writing new code. Results are bounded, summary-first (REA).

Review finding 34: an SSRF guard — every hop resolves the host and rejects
private/loopback/link-local/metadata ranges (169.254.169.254 included),
redirects are followed manually so EACH hop is re-checked, non-http(s)
schemes and embedded credentials are refused.
"""

from __future__ import annotations

import html as html_mod
import ipaddress
import re
import socket
import urllib.error
import urllib.request
from urllib.parse import quote_plus, urlparse

from marfim.tools.base import Tool, ToolCtx, ToolResult
from marfim.util import truncate_middle

UA = "Mozilla/5.0 (compatible; MarfimAgent/0.1; +https://github.com/)"
BLOCKED_SCHEMES = {"file", "ftp", "gopher", "unix", "data"}
MAX_REDIRECTS = 5


class SSRFBlocked(ValueError):
    pass


def _assert_public_host(url: str) -> None:
    """Resolve the host and refuse private/loopback/link-local targets."""
    parsed = urlparse(url)
    if parsed.scheme not in ("http", "https"):
        raise SSRFBlocked(f"scheme not allowed: {parsed.scheme or '(none)'}")
    if parsed.username or parsed.password:
        raise SSRFBlocked("URLs with embedded credentials are not allowed")
    host = parsed.hostname or ""
    if not host:
        raise SSRFBlocked("URL has no host")
    # literal IPs (incl. the cloud-metadata classic 169.254.169.254)
    try:
        addrs = [ipaddress.ip_address(host)]
    except ValueError:
        try:
            infos = socket.getaddrinfo(host, parsed.port or (443 if parsed.scheme == "https" else 80),
                                       proto=socket.IPPROTO_TCP)
        except socket.gaierror as exc:
            raise SSRFBlocked(f"cannot resolve host {host}: {exc}") from exc
        addrs = []
        for info in infos:
            try:
                addrs.append(ipaddress.ip_address(info[4][0]))
            except ValueError:
                continue
    for addr in addrs:
        if (addr.is_private or addr.is_loopback or addr.is_link_local
                or addr.is_reserved or addr.is_multicast or addr.is_unspecified):
            raise SSRFBlocked(
                f"refusing to fetch {url}: host resolves to a non-public address "
                f"({addr}) — SSRF guard (metadata endpoints included)")


def _http_get(url: str, timeout: int = 20) -> str:
    """GET with a manual redirect loop: EVERY hop passes the SSRF guard."""
    current = url
    for _hop in range(MAX_REDIRECTS + 1):
        _assert_public_host(current)
        parsed = urlparse(current)
        if parsed.scheme not in ("http", "https"):
            raise SSRFBlocked(f"scheme not allowed: {parsed.scheme or '(none)'}")
        req = urllib.request.Request(current, headers={"User-Agent": UA,
                                                       "Accept": "text/html,*/*"})
        opener = urllib.request.build_opener(_NoRedirect())
        try:
            with opener.open(req, timeout=timeout) as resp:
                charset = resp.headers.get_content_charset() or "utf-8"
                return resp.read(2_000_000).decode(charset, "replace")
        except urllib.error.HTTPError as exc:
            location = exc.headers.get("Location") if exc.headers else None
            if exc.code in (301, 302, 303, 307, 308) and location:
                from urllib.parse import urljoin
                current = urljoin(current, location)
                continue
            raise
    raise SSRFBlocked(f"too many redirects (> {MAX_REDIRECTS})")


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):  # noqa: ANN002, ANN003
        return None


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
    except SSRFBlocked as exc:
        return ToolResult(f"error: blocked: {exc}", is_error=True)
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
    except SSRFBlocked as exc:
        return ToolResult(f"error: blocked: {exc}", is_error=True)
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
                          "Private/metadata addresses are blocked (SSRF guard). "
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
