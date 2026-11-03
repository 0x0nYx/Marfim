"""Wire protocol tests — drive the real SSE parsers against a local canned
HTTP server (no network, no keys)."""

import json
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from marfim.config import ProviderConfig
from marfim.providers.anthropic import AnthropicProvider
from marfim.providers.openai_compat import OpenAICompatProvider

OPENAI_SSE = [
    'data: {"choices":[{"delta":{"role":"assistant","content":"Hel"}}]}',
    'data: {"choices":[{"delta":{"content":"lo"}}]}',
    'data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_9","function":{"name":"read","arguments":"{\\"file_"}}]}}]}',
    'data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"path\\": \\"x.py\\"}"}}]}}]}',
    'data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":11,"completion_tokens":7}}',
    'data: [DONE]',
]

ANTHROPIC_SSE = [
    'event: message_start\ndata: {"type":"message_start","message":{"usage":{"input_tokens":21,"cache_read_input_tokens":5}}}',
    'event: content_block_start\ndata: {"type":"content_block_start","index":0,"content_block":{"type":"text"}}',
    'event: content_block_delta\ndata: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Bonjour"}}',
    'event: content_block_stop\ndata: {"type":"content_block_stop","index":0}',
    'event: content_block_start\ndata: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"tu_1","name":"grep"}}',
    'event: content_block_delta\ndata: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\\"pattern\\": "}}',
    'event: content_block_delta\ndata: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"\\"x\\"}"}}',
    'event: content_block_stop\ndata: {"type":"content_block_stop","index":1}',
    'event: message_delta\ndata: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":9}}',
    'event: message_stop\ndata: {"type":"message_stop"}',
]


class CannedHandler(BaseHTTPRequestHandler):
    body = OPENAI_SSE

    def do_POST(self):
        payload = "".join(line + "\n\n" for line in self.body).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, *_args):
        pass


class WireTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = ThreadingHTTPServer(("127.0.0.1", 0), CannedHandler)
        cls.port = cls.server.server_address[1]
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()

    def test_openai_compat_stream_and_tools(self):
        provider = OpenAICompatProvider(
            ProviderConfig(name="t", base_url=f"http://127.0.0.1:{self.port}",
                           wire="openai", api_key="k"), "test-model")
        events = list(provider.stream([{"role": "user", "content": "hi"}], [], system="sys"))
        text = "".join(e.get("text", "") for e in events if e["type"] == "text_delta")
        self.assertEqual(text, "Hello")
        calls = [e for e in events if e["type"] == "tool_call"]
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0]["name"], "read")
        self.assertEqual(json.loads(calls[0]["arguments"]), {"file_path": "x.py"})
        done = events[-1]
        self.assertEqual(done["usage"].input_tokens, 11)
        self.assertEqual(done["usage"].output_tokens, 7)
        self.assertEqual(done["stop_reason"], "tool_calls")

    def test_anthropic_stream_and_tools(self):
        CannedHandler.body = ANTHROPIC_SSE
        try:
            provider = AnthropicProvider(
                ProviderConfig(name="t", base_url=f"http://127.0.0.1:{self.port}",
                               wire="anthropic", api_key="k"), "test-model")
            events = list(provider.stream([{"role": "user", "content": "hi"}], [],
                                          system="sys"))
        finally:
            CannedHandler.body = OPENAI_SSE
        text = "".join(e.get("text", "") for e in events if e["type"] == "text_delta")
        self.assertEqual(text, "Bonjour")
        calls = [e for e in events if e["type"] == "tool_call"]
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0]["name"], "grep")
        self.assertEqual(json.loads(calls[0]["arguments"]), {"pattern": "x"})
        done = events[-1]
        self.assertEqual(done["usage"].input_tokens, 21)
        self.assertEqual(done["usage"].cached_tokens, 5)
        self.assertEqual(done["usage"].output_tokens, 9)
        self.assertEqual(done["stop_reason"], "tool_use")


if __name__ == "__main__":
    unittest.main()
