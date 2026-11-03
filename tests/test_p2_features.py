"""P2 regression tests — review findings 32, 33, 34 + §4 features."""

import json
import os
import subprocess
import sys
import tempfile
import textwrap
import threading
import time
import unittest
from pathlib import Path
from unittest import mock

from marfim.config import Config
from marfim.hooks import HookBlocked, HookRunner
from marfim.mcp.client import McpError, McpManager, StdioMcpClient, sanitize_tool_name
from marfim.permissions import Decision, Mode, PermissionEngine

# --------------------------------------------------------------------------- #
# finding 32: MCP protocol robustness
# --------------------------------------------------------------------------- #

BAD_SERVER = textwrap.dedent("""
    import json, sys, threading, time
    def spam():
        line = "-" * 200
        while True:
            sys.stderr.write(line + "\\n")
            sys.stderr.flush()
            time.sleep(0.005)
    threading.Thread(target=spam, daemon=True).start()
    counter = 1000
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        msg = json.loads(line)
        method = msg.get("method", "")
        # server-initiated request with a COLLIDING id (finding 32: the old
        # client treated any message with an id as the pending response)
        if method == "tools/list":
            cursor = (msg.get("params") or {}).get("cursor")
            sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": msg["id"],
                                         "method": "ping", "params": {}}) + "\\n")
            sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": msg["id"],
                                         "method": "roots/list", "params": {}}) + "\\n")
            if cursor == "page-2":
                page = {"tools": [{"name": "page2_tool", "description": "second page",
                                   "inputSchema": {"type": "object", "properties": {}}}]}
            else:
                page = {"tools": [{"name": "real_tool", "description": "the only real tool",
                                   "annotations": {"readOnlyHint": True},
                                   "inputSchema": {"type": "object", "properties": {}}}],
                        "nextCursor": "page-2"}
            sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": msg["id"],
                                         "result": page}) + "\\n")
            sys.stdout.flush()
        elif "id" in msg:
            sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": msg["id"],
                                         "result": {"serverInfo": {"name": "bad-mcp", "version": "1"}}}) + "\\n")
            sys.stdout.flush()
""")

DEAD_SERVER = textwrap.dedent("""
    import json, sys, os
    for line in sys.stdin:
        if '"initialize"' in line:
            sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": json.loads(line)["id"],
                                         "result": {"serverInfo": {"name": "dying"}}}) + "\\n")
            sys.stdout.flush()
            os._exit(0)   # die immediately after init
""")


LOOPING_CURSOR_SERVER = textwrap.dedent("""
    import json, sys
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        msg = json.loads(line)
        if msg.get("method") == "tools/list":
            # hostile: ALWAYS claims there is another page
            sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {
                "tools": [{"name": "tool", "description": "d",
                           "inputSchema": {"type": "object", "properties": {}}}],
                "nextCursor": "same"}}) + "\\n")
            sys.stdout.flush()
        elif "id" in msg:
            sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": msg["id"],
                                         "result": {"serverInfo": {"name": "looper"}}}) + "\\n")
            sys.stdout.flush()
""")


class McpProtocolTest(unittest.TestCase):
    def _client(self, script: str) -> StdioMcpClient:
        f = tempfile.NamedTemporaryFile("w", suffix=".py", delete=False)
        f.write(script)
        f.close()
        client = StdioMcpClient("t", sys.executable, [f.name], timeout=10)
        self.addCleanup(Path(f.name).unlink)
        self.addCleanup(client.stop)
        return client

    def test_server_requests_answered_and_pagination_followed(self):
        client = self._client(BAD_SERVER)
        client.start()
        # finding 32: colliding-id server requests must not poison the client
        self.assertEqual(client.server_info.get("name"), "bad-mcp")
        tools = client.list_tools()
        self.assertEqual([t["name"] for t in tools], ["real_tool", "page2_tool"])

    def test_repeating_cursor_terminates(self):
        """A hostile server that always sets nextCursor must not hang the loop."""
        client = self._client(LOOPING_CURSOR_SERVER)
        client.start()
        tools = client.list_tools()
        self.assertTrue(tools)   # we got pages and STOPPED
        self.assertTrue(all(t["name"] == "tool" for t in tools))

    def test_stderr_flood_does_not_deadlock(self):
        client = self._client(BAD_SERVER)
        client.start()   # the server spams stderr forever; init must return
        self.assertTrue(client._alive)

    def test_dead_server_marked_dead(self):
        client = self._client(DEAD_SERVER)
        client.start()
        time.sleep(0.3)   # server exits after initialize
        with self.assertRaises(McpError):
            client.list_tools()

    def test_tool_name_sanitized(self):
        long = "x" * 100
        name = sanitize_tool_name("srv", long)
        self.assertLessEqual(len(name), 64)
        weird = sanitize_tool_name("my server", "tool.name/with:symbols")
        self.assertEqual(weird, "mcp__my_server__tool_name_with_symbols")
        self.assertLessEqual(len(sanitize_tool_name("s", "t" * 200)), 64)

    def test_structured_content_preserved(self):
        # exercised through the fake server contract: results with
        # structuredContent keep it in the output
        from marfim.mcp.client import BaseMcpClient

        class Fake(BaseMcpClient):
            def _send_request(self, method, params, timeout):
                if method == "tools/call":
                    return {"content": [{"type": "text", "text": "plain"}],
                            "structuredContent": {"answer": 42}}
                return {}

        out = Fake("f").call_tool("x", {})
        self.assertIn("plain", out)
        self.assertIn('"answer": 42', out)


