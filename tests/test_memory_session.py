"""Tests: memory/skills discovery, slash command registry, registry module."""

import tempfile
import unittest
from pathlib import Path

from marfim.config import Config
from marfim.memory import discover_skills
from marfim.session.store import Session, find_session, list_sessions


class MemorySkillsTest(unittest.TestCase):
    def test_skill_discovery(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            skill_dir = root / ".marfim" / "skills" / "demo"
            skill_dir.mkdir(parents=True)
            (skill_dir / "SKILL.md").write_text(
                "---\nname: demo\ndescription: A demo skill for tests\n---\n"
                "# Demo\nDo the thing.\n")
            cfg = Config.load(project_root=root)
            skills = discover_skills(cfg)
            self.assertEqual(len(skills), 1)
            self.assertEqual(skills[0].name, "demo")
            self.assertIn("Do the thing", skills[0].body_without_frontmatter())

    def test_claude_dir_compat(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            compat = root / ".claude" / "skills" / "legacy"
            compat.mkdir(parents=True)
            (compat / "SKILL.md").write_text("---\ndescription: legacy skill\n---\nbody")
            cfg = Config.load(project_root=root)
            skills = discover_skills(cfg)
            self.assertTrue(any(s.name == "legacy" for s in skills))


class SessionStoreTest(unittest.TestCase):
    def test_create_record_load_roundtrip(self):
        with tempfile.TemporaryDirectory() as tmp:
            cfg_dir = str(Path(tmp) / "sessions")
            s = Session.create(cfg_dir, str(tmp), title="t")
            s.append_message({"role": "user", "content": "hello world"})
            s.append_message({"role": "assistant", "content": "hi"})
            s.add_usage(type("U", (), {"input_tokens": 12, "output_tokens": 3})())
            s.record("todo", {"items": [{"content": "x", "status": "pending"}]})
            loaded = Session.load(s.path)
            self.assertEqual(len(loaded.messages), 2)
            self.assertEqual(loaded.usage_in, 12)
            self.assertEqual(loaded.todos[0]["content"], "x")
            self.assertEqual(loaded.title, "t")

    def test_list_and_find(self):
        with tempfile.TemporaryDirectory() as tmp:
            cfg_dir = str(Path(tmp) / "sessions")
            a = Session.create(cfg_dir, str(tmp), title="alpha")
            b = Session.create(cfg_dir, str(tmp), title="beta")
            found = list_sessions(cfg_dir)
            self.assertEqual({s.sid for s in found}, {a.sid, b.sid})
            got = find_session(cfg_dir, b.sid)
            self.assertEqual(got.sid, b.sid)
            self.assertEqual(find_session(cfg_dir, "last").sid in (a.sid, b.sid), True)

    def test_fork_copies_messages(self):
        with tempfile.TemporaryDirectory() as tmp:
            cfg_dir = str(Path(tmp) / "sessions")
            s = Session.create(cfg_dir, str(tmp), title="orig")
            s.append_message({"role": "user", "content": "one"})
            forked = s.fork(cfg_dir)
            self.assertEqual(forked.messages, s.messages)
            self.assertNotEqual(forked.sid, s.sid)


class SlashRegistryTest(unittest.TestCase):
    def test_registry_populated(self):
        from marfim.ui.slash import COMMANDS
        expected = {"model", "provider", "permissions", "sessions", "resume", "fork",
                    "export", "rewind", "compact", "clear", "context", "memory", "init",
                    "skills", "tools", "map", "mcp", "usage", "run", "undo", "config",
                    "doctor", "help", "exit"}
        self.assertTrue(expected.issubset(set(COMMANDS)), f"missing: {expected - set(COMMANDS)}")


class ProviderRegistryTest(unittest.TestCase):
    def test_presets_cover_major_wires(self):
        from marfim.providers.registry import PRESETS, get_provider_preset
        for name in ("openai", "anthropic", "openrouter", "groq", "deepseek", "ollama"):
            preset = get_provider_preset(name)
            assert preset is not None
            self.assertIn(preset["wire"], ("openai", "anthropic"))
        self.assertEqual(PRESETS["mock"]["wire"], "mock")

    def test_build_provider_factory(self):
        from marfim.config import ProviderConfig
        from marfim.providers.base import build_provider
        from marfim.providers.anthropic import AnthropicProvider
        from marfim.providers.mock import MockProvider
        from marfim.providers.openai_compat import OpenAICompatProvider
        self.assertIsInstance(build_provider(ProviderConfig(name="x", wire="openai"), "m"),
                              OpenAICompatProvider)
        self.assertIsInstance(build_provider(ProviderConfig(name="a", wire="anthropic"), "m"),
                              AnthropicProvider)
        self.assertIsInstance(build_provider(ProviderConfig(name="m", wire="mock"), "m"),
                              MockProvider)


if __name__ == "__main__":
    unittest.main()
