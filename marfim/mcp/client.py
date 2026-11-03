"""MCP client — Model Context Protocol (JSON-RPC 2.0) over stdio AND
streamable-HTTP (review §4.2: MCP as the universal adapter).

Claude Code/Goose/Oh My Pi doctrine: any MCP server becomes first-class tools.
Handshake: initialize -> notifications/initialized -> tools/list -> tools/call.

Review finding 32 fixes:
- server-initiated requests (ping, roots/list) are answered, not mistaken for
  responses; unknown server requests get a method-not-found error;
- stderr is drained in a thread (a chatty server can no longer deadlock the
  handshake);
- a dead server is marked dead on EOF and pending calls fail fast;
- tools/list pagination (nextCursor) is followed; tools/list_changed clears
  the cache;
- per-call timeouts; parallel startup; scrubbed child environment;
- structuredContent is preserved.
"""

from __future__ import annotations

import json
import os
import subprocess
import threading
import queue
import urllib.error
import urllib.request
from collections import deque
from pathlib import Path

from marfim.util import gen_id, strict_secret_env

PROTOCOL_VERSION = "2024-11-05"


class McpError(Exception):
    pass


def _env_expand(value: str) -> str:
    """Expand ${VAR} in configured values against the real environment."""
    import re
    return re.sub(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}",
                  lambda m: os.environ.get(m.group(1), ""), value)


class BaseMcpClient:
    """Shared tool-listing/call logic for both transports."""

    name = "base"

    def __init__(self, name: str, timeout: float = 30.0):
        self.name = name
        self.timeout = timeout
        self.server_info: dict = {}
        self._tools: list[dict] = []
        self._tools_lock = threading.Lock()
        self._alive = False

    # ------------------------------------------------------------- overrides

    def _send_request(self, method: str, params: dict, timeout: float) -> dict:
        raise NotImplementedError

    def _send_notification(self, method: str, params: dict) -> None:
        raise NotImplementedError

    # ------------------------------------------------------------------- api

    def start(self) -> None:  # pragma: no cover - transport specific
        raise NotImplementedError

    def stop(self) -> None:  # pragma: no cover - transport specific
        raise NotImplementedError

    def list_tools(self, force: bool = False) -> list[dict]:
        with self._tools_lock:
            cached = self._tools
        if cached and not force:
            return cached
        tools: list[dict] = []
        cursor: str | None = None
        seen_cursors: set = set()   # a hostile/repeating nextCursor must not loop forever
        while True:   # follow nextCursor pagination (finding 32)
            params: dict = {}
            if cursor:
                params["cursor"] = cursor
            result = self._send_request("tools/list", params, self.timeout)
            tools += [t for t in result.get("tools", []) if isinstance(t, dict)]
            cursor = result.get("nextCursor")
            if not cursor or cursor in seen_cursors:
                break
            seen_cursors.add(cursor)
        with self._tools_lock:
            self._tools = tools
        return tools

    def call_tool(self, tool_name: str, arguments: dict, timeout: float | None = None) -> str:
        result = self._send_request("tools/call",
                                    {"name": tool_name, "arguments": arguments},
                                    timeout or max(self.timeout, 120.0))
        if result.get("isError"):
            content = result.get("content", [])
            text = " ".join(c.get("text", "") for c in content if isinstance(c, dict))
            raise McpError(text or "MCP tool error")
        content = result.get("content", [])
        parts = []
        for c in content:
            if not isinstance(c, dict):
                continue
            if c.get("type") == "text":
                parts.append(c.get("text", ""))
            else:
                parts.append(f"[{c.get('type')}]")
        out = "\n".join(parts) or "(empty MCP result)"
        # structuredContent is preserved (finding 32: it was dropped)
        structured = result.get("structuredContent")
        if structured is not None:
            out += "\n[structured] " + json.dumps(structured, ensure_ascii=False, default=str)[:4000]
        return out