# --------------------------------------------------------------------------- #
# finding 33: hooks fail closed
# --------------------------------------------------------------------------- #

class HookFailClosedTest(unittest.TestCase):
    def _cfg_with(self, command: str) -> Config:
        with tempfile.TemporaryDirectory() as tmp:
            cfg = Config.load(project_root=Path(tmp))
        cfg.hooks["PreToolUse"] = [command]
        return cfg

    def test_timeout_denies(self):
        cfg = self._cfg_with("sleep 30")
        runner = HookRunner(cfg)
        with self.assertRaises(HookBlocked):
            runner.run("PreToolUse", {"tool": "bash", "input": {}}, fail_closed=True)

    def test_exit2_blocks(self):
        cfg = self._cfg_with("exit 2")
        runner = HookRunner(cfg)
        with self.assertRaises(HookBlocked):
            runner.run("PreToolUse", {"tool": "bash", "input": {}})

    def test_normal_passes(self):
        cfg = self._cfg_with("exit 0")
        HookRunner(cfg).run("PreToolUse", {"tool": "bash", "input": {}}, fail_closed=True)

    def test_scrubbed_env(self):
        script = 'test -z "$OPENAI_API_KEY" || exit 2'
        cfg = self._cfg_with(script)
        HookRunner(cfg).run("PreToolUse", {"tool": "bash", "input": {}}, fail_closed=True)


# --------------------------------------------------------------------------- #
# finding 34: SSRF guard
# --------------------------------------------------------------------------- #

class SSRFGuardTest(unittest.TestCase):
    def test_metadata_ip_blocked(self):
        from marfim.tools.web import SSRFBlocked, _assert_public_host
        for url in ("http://169.254.169.254/latest/meta-data/",
                    "http://127.0.0.1/x", "http://10.0.0.1/x",
                    "http://192.168.1.1/x", "http://[::1]/x",
                    "http://172.16.0.1/x", "file:///etc/passwd",
                    "http://user:pass@example.com/x"):
            with self.assertRaises(SSRFBlocked, msg=url):
                _assert_public_host(url)

    def test_public_host_passes_guard(self):
        from marfim.tools.web import _assert_public_host
        _assert_public_host("https://example.com/x")   # must not raise

    def test_localhost_hostname_blocked(self):
        from marfim.tools.web import SSRFBlocked, _assert_public_host
        with self.assertRaises(SSRFBlocked):
            _assert_public_host("http://localhost:8080/x")


# --------------------------------------------------------------------------- #
# §4.1 router
# --------------------------------------------------------------------------- #

class RouterTest(unittest.TestCase):
    def test_candidates_and_fallback(self):
        from marfim.router import Router
        with tempfile.TemporaryDirectory() as tmp:
            cfg = Config.load(project_root=Path(tmp))
            cfg.model_roles["smol"] = "  "
            r = Router(cfg)
            self.assertEqual(r.candidates("smol"), [("openai", "gpt-4.1-mini")])
            cfg.model_roles["smol"] = "anthropic/claude-sonnet-4-5, openai/gpt-4.1-mini"
            self.assertEqual(r.candidates("smol"),
                             [("anthropic", "claude-sonnet-4-5"), ("openai", "gpt-4.1-mini")])

    def test_failover_on_5xx(self):
        from marfim.router import Router
        from marfim.providers.base import ProviderError, ToolSpec
        with tempfile.TemporaryDirectory() as tmp:
            cfg = Config.load(project_root=Path(tmp))
            cfg.model_roles["default"] = "mock/failing,mock/working"
            cfg.providers["mock"] = cfg.providers.get("mock")
            r = Router(cfg)

            class FakeProvider:
                def __init__(self, config, model):
                    self.model = model
                    self.config = config

                def stream(self, messages, tools, system=""):
                    if self.model == "failing":
                        raise ProviderError("boom", status=503)
                    yield {"type": "text_delta", "text": f"hello from {self.model}"}
                    from marfim.providers.base import Usage
                    yield {"type": "done", "usage": Usage(1, 1), "stop_reason": "stop"}

            import marfim.router as router_mod
            with mock.patch.object(router_mod, "build_provider",
                                   lambda c, m, max_output_tokens=None: FakeProvider(c, m)):
                events = list(r.stream_with_failover([{"role": "user", "content": "hi"}],
                                                     [], system="s"))
            text = "".join(e.get("text", "") for e in events if e["type"] == "text_delta")
            self.assertEqual(text, "hello from working")

    def test_budget_refuses(self):
        from marfim.router import BudgetExceeded, Router
        with tempfile.TemporaryDirectory() as tmp:
            cfg = Config.load(project_root=Path(tmp))
            cfg.role_budgets["smol"] = 0.01
            r = Router(cfg)
            r.add_spend("smol", "openai", "gpt-4.1-mini", 10_000_000, 10_000_000)
            with self.assertRaises(BudgetExceeded):
                r.check_budget("smol")


