"""End-to-end agent loop test with the mock provider (opencode `simulation`
doctrine) — exercises streaming, tool dispatch, permissions, steering queue,
session persistence and compaction boundaries."""

import tempfile
import unittest
from pathlib import Path

from marfim.agent.loop import Agent
from marfim.config import Config
from marfim.permissions import Decision, Mode, PermissionEngine
from marfim.providers.base import Usage
from marfim.providers.mock import MockProvider
from marfim.session.store import Session


def make_agent(script, tmp: Path, mode: Mode = Mode.BYPASS, events=None):
    cfg = Config.load(project_root=tmp)
    cfg.mode = mode.value
    cfg.session_dir = str(tmp / "sessions")
    cfg.max_turns = 10
    session = Session.create(cfg.session_dir, str(tmp))
    provider = MockProvider(cfg.provider("mock"), "mock-1", script)
    perms = PermissionEngine.from_config(cfg)
    perms.mode = mode
    collected = []
    agent = Agent.create(config=cfg, session=session, provider=provider,
                         permissions=perms, say=collected.append)
    return agent, session, collected


class AgentLoopTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)

    def tearDown(self):
        self.tmp.cleanup()

    def test_plain_text_turn(self):
        script = [[{"type": "text_delta", "text": "Hello "},
                   {"type": "text_delta", "text": "world"},
                   {"type": "done", "usage": Usage(10, 5), "stop_reason": "stop"}]]
        agent, session, events = make_agent(script, self.root)
        result = agent.run_turn("hi")
        self.assertEqual(result.text, "Hello world")
        self.assertEqual(result.tool_calls, 0)
        self.assertEqual(len(session.messages), 2)  # user + assistant
        self.assertEqual(agent.usage.input_tokens, 10)

    def test_tool_call_roundtrip(self):
        script = [
            [{"type": "tool_call", "id": "t1", "name": "write",
              "arguments": '{"file_path": "made.txt", "content": "made by agent"}'},
             {"type": "done", "usage": Usage(20, 10), "stop_reason": "tool_use"}],
            [{"type": "text_delta", "text": "I created the file."},
             {"type": "done", "usage": Usage(30, 10), "stop_reason": "stop"}],
        ]
        agent, session, events = make_agent(script, self.root)
        result = agent.run_turn("create made.txt")
        self.assertEqual(result.text, "I created the file.")
        self.assertEqual(result.tool_calls, 1)
        self.assertTrue((self.root / "made.txt").exists())
        tool_msg = [m for m in session.messages if m.get("role") == "tool"]
        self.assertEqual(len(tool_msg), 1)
        self.assertIn("created", tool_msg[0]["content"])

    def test_unknown_tool_is_error_result(self):
        script = [
            [{"type": "tool_call", "id": "t1", "name": "nonexistent",
              "arguments": "{}"},
             {"type": "done", "usage": Usage(5, 5), "stop_reason": "tool_use"}],
            [{"type": "text_delta", "text": "ok"},
             {"type": "done", "usage": Usage(5, 5), "stop_reason": "stop"}],
        ]
        agent, session, _ = make_agent(script, self.root)
        result = agent.run_turn("try it")
        self.assertTrue(any(m.get("role") == "tool" and m.get("is_error")
                            for m in session.messages))
        self.assertEqual(result.text, "ok")

    def test_permission_ask_denied_headless(self):
        script = [
            [{"type": "tool_call", "id": "t1", "name": "bash",
              "arguments": '{"command": "echo hi"}'},
             {"type": "done", "usage": Usage(5, 5), "stop_reason": "tool_use"}],
            [{"type": "text_delta", "text": "understood"},
             {"type": "done", "usage": Usage(5, 5), "stop_reason": "stop"}],
        ]
        agent, session, _ = make_agent(script, self.root, mode=Mode.DEFAULT)
        result = agent.run_turn("run echo")
        tool_msgs = [m for m in session.messages if m.get("role") == "tool"]
        self.assertEqual(len(tool_msgs), 1)
        self.assertTrue(tool_msgs[0].get("is_error"))
        self.assertIn("denied", tool_msgs[0]["content"])

    def test_retry_on_provider_error(self):
        from marfim.providers.base import ProviderError

        class FlakyProvider(MockProvider):
            attempts = 0

            def stream(self, messages, tools, system=""):
                self.attempts += 1  # type: ignore[attr-defined]
                if self.attempts < 2:
                    raise ProviderError("transient", status=503)
                yield from super().stream(messages, tools, system)

        cfg = Config.load(project_root=self.root)
        cfg.mode = "bypass"
        cfg.session_dir = str(self.root / "sessions")
        session = Session.create(cfg.session_dir, str(self.root))
        provider = FlakyProvider(cfg.provider("mock"), "mock-1",
                                 [[{"type": "text_delta", "text": "recovered"},
                                   {"type": "done", "usage": Usage(5, 2),
                                    "stop_reason": "stop"}]])
        agent = Agent.create(config=cfg, session=session, provider=provider,
                             permissions=PermissionEngine.from_config(cfg))
        result = agent.run_turn("hello")
        self.assertEqual(result.text, "recovered")
        self.assertEqual(result.turns, 2)

    def test_session_resume(self):
        script = [[{"type": "text_delta", "text": "reply one"},
                   {"type": "done", "usage": Usage(5, 2), "stop_reason": "stop"}]]
        agent, session, _ = make_agent(script, self.root)
        agent.run_turn("first")
        reloaded = Session.load(session.path)
        self.assertEqual(len(reloaded.messages), 2)
        self.assertEqual(reloaded.messages[0]["content"], "first")

    def test_checkpoint_and_rewind(self):
        script = [
            [{"type": "tool_call", "id": "t1", "name": "write",
              "arguments": '{"file_path": "track.txt", "content": "v1"}'},
             {"type": "done", "usage": Usage(5, 5), "stop_reason": "tool_use"}],
            [{"type": "text_delta", "text": "created"},
             {"type": "done", "usage": Usage(5, 5), "stop_reason": "stop"}],
            [{"type": "tool_call", "id": "t2", "name": "write",
              "arguments": '{"file_path": "track.txt", "content": "v2"}'},
             {"type": "done", "usage": Usage(5, 5), "stop_reason": "tool_use"}],
            [{"type": "text_delta", "text": "updated"},
             {"type": "done", "usage": Usage(5, 5), "stop_reason": "stop"}],
        ]
        agent, session, _ = make_agent(script, self.root)
        agent.run_turn("write v1")
        agent.run_turn("write v2")
        target = self.root / "track.txt"
        self.assertEqual(target.read_text(), "v2")
        restored = session.rewind(self.root, 1)   # restore file(s) from last checkpoint
        self.assertIn("track.txt", restored)
        self.assertEqual(target.read_text(), "v1")

    def test_subagent_flow(self):
        outer_script = [
            [{"type": "tool_call", "id": "t1", "name": "task",
              "arguments": '{"prompt": "find the answer", "role": "explore"}'},
             {"type": "done", "usage": Usage(5, 5), "stop_reason": "tool_use"}],
            [{"type": "text_delta", "text": "subagent reported"},
             {"type": "done", "usage": Usage(5, 5), "stop_reason": "stop"}],
        ]
        agent, session, _ = make_agent(outer_script, self.root)
        # inject a script for the spawned subagent by monkeypatching the factory
        from marfim.agent import loop as loop_mod

        def fake_spawn(parent, role, cwd, depth, model_role=""):
            sub_cfg = parent.config
            sub_session = Session.create(sub_cfg.session_dir, str(cwd))
            sub_provider = MockProvider(sub_cfg.provider("mock"), "mock-1",
                                        [[{"type": "text_delta",
                                           "text": "explore report: found 3 things"},
                                          {"type": "done", "usage": Usage(5, 5),
                                           "stop_reason": "stop"}]])
            return Agent(config=sub_cfg, session=sub_session, provider=sub_provider,
                         permissions=parent.permissions, skills=[], memory=[],
                         depth=depth, role=role, say=lambda m: None)
        loop_mod.Agent.spawn_subagent = classmethod(
            lambda cls, parent, role, cwd, depth, model_role="": fake_spawn(
                parent, role, cwd, depth, model_role))
        result = agent.run_turn("delegate exploration")
        tool_msgs = [m for m in session.messages if m.get("role") == "tool"]
        self.assertEqual(len(tool_msgs), 1)
        self.assertFalse(tool_msgs[0].get("is_error"))
        self.assertIn("explore report", tool_msgs[0]["content"])


if __name__ == "__main__":
    unittest.main()