class StdioMcpClient(BaseMcpClient):
    """One JSON-RPC connection to an MCP server subprocess."""

    def __init__(self, name: str, command: str, args: list[str] | None = None,
                 env: dict | None = None, cwd: Path | None = None, timeout: float = 30.0):
        super().__init__(name, timeout)
        self.command = command
        self.args = args or []
        self.env_extra = env or {}
        self.cwd = cwd
        self.proc: subprocess.Popen | None = None
        self.stderr_tail: deque = deque(maxlen=50)
        self._pending: dict[int | str, queue.Queue] = {}
        self._pending_lock = threading.Lock()
        self._lock = threading.Lock()
        self._reader: threading.Thread | None = None
        self._stderr: threading.Thread | None = None

    # ------------------------------------------------------------- lifecycle

    def start(self) -> None:
        if self._alive:
            return
        env = strict_secret_env()
        env.update({k: _env_expand(str(v)) for k, v in self.env_extra.items()})
        try:
            self.proc = subprocess.Popen(
                [self.command, *self.args], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                stderr=subprocess.PIPE, text=True, bufsize=1, env=env,
                cwd=str(self.cwd) if self.cwd else None)
        except (OSError, FileNotFoundError) as exc:
            raise McpError(f"cannot start MCP server '{self.name}': {exc}") from exc
        self._alive = True
        self._reader = threading.Thread(target=self._read_loop, daemon=True,
                                        name=f"mcp:{self.name}:read")
        self._reader.start()
        self._stderr = threading.Thread(target=self._drain_stderr, daemon=True,
                                        name=f"mcp:{self.name}:stderr")
        self._stderr.start()
        try:
            result = self.request("initialize", {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "marfim", "version": "0.2.0"},
            })
        except McpError:
            self._alive = False
            tail = " ".join(self.stderr_tail)[-300:]
            raise McpError(f"MCP server '{self.name}' failed to initialize"
                           + (f"; stderr: {tail}" if tail else "")) from None
        self.server_info = result.get("serverInfo", {}) or {}
        self.notify("notifications/initialized", {})

    def stop(self) -> None:
        self._alive = False
        if self.proc is not None:
            try:
                if self.proc.stdin is not None:
                    self.proc.stdin.close()
                if self.proc.stdout is not None:
                    self.proc.stdout.close()
                if self.proc.stderr is not None:
                    self.proc.stderr.close()
            except Exception:
                pass
            try:
                self.proc.terminate()
                self.proc.wait(timeout=5)
            except Exception:
                try:
                    self.proc.kill()
                except Exception:
                    pass
            self.proc = None

    # ------------------------------------------------------------------ rpc

    def _next_id(self) -> int:
        with self._lock:
            self._counter = getattr(self, "_counter", 0) + 1
            return self._counter

    def _drain_stderr(self) -> None:
        """stderr is drained in a thread (finding 32: 300KB of stderr before
        the initialize reply used to deadlock the handshake)."""
        assert self.proc is not None and self.proc.stderr is not None
        try:
            for line in self.proc.stderr:
                self.stderr_tail.append(line.rstrip()[:300])
        except (ValueError, OSError):
            pass

    def _fail_pending(self, message: str) -> None:
        with self._pending_lock:
            pending = list(self._pending.items())
            self._pending.clear()
        for _rid, q in pending:
            q.put({"__dead__": True, "error": {"message": message}})

    def _deliver(self, rid, msg: dict) -> None:
        """Deliver a response; the reader thread must NEVER block here — a
        timed-out or duplicated response id is dropped instead (finding 32)."""
        with self._pending_lock:
            q = self._pending.get(rid)
        if q is not None:
            try:
                q.put_nowait(msg)
            except queue.Full:
                pass

    def _read_loop(self) -> None:
        assert self.proc is not None and self.proc.stdout is not None
        try:
            for line in self.proc.stdout:
                line = line.strip()
                if not line:
                    continue
                try:
                    msg = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if not isinstance(msg, dict):   # non-dict message: skip (finding 32)
                    continue
                self._handle_message(msg)
        finally:
            if self._alive:
                # EOF while we still think we are connected: the server died
                self._alive = False
            self._fail_pending(f"MCP server '{self.name}' closed the connection")

    def _handle_message(self, msg: dict) -> None:
        rid = msg.get("id")
        if "method" in msg:
            # a server->client REQUEST: answer it, never treat it as a response
            self._answer_server_request(msg)
            return
        if rid is None:
            # a notification
            if msg.get("method") == "notifications/tools/list_changed":
                with self._tools_lock:
                    self._tools = []
            return
        with self._pending_lock:
            q = self._pending.get(rid)
        if q is not None:
            self._deliver(rid, msg)
        # unmatched response ids are dropped (the request already timed out)

    def _answer_server_request(self, msg: dict) -> None:
        method = msg.get("method", "")
        rid = msg.get("id")
        if method == "ping":
            result: dict = {}
        elif method == "roots/list":
            result = {"roots": []}
        else:
            self._send_raw({"jsonrpc": "2.0", "id": rid,
                            "error": {"code": -32601, "message": "method not found"}})
            return
        self._send_raw({"jsonrpc": "2.0", "id": rid, "result": result})

    def _send_raw(self, msg: dict) -> None:
        if self.proc is None or self.proc.stdin is None:
            return
        try:
            self.proc.stdin.write(json.dumps(msg) + "\n")
            self.proc.stdin.flush()
        except (BrokenPipeError, OSError):
            self._alive = False

    def request(self, method: str, params: dict, timeout: float | None = None) -> dict:
        if not self._alive:
            raise McpError(f"MCP server '{self.name}' is not running")
        rid = self._next_id()
        q: queue.Queue = queue.Queue(maxsize=1)
        with self._pending_lock:
            self._pending[rid] = q
        msg = {"jsonrpc": "2.0", "id": rid, "method": method, "params": params}
        try:
            self._send(msg)
            try:
                resp = q.get(timeout=timeout or self.timeout)
            except queue.Empty as exc:
                raise McpError(f"MCP server '{self.name}' timed out on {method}") from exc
        finally:
            with self._pending_lock:
                self._pending.pop(rid, None)
        if resp.get("__dead__"):
            raise McpError(f"MCP server '{self.name}': "
                           f"{resp.get('error', {}).get('message', 'connection closed')}")
        if "error" in resp:
            err = resp["error"]
            raise McpError(f"MCP error on {method}: {err.get('message', err)}")
        return resp.get("result", {})

    def notify(self, method: str, params: dict) -> None:
        self._send({"jsonrpc": "2.0", "method": method, "params": params})

    def _send(self, msg: dict) -> None:
        if self.proc is None or self.proc.stdin is None:
            raise McpError(f"MCP server '{self.name}' has no stdin")
        try:
            self.proc.stdin.write(json.dumps(msg) + "\n")
            self.proc.stdin.flush()
        except (BrokenPipeError, OSError) as exc:
            self._alive = False
            raise McpError(f"MCP server '{self.name}' died: {exc}") from exc

    def _send_request(self, method: str, params: dict, timeout: float) -> dict:
        return self.request(method, params, timeout=timeout)

    def _send_notification(self, method: str, params: dict) -> None:
        self.notify(method, params)