# --------------------------------------------------------------------------- #
# §4.4 parallel tasks + §4.5 verify + finding 25 role isolation
# --------------------------------------------------------------------------- #

class ParallelAndRoleTest(unittest.TestCase):
    def test_parallel_task_calls_run(self):
        from marfim.agent.loop import Agent
        from marfim.permissions import PermissionEngine
        from marfim.providers.mock import MockProvider
        from marfim.providers.base import Usage
        from marfim.session.store import Session
        from marfim.tools.task import ROLE_TOOLS

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            cfg = Config.load(project_root=root)
            cfg.mode = "bypass"
            cfg.session_dir = str(root / "sessions")
            cfg.max_parallel_tasks = 4
            session = Session.create(cfg.session_dir, str(root))
            script = [[
                {"type": "tool_call", "id": "p1", "name": "task",
                 "arguments": json.dumps({"prompt": "one", "role": "explore"})},
                {"type": "tool_call", "id": "p2", "name": "task",
                 "arguments": json.dumps({"prompt": "two", "role": "explore"})},
                {"type": "tool_call", "id": "p3", "name": "task",
                 "arguments": json.dumps({"prompt": "three", "role": "explore"})},
                {"type": "done", "usage": Usage(5, 5), "stop_reason": "tool_use"}],
                [{"type": "text_delta", "text": "all done"},
                 {"type": "done", "usage": Usage(5, 5), "stop_reason": "stop"}]]
            provider = MockProvider(cfg.provider("mock"), "mock-1", script)
            perms = PermissionEngine.from_config(cfg)
            perms.mode = Mode.BYPASS
            agent = Agent.create(config=cfg, session=session, provider=provider,
                                 permissions=perms, say=lambda m: None)

            def fake_spawn(parent, role, cwd, depth, model_role=""):
                sub_cfg = parent.config
                sub_session = Session.create(sub_cfg.session_dir, str(cwd))
                sub_provider = MockProvider(
                    sub_cfg.provider("mock"), "mock-1",
                    [[{"type": "text_delta", "text": "explore report ok"},
                      {"type": "done", "usage": Usage(1, 1), "stop_reason": "stop"}]])
                from marfim.tools.task import ROLE_TOOLS
                return Agent(config=sub_cfg, session=sub_session, provider=sub_provider,
                             permissions=parent.permissions.child_for_role(role, ROLE_TOOLS[role]),
                             skills=[], memory=[], depth=depth, role=role,
                             say=lambda m: None, stop_event=parent.stop_event,
                             default_cwd=cwd)

            import marfim.agent.loop as loop_mod
            orig = loop_mod.Agent.spawn_subagent
            loop_mod.Agent.spawn_subagent = classmethod(
                lambda cls, parent, role, cwd, depth, model_role="": fake_spawn(
                    parent, role, cwd, depth, model_role))
            try:
                result = agent.run_turn("fan out")
            finally:
                loop_mod.Agent.spawn_subagent = orig
            tool_msgs = [m for m in session.messages if m.get("role") == "tool"]
            self.assertEqual(len(tool_msgs), 3)
            for m in tool_msgs:
                self.assertFalse(m.get("is_error"), m["content"])
            joined = " ".join(m["content"] for m in tool_msgs)
            self.assertEqual(joined.count("explore report ok"), 3)

    def test_role_allowlist_blocks_writes_even_in_bypass(self):
        from marfim.tools.task import ROLE_TOOLS
        with tempfile.TemporaryDirectory() as tmp:
            cfg = Config.load(project_root=Path(tmp))
            pe = PermissionEngine.from_config(cfg)
            pe.mode = Mode.BYPASS
            child = pe.child_for_role("explore", ROLE_TOOLS["explore"])
            r = child.check("write", {"file_path": str(Path(tmp) / "x.txt")}, Path(tmp))
            self.assertEqual(r.decision, Decision.DENY)
            r2 = child.check("read", {"file_path": str(Path(tmp) / "x.txt")}, Path(tmp))
            self.assertEqual(r2.decision, Decision.ALLOW)
            r3 = child.check("bash", {"command": "git status"}, Path(tmp))
            self.assertEqual(r3.decision, Decision.ALLOW)   # bash IS in explore's set

    def test_verify_tool(self):
        from marfim.tools.base import ToolCtx
        from marfim.tools.verify import build_tool
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            cfg = Config.load(project_root=root)
            cfg.verify_commands = ["true", "false"]
            from types import SimpleNamespace
            ctx = ToolCtx(cwd=root, agent=SimpleNamespace(config=cfg))
            r = build_tool().run({}, ctx)
            self.assertTrue(r.is_error)
            self.assertIn("verification FAILED", r.text)
            self.assertIn("✓ true", r.text)
            self.assertIn("✗ false", r.text)
            cfg.verify_commands = ["true"]
            r2 = build_tool().run({}, ctx)
            self.assertFalse(r2.is_error)
            self.assertIn("verification PASSED", r2.text)


