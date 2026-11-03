"""Tests: config layering, permission engine, edit ladder, patch DSL."""

import os
import tempfile
import unittest
from pathlib import Path

from marfim.config import Config
from marfim.permissions import Decision, Mode, PermissionEngine


class ConfigTest(unittest.TestCase):
    def test_defaults_load(self):
        with tempfile.TemporaryDirectory() as tmp:
            cfg = Config.load(project_root=Path(tmp))
            self.assertEqual(cfg.mode, "default")
            self.assertTrue(cfg.protect_configs)
            self.assertIn("openai", cfg.providers)  # default provider auto-preset

    def test_toml_layering(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / ".marfim").mkdir()
            (root / ".marfim" / "config.toml").write_text(
                'default_model = "test-model"\n'
                "[permissions]\nallow = ['Bash(git *)']\n"
                "[mcp_servers.demo]\ncommand = 'echo'\nargs = ['hi']\n")
            cfg = Config.load(project_root=root)
            self.assertEqual(cfg.default_model, "test-model")
            self.assertIn("Bash(git *)", cfg.allow)
            self.assertIn("demo", cfg.mcp_servers)
            self.assertEqual(cfg.mcp_servers["demo"].args, ["hi"])


class PermissionTest(unittest.TestCase):
    def setUp(self):
        self.cfg = Config.load(project_root=Path("/tmp"))
        self.root = Path("/tmp/proj")
        self.pe = PermissionEngine.from_config(self.cfg)

    def test_readonly_allowed(self):
        r = self.pe.check("read", {"file_path": "/tmp/x.py"}, self.root)
        self.assertEqual(r.decision, Decision.ALLOW)

    def test_write_asks_in_default(self):
        r = self.pe.check("write", {"file_path": "/tmp/proj/a.py"}, self.root)
        self.assertEqual(r.decision, Decision.ASK)

    def test_write_allowed_in_accept_edits(self):
        self.pe.mode = Mode.ACCEPT_EDITS
        r = self.pe.check("write", {"file_path": "/tmp/proj/a.py"}, self.root)
        self.assertEqual(r.decision, Decision.ALLOW)

    def test_plan_mode_blocks_mutations(self):
        self.pe.mode = Mode.PLAN
        r = self.pe.check("edit", {"file_path": "/tmp/proj/a.py", "old_string": "a",
                                   "new_string": "b"}, self.root)
        self.assertEqual(r.decision, Decision.DENY)
        r2 = self.pe.check("read", {"file_path": "/tmp/proj/a.py"}, self.root)
        self.assertEqual(r2.decision, Decision.ALLOW)

    def test_plan_mode_readonly_bash(self):
        self.pe.mode = Mode.PLAN
        r = self.pe.check("bash", {"command": "git status"}, self.root)
        self.assertEqual(r.decision, Decision.ALLOW)
        r2 = self.pe.check("bash", {"command": "rm -rf /"}, self.root)
        self.assertEqual(r2.decision, Decision.DENY)

    def test_deny_rule_wins(self):
        self.cfg.deny.append("Bash(rm *)")
        r = self.pe.check("bash", {"command": "rm -rf /tmp/x"}, self.root)
        self.assertEqual(r.decision, Decision.DENY)

    def test_allow_rule(self):
        self.cfg.allow.append("Bash(git *)")
        r = self.pe.check("bash", {"command": "git push origin main"}, self.root)
        self.assertEqual(r.decision, Decision.ALLOW)

    def test_config_protection(self):
        target = self.root / ".marfim" / "config.toml"
        r = self.pe.check("write", {"file_path": str(target), "content": "x"}, self.root)
        self.assertEqual(r.decision, Decision.DENY)
        self.assertIn("config-protection", r.reason)

    def test_bypass(self):
        self.pe.mode = Mode.BYPASS
        r = self.pe.check("bash", {"command": "anything"}, self.root)
        self.assertEqual(r.decision, Decision.ALLOW)

    def test_grant_always_shape(self):
        rule = self.pe.grant_always("bash", {"command": "git commit -m 'x'"})
        self.assertEqual(rule, "Bash(git commit *)")
        r = self.pe.check("bash", {"command": "git commit -m 'y'"}, self.root)
        self.assertEqual(r.decision, Decision.ALLOW)


if __name__ == "__main__":
    unittest.main()
