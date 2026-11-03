"""P1 regression tests — review findings 20–27 (loop, session, context)."""

import tempfile
import unittest
from pathlib import Path

from marfim.agent import compaction
from marfim.agent.loop import Agent
from marfim.config import Config
from marfim.permissions import Mode, PermissionEngine
from marfim.providers.base import ProviderError, Usage
from marfim.providers.mock import MockProvider
from marfim.session.store import Session


def make_agent(script, tmp: Path, mode: Mode = Mode.BYPASS):
    cfg = Config.load(project_root=tmp)
    cfg.mode = mode.value
    cfg.session_dir = str(tmp / "sessions")
    cfg.max_turns = 10
    session = Session.create(cfg.session_dir, str(tmp))
    provider = MockProvider(cfg.provider("mock"), "mock-1", script)
    perms = PermissionEngine.from_config(cfg)
    perms.mode = mode
    agent = Agent.create(config=cfg, session=session, provider=provider,
                         permissions=perms, say=lambda m: None)
    return agent, session


class Finding20InterruptTest(unittest.TestCase):
    def test_interrupt_fills_synthetic_tool_results(self):
        """After a stop between two tool calls, t2 must still get a result."""
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            script = [
                [{"type": "tool_call", "id": "t1", "name": "todo",
                  "arguments": '{"items": []}'},
                 {"type": "tool_call", "id": "t2", "name": "todo",
                  "arguments": '{"items": []}'},
                 {"type": "done", "usage": Usage(5, 5), "stop_reason": "tool_use"}],
            ]
            agent, session = make_agent(script, root)

            def interrupt_after_first_tool(event):
                if event.get("kind") == "tool_result":
                    agent.stop_event.set()   # user pressed Ctrl-C after t1

            agent.say = interrupt_after_first_tool
            agent.run_turn("go")
            tool_msgs = [m for m in session.messages if m.get("role") == "tool"]
            ids = {m["tool_call_id"] for m in tool_msgs}
            self.assertEqual(ids, {"t1", "t2"})   # NO dangling calls
            interrupted = [m for m in tool_msgs if m["content"] == "interrupted"]
            self.assertEqual(len(interrupted), 1)   # exactly t2
            self.assertEqual(interrupted[0]["tool_call_id"], "t2")
            # the persisted history is provider-valid: every tool_call answered
            assistants = [m for m in session.messages if m.get("role") == "assistant"
                          and m.get("tool_calls")]
            answered = {m["tool_call_id"] for m in session.messages
                        if m.get("role") == "tool"}
            for a in assistants:
                for tc in a["tool_calls"]:
                    self.assertIn(tc["id"], answered)