class HttpMcpClient(BaseMcpClient):
    """Streamable-HTTP transport (MCP 2025-03-26): POST JSON-RPC to a single
    endpoint; responses come back as JSON or as an SSE stream; a session id
    header is echoed when the server assigns one.  Auth headers come from
    config with ${ENV_VAR} expansion (full OAuth flows are documented future
    work)."""

    def __init__(self, name: str, url: str, headers: dict | None = None,
                 env: dict | None = None, timeout: float = 30.0):
        super().__init__(name, timeout)
        self.url = url
        self.extra_headers = {k: _env_expand(str(v)) for k, v in (headers or {}).items()}
        self.env_extra = env or {}
        self.session_id: str | None = None
        self._id_counter = 0
        self._id_lock = threading.Lock()

    def start(self) -> None:
        resp = self._post({"jsonrpc": "2.0", "id": self._next_id(),
                           "method": "initialize",
                           "params": {"protocolVersion": PROTOCOL_VERSION,
                                      "capabilities": {},
                                      "clientInfo": {"name": "marfim", "version": "0.2.0"}}})
        result = (resp or {}).get("result", {}) if isinstance(resp, dict) else {}
        self.server_info = result.get("serverInfo", {}) or {}
        self._alive = True
        self._post({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}})

    def stop(self) -> None:
        self._alive = False

    def _next_id(self) -> int:
        with self._id_lock:
            self._id_counter += 1
            return self._id_counter

    def _headers(self) -> dict:
        headers = {
            "Content-Type": "application/json",
            "Accept": "application/json, text/event-stream",
            "User-Agent": "marfim/0.2",
            **self.extra_headers,
        }
        if self.session_id:
            headers["Mcp-Session-Id"] = self.session_id
        return headers

    def _post(self, payload: dict, timeout: float | None = None) -> dict | None:
        body = json.dumps(payload).encode("utf-8")
        req = urllib.request.Request(self.url, data=body, headers=self._headers(),
                                     method="POST")
        try:
            resp = urllib.request.urlopen(req, timeout=timeout or self.timeout)
        except urllib.error.HTTPError as exc:
            detail = ""
            try:
                detail = exc.read().decode("utf-8", "replace")[:300]
            except Exception:
                pass
            raise McpError(f"MCP HTTP error {exc.code} on '{self.name}': {detail}") from exc
        except (urllib.error.URLError, TimeoutError, OSError) as exc:
            raise McpError(f"MCP HTTP transport error on '{self.name}': {exc}") from exc
        with resp:
            sid = resp.headers.get("Mcp-Session-Id")
            if sid:
                self.session_id = sid
            ctype = (resp.headers.get("Content-Type") or "").lower()
            raw = resp.read(4_000_000).decode("utf-8", "replace")
        if "text/event-stream" in ctype:
            return self._parse_sse(raw)
        if not raw.strip():
            return None
        try:
            return json.loads(raw)
        except json.JSONDecodeError:
            return None

    @staticmethod
    def _parse_sse(raw: str) -> dict | None:
        """Extract the first JSON-RPC response (id present) from an SSE body."""
        for block in raw.split("\n\n"):
            data_lines = [l[5:].strip() for l in block.splitlines()
                          if l.startswith("data:")]
            if not data_lines:
                continue
            try:
                msg = json.loads("".join(data_lines))
            except json.JSONDecodeError:
                continue
            if isinstance(msg, dict) and ("id" in msg or "error" in msg):
                return msg
        return None

    def _send_request(self, method: str, params: dict, timeout: float) -> dict:
        if not self._alive and method != "initialize":
            raise McpError(f"MCP server '{self.name}' is not started")
        resp = self._post({"jsonrpc": "2.0", "id": self._next_id(),
                           "method": method, "params": params}, timeout=timeout)
        if resp is None:
            raise McpError(f"MCP server '{self.name}' returned an empty response")
        if "error" in resp:
            err = resp["error"]
            raise McpError(f"MCP error on {method}: {err.get('message', err)}")
        return resp.get("result", {})

    def _send_notification(self, method: str, params: dict) -> None:
        try:
            self._post({"jsonrpc": "2.0", "method": method, "params": params})
        except McpError:
            pass


