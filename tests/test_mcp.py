"""Tests: MCP client (JSON-RPC framing against a fake server process)."""

import json
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

from marfim.mcp.client import McpError, StdioMcpClient

FAKE_SERVER = textwrap.dedent("""
    import json, sys
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        msg = json.loads(line)
        if msg.get("method") == "initialize":
            resp = {"jsonrpc": "2.0", "id": msg["id"], "result": {
                "protocolVersion": "2024-11-05",
                "serverInfo": {"name": "fake-mcp", "version": "1.0"}}}
        elif msg.get("method") == "tools/list":
            resp = {"jsonrpc": "2.0", "id": msg["id"], "result": {"tools": [
                {"name": "echo", "description": "Echo text",
                 "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}},
                {"name": "boom", "description": "Always errors",
                 "inputSchema": {"type": "object", "properties": {}}}]}}
        elif msg.get("method") == "tools/call":
            name = msg["params"]["name"]
            if name == "boom":
                resp = {"jsonrpc": "2.0", "id": msg["id"], "result": {
                    "isError": True, "content": [{"type": "text", "text": "kaboom"}]}}
            else:
                resp = {"jsonrpc": "2.0", "id": msg["id"], "result": {
                    "content": [{"type": "text",
                                 "text": "echo: " + msg["params"]["arguments"].get("text", "")}]}}
        elif "id" in msg:
            resp = {"jsonrpc": "2.0", "id": msg["id"], "result": {}}
        else:
            continue
        sys.stdout.write(json.dumps(resp) + "\\n")
        sys.stdout.flush()
""")


class McpClientTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.NamedTemporaryFile("w", suffix=".py", delete=False)
        self.tmp.write(FAKE_SERVER)
        self.tmp.close()

    def tearDown(self):
        Path(self.tmp.name).unlink(missing_ok=True)

    def _client(self) -> StdioMcpClient:
        client = StdioMcpClient("fake", sys.executable, [self.tmp.name], timeout=10)
        client.start()
        self.addCleanup(client.stop)
        return client

    def test_handshake_and_tools(self):
        client = self._client()
        self.assertEqual(client.server_info.get("name"), "fake-mcp")
        tools = client.list_tools()
        self.assertEqual([t["name"] for t in tools], ["echo", "boom"])

    def test_call_tool(self):
        client = self._client()
        client.list_tools()
        out = client.call_tool("echo", {"text": "hello marfim"})
        self.assertEqual(out, "echo: hello marfim")

    def test_tool_error_surfaces(self):
        client = self._client()
        client.list_tools()
        with self.assertRaises(McpError):
            client.call_tool("boom", {})


if __name__ == "__main__":
    unittest.main()
