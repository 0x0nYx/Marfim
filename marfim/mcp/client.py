"""MCP client — Model Context Protocol over stdio (JSON-RPC 2.0).

Claude Code/Goose/Oh My Pi doctrine: any MCP server becomes first-class tools.
Handshake: initialize -> notifications/initialized -> tools/list -> tools/call.
"""

from __future__ import annotations

import json
import os
import subprocess
import threading
import queue
from pathlib import Path

from marfim.util import gen_id

PROTOCOL_VERSION = "2024-11-05"


class McpError(Exception):
    pass


class StdioMcpClient:
    """One JSON-RPC connection to an MCP server subprocess."""

    def __init__(self, name: str, command: str, args: list[str] | None = None,
                 env: dict | None = None, cwd: Path | None = None, timeout: float = 30.0):
        self.name = name
        self.command = command
        self.args = args or []
        self.env_extra = env or {}
        self.cwd = cwd
        self.timeout = timeout
        self.proc: subprocess.Popen | None = None
        self._pending: dict[int | str, queue.Queue] = {}
        self._lock = threading.Lock()
        self._reader: threading.Thread | None = None
        self.server_info: dict = {}
        self._tools: list[dict] = []
        self._alive = False

    # ------------------------------------------------------------- lifecycle

    def start(self) -> None:
        if self._alive:
            return
        env = {**os.environ, **self.env_extra}
        try:
            self.proc = subprocess.Popen(
                [self.command, *self.args], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                stderr=subprocess.PIPE, text=True, bufsize=1, env=env,
                cwd=str(self.cwd) if self.cwd else None)
        except (OSError, FileNotFoundError) as exc:
            raise McpError(f"cannot start MCP server '{self.name}': {exc}") from exc
        self._alive = True
        self._reader = threading.Thread(target=self._read_loop, daemon=True)
        self._reader.start()
        result = self.request("initialize", {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": "marfim", "version": "0.1.0"},
        })
        self.server_info = result.get("serverInfo", {})
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

    def _read_loop(self) -> None:
        assert self.proc is not None and self.proc.stdout is not None
        for line in self.proc.stdout:
            line = line.strip()
            if not line:
                continue
            try:
                msg = json.loads(line)
            except json.JSONDecodeError:
                continue
            rid = msg.get("id")
            if rid is not None and rid in self._pending:
                self._pending[rid].put(msg)
            # notifications (e.g. tools/list_changed) are currently ignored

    def request(self, method: str, params: dict) -> dict:
        if not self._alive:
            raise McpError(f"MCP server '{self.name}' is not running")
        rid = self._next_id()
        q: queue.Queue = queue.Queue(maxsize=1)
        self._pending[rid] = q
        msg = {"jsonrpc": "2.0", "id": rid, "method": method, "params": params}
        self._send(msg)
        try:
            resp = q.get(timeout=self.timeout)
        except queue.Empty as exc:
            self._pending.pop(rid, None)
            raise McpError(f"MCP server '{self.name}' timed out on {method}") from exc
        finally:
            self._pending.pop(rid, None)
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

    # ---------------------------------------------------------------- tools

    def list_tools(self) -> list[dict]:
        if not self._tools:
            result = self.request("tools/list", {})
            self._tools = result.get("tools", [])
        return self._tools

    def call_tool(self, tool_name: str, arguments: dict) -> str:
        result = self.request("tools/call", {"name": tool_name, "arguments": arguments})
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
        return "\n".join(parts) or "(empty MCP result)"


class McpManager:
    """Owns all configured MCP servers; exposes prefixed tools mcp__<srv>__<tool>."""

    def __init__(self):
        self.clients: dict[str, StdioMcpClient] = {}

    def register(self, name: str, command: str, args: list[str] | None = None,
                 env: dict | None = None, cwd: Path | None = None) -> StdioMcpClient:
        client = StdioMcpClient(name, command, args, env, cwd)
        self.clients[name] = client
        return client

    def start_all(self, cwd: Path | None = None) -> dict[str, str]:
        """Returns {server: status}; failures are recorded, never fatal."""
        status: dict[str, str] = {}
        for name, client in self.clients.items():
            try:
                client.start()
                count = len(client.list_tools())
                status[name] = f"ok ({count} tools)"
            except McpError as exc:
                status[name] = f"failed: {exc}"
        return status

    def stop_all(self) -> None:
        for client in self.clients.values():
            client.stop()

    def all_tools(self) -> list[dict]:
        out = []
        for name, client in self.clients.items():
            if not client._alive:
                continue
            for tool in client.list_tools():
                out.append({"server": name, "tool": tool})
        return out