class Finding29SteeringTest(unittest.TestCase):
    def test_mid_turn_steering_reaches_the_model(self):
        """Review finding 29: steering injected mid-turn is delivered at the
        next tool boundary and reaches the next provider request."""
        from marfim.agent.loop import Agent
        from marfim.permissions import PermissionEngine
        from marfim.providers.mock import MockProvider
        from marfim.providers.base import Usage
        from marfim.session.store import Session
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            cfg = Config.load(project_root=root)
            cfg.mode = "bypass"
            cfg.session_dir = str(root / "sessions")
            cfg.max_turns = 10
            session = Session.create(cfg.session_dir, str(root))
            script = [
                # a text delta FIRST: the user types while the model streams
                [{"type": "text_delta", "text": "let me check the list"},
                 {"type": "tool_call", "id": "t1", "name": "todo",
                  "arguments": '{"items": []}'},
                 {"type": "done", "usage": Usage(5, 5), "stop_reason": "tool_use"}],
                [{"type": "text_delta", "text": "acknowledged steering"},
                 {"type": "done", "usage": Usage(5, 5), "stop_reason": "stop"}],
            ]
            provider = MockProvider(cfg.provider("mock"), "mock-1", script)
            perms = PermissionEngine.from_config(cfg)
            perms.mode = Mode.BYPASS
            agent = Agent.create(config=cfg, session=session, provider=provider,
                                 permissions=perms, say=lambda m: None)

            steered = {"done": False}

            def steer_mid_turn(event):
                if event.get("kind") == "text_delta" and not steered["done"]:
                    steered["done"] = True
                    agent.steer("stop using the todo tool")

            agent.say = steer_mid_turn
            result = agent.run_turn("go")
            self.assertEqual(result.text, "acknowledged steering")
            steer_msgs = [m for m in session.messages
                          if m.get("role") == "user" and "mid-turn steering" in str(m.get("content"))]
            self.assertEqual(len(steer_msgs), 1)
            self.assertIn("stop using the todo tool", steer_msgs[0]["content"])
            # the second provider request included the steering message
            self.assertIn("mid-turn steering",
                          json.dumps(provider.requests[1]["messages"]))


class Finding30McpAddTest(unittest.TestCase):
    def test_mcp_add_writes_valid_toml_and_refuses_duplicates(self):
        """Review finding 30: `marfim mcp add` used to overwrite the
        subcommand dest and launch a REPL."""
        import shutil
        from marfim import cli
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / ".git").mkdir()          # project root marker
            home = root / "home"
            home.mkdir()
            env = {"MARFIM_HOME": str(home)}
            old_cwd = os.getcwd()
            os.chdir(root)
            import marfim.config as _cfgmod
            _orig_home = _cfgmod.marfim_home
            _cfgmod.marfim_home = lambda: home
            from marfim import trust as _trust
            _orig_trust_home = _trust.marfim_home
            _trust.marfim_home = lambda: home
            try:
                rc = cli.main(["mcp", "add", "srv", 'bash -c "echo hi"'])
                self.assertEqual(rc, 0)
                import tomllib
                data = tomllib.loads((root / ".marfim" / "config.toml").read_text())
                self.assertEqual(data["mcp_servers"]["srv"]["command"], 'bash -c "echo hi"')
                # duplicate refused
                rc2 = cli.main(["mcp", "add", "srv", "echo"])
                self.assertEqual(rc2, 1)
            finally:
                _cfgmod.marfim_home = _orig_home
                _trust.marfim_home = _orig_trust_home
                os.chdir(old_cwd)


if __name__ == "__main__":
    unittest.main()