class Finding21CompactionResumeTest(unittest.TestCase):
    def test_compact_boundary_survives_resume(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            agent, session = make_agent([], root)
            for i in range(10):
                session.append_message({"role": "user", "content": f"q{i}"})
                session.append_message({"role": "assistant", "content": f"a{i}"})
            new_messages, summary = compaction.compact_messages(
                session.messages, lambda msgs: "SUMMARY-TEXT")
            self.assertTrue(summary)                       # a real boundary
            session.record("compact", {"summary": summary, "keep": len(new_messages)})
            session.messages = new_messages
            reloaded = Session.load(session.path)
            self.assertEqual(len(reloaded.messages), len(new_messages))
            self.assertIn("SUMMARY-TEXT", reloaded.messages[0]["content"])
            self.assertEqual(reloaded.messages[1:], new_messages[1:])

    def test_clear_survives_resume(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            session = Session.create(str(root / "sessions"), str(root))
            session.append_message({"role": "user", "content": "hi"})
            session.record("truncate", {"keep": 0})
            reloaded = Session.load(session.path)
            self.assertEqual(reloaded.messages, [])


class Finding22CompactionSafetyTest(unittest.TestCase):
    def test_tail_never_starts_on_tool_message(self):
        messages = ([{"role": "user", "content": f"u{i}"},
                     {"role": "assistant", "content": None,
                      "tool_calls": [{"id": f"c{i}", "name": "read", "arguments": "{}"}]},
                     {"role": "tool", "tool_call_id": f"c{i}", "content": "data"}]
                    for i in range(10))
        msgs = [m for seq in messages for m in seq]
        new_messages, _ = compaction.compact_messages(msgs, lambda m: "sum")
        self.assertNotEqual(new_messages[1].get("role"), "tool")

    def test_failed_summary_keeps_history(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            agent, session = make_agent([], root)
            for i in range(30):
                session.append_message({"role": "user", "content": "x" * 5000})
            before = len(session.messages)

            def exploding_complete(msgs):
                raise ProviderError("summary endpoint down", status=503)

            messages, ok = agent._messages_with_compaction()  # uses real provider (mock empty)
            # the mock returns "(mock: no scripted turn left)" — non-empty, so
            # compaction proceeds; but an EMPTY summary must raise ValueError
            def empty_complete(msgs):
                return ""
            with self.assertRaises(ValueError):
                compaction.compact_messages(session.messages, empty_complete)


class Finding26TodoSyncTest(unittest.TestCase):
    def test_todo_write_updates_memory(self):
        from marfim.tools.base import ToolCtx
        from marfim.tools.todo import build_tools
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            session = Session.create(str(root / "sessions"), str(root))
            ctx = ToolCtx(cwd=root, session=session)
            todo = build_tools()[0]
            r = todo.run({"items": [{"content": "task", "status": "pending"}]}, ctx)
            self.assertFalse(r.is_error)
            self.assertEqual(session.todos[0]["content"], "task")   # same session!
            reloaded = Session.load(session.path)
            self.assertEqual(reloaded.todos[0]["content"], "task")


class Finding21bSessionFilePermsTest(unittest.TestCase):
    def test_session_file_is_0600(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            session = Session.create(str(root / "sessions"), str(root))
            mode = session.path.stat().st_mode & 0o777
            self.assertEqual(mode, 0o600)


class Finding31RewindTest(unittest.TestCase):
    def test_rewind_deletes_created_files_and_batches(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            script = [
                # turn 1: read the existing file (GateGuard evidence)
                [{"type": "tool_call", "id": "r1", "name": "read",
                  "arguments": '{"file_path": "old.txt"}'},
                 {"type": "done", "usage": Usage(5, 5), "stop_reason": "tool_use"}],
                # turn 2: create new file + overwrite the existing one
                [{"type": "tool_call", "id": "w1", "name": "write",
                  "arguments": '{"file_path": "new.txt", "content": "created"}'},
                 {"type": "tool_call", "id": "w2", "name": "write",
                  "arguments": '{"file_path": "old.txt", "content": "v2"}'},
                 {"type": "done", "usage": Usage(5, 5), "stop_reason": "tool_use"}],
                [{"type": "text_delta", "text": "done"},
                 {"type": "done", "usage": Usage(5, 5), "stop_reason": "stop"}],
            ]
            agent, session = make_agent(script, root)
            (root / "old.txt").write_text("v1")
            agent.run_turn("mutate")
            self.assertTrue((root / "new.txt").exists())
            self.assertEqual((root / "old.txt").read_text(), "v2")
            restored = session.rewind(root, 1)
            self.assertFalse((root / "new.txt").exists())   # created → deleted
            self.assertEqual((root / "old.txt").read_text(), "v1")  # edited → restored
            self.assertIn("new.txt", " ".join(restored))


class Finding27RepomapTest(unittest.TestCase):
    def test_js_go_alternation_no_crash_and_methods_visible(self):
        from marfim.tools.repomap import _symbols
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "x.js").write_text("const a = 1\nfunction hello() {}\n"
                                       "  function nested() {}\nclass Foo {}\n")
            (root / "y.go").write_text("package main\nfunc main() {}\n"
                                       "func (s *S) Method() {}\ntype T struct {}\n")
            (root / "z.py").write_text("class A:\n    def method(self):\n        pass\n")
            (root / "w.dart").write_text("class Widget extends S {}\n")
            (root / "v.kt").write_text("fun main() {}\nclass P {}\n")
            (root / "u.hpp").write_text("int compute(int x) { return x; }\n")
            js = _symbols(root / "x.js")
            self.assertIn("hello", js)
            go = _symbols(root / "y.go")
            self.assertIn("main", go)
            self.assertIn("Method", go)
            self.assertIn("T", go)
            py = _symbols(root / "z.py")
            self.assertIn("method", py)      # indented method visible
            self.assertIn("A", py)
            self.assertIn("Widget", _symbols(root / "w.dart"))
            self.assertIn("main", _symbols(root / "v.kt"))
            self.assertIn("compute", _symbols(root / "u.hpp"))


class Finding23ContextWindowTest(unittest.TestCase):
    def _with_catalog(self, catalog: dict | None):
        import marfim.agent.compaction as c
        import marfim.providers.registry as reg
        cache = Path(self.tmp.name) / "cache" / "models-dev.json"
        cache.parent.mkdir(parents=True, exist_ok=True)
        cache.write_text("") if catalog is None else cache.write_text(
            __import__("json").dumps(catalog))
        self._orig_cache_path = reg.catalog_cache_path
        reg.catalog_cache_path = lambda: cache
        reg._MODELS_CACHE.clear()
        self._reg = reg
        self._comp = c

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()

    def tearDown(self):
        if hasattr(self, "_orig_cache_path"):
            self._reg.catalog_cache_path = self._orig_cache_path
            self._reg._MODELS_CACHE.clear()
        self.tmp.cleanup()

    def test_offline_no_cache_gives_default(self):
        self._with_catalog(None)
        w = self._comp.context_window_for("openai/gpt-4.1-mini")
        self.assertEqual(w, 200_000)   # no network fetch inside the loop

    def test_provider_qualified_lookup(self):
        self._with_catalog({"openai": {"models": {
            "gpt-4.1-mini": {"limit": {"context": 128000}}}}})
        self.assertEqual(self._comp.context_window_for("openai/gpt-4.1-mini"), 128000)
        self.assertEqual(self._comp.context_window_for("gpt-4.1-mini"), 128000)


if __name__ == "__main__":
    unittest.main()