class McpManager:
    """Owns all configured MCP servers; exposes prefixed tools mcp__<srv>__<tool>."""

    def __init__(self):
        self.clients: dict[str, BaseMcpClient] = {}

    def register(self, name: str, command: str = "", args: list[str] | None = None,
                 env: dict | None = None, cwd: Path | None = None,
                 url: str = "", headers: dict | None = None) -> BaseMcpClient:
        if url:
            client: BaseMcpClient = HttpMcpClient(name, url, headers, env)
        else:
            client = StdioMcpClient(name, command, args, env, cwd)
        self.clients[name] = client
        return client

    def start_all(self, cwd: Path | None = None) -> dict[str, str]:
        """Returns {server: status}; failures recorded, never fatal.
        Startup is PARALLEL (finding 32: serial blocking startup delayed the
        REPL by the sum of all server handshakes)."""
        status: dict[str, str] = {}
        lock = threading.Lock()

        def _start(name: str, client: BaseMcpClient) -> None:
            try:
                client.start()
                count = len(client.list_tools())
                result = f"ok ({count} tools)"
            except McpError as exc:
                result = f"failed: {exc}"
            with lock:
                status[name] = result

        threads = [threading.Thread(target=_start, args=(n, c), daemon=True)
                   for n, c in self.clients.items()]
        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=60)
        return status

    def stop_all(self) -> None:
        for client in self.clients.values():
            client.stop()

    def all_tools(self) -> list[dict]:
        out = []
        for name, client in self.clients.items():
            if not client._alive:
                continue
            try:
                for tool in client.list_tools():
                    out.append({"server": name, "tool": tool})
            except McpError:
                continue
        return out


def sanitize_tool_name(server: str, tool: str) -> str:
    """Provider-safe tool name: ^[a-zA-Z0-9_-]{1,64}$ (OpenAI limit) with a
    digest suffix for overlong names (finding 32: names were not sanitized)."""
    import re
    from marfim.util import sha_short
    raw = f"mcp__{server}__{tool}"
    cleaned = re.sub(r"[^A-Za-z0-9_-]", "_", raw)
    if len(cleaned) > 64:
        cleaned = cleaned[:56] + "_" + sha_short(raw, 6)
    return cleaned
