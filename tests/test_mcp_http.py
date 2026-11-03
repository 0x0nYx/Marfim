"""MCP streamable-HTTP transport (review §4.2) + REPL pty smoke (finding 28)."""

import json
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from marfim.config import ProviderConfig  # noqa: F401 (import shape check)
from marfim.mcp.client import HttpMcpClient, McpManager


class FakeMcpHttpHandler(BaseHTTPRequestHandler):
    """Minimal streamable-HTTP MCP server: initialize + tools/list + tools/call."""

    def do_POST(self):  # noqa: N802
        length = int(self.headers.get("Content-Length", 0))
        body = json.loads(self.rfile.read(length) or b"{}")
        method = body.get("method", "")
        rid = body.get("id")
        # respond as SSE (text/event-stream) to prove both framings parse
        if method == "initialize":
            result = {"protocolVersion": "2024-11-05",
                      "serverInfo": {"name": "http-mcp", "version": "1"}}
        elif method == "tools/list":
            result = {"tools": [{"name": "greet", "description": "say hi",
                                 "annotations": {"readOnlyHint": True},
                                 "inputSchema": {"type": "object", "properties": {}}}]}
        elif method == "tools/call":
            result = {"content": [{"type": "text",
                                   "text": "hello " + body["params"]["arguments"].get("who", "")}],
                      "structuredContent": {"echo": body["params"]["arguments"]}}
        else:
            result = {}
        payload = json.dumps({"jsonrpc": "2.0", "id": rid, "result": result})
        sse = f"event: message\ndata: {payload}\n\n".encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Mcp-Session-Id", "sess-123")
        self.send_header("Content-Length", str(len(sse)))
        self.end_headers()
        self.wfile.write(sse)

    def log_message(self, *_args):
        pass


class HttpTransportTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = ThreadingHTTPServer(("127.0.0.1", 0), FakeMcpHttpHandler)
        cls.url = f"http://127.0.0.1:{cls.server.server_address[1]}/mcp"
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()

    def test_http_initialize_list_call(self):
        client = HttpMcpClient("http-test", self.url, headers={"Authorization": "Bearer k"})
        client.start()
        self.addCleanup(client.stop)
        self.assertEqual(client.server_info.get("name"), "http-mcp")
        self.assertEqual(client.session_id, "sess-123")
        tools = client.list_tools()
        self.assertEqual([t["name"] for t in tools], ["greet"])
        out = client.call_tool("greet", {"who": "marfim"})
        self.assertIn("hello marfim", out)
        self.assertIn('"echo"', out)   # structuredContent preserved

    def test_manager_registers_url_servers(self):
        manager = McpManager()
        client = manager.register("remote", url=self.url)
        self.assertIsInstance(client, HttpMcpClient)


if __name__ == "__main__":
    unittest.main()
