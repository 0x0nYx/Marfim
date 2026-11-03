"""Tests: fuzzy edit ladder, patch DSL, repomap, session store."""

import tempfile
import unittest
from pathlib import Path

from marfim.tools.base import ToolCtx
from marfim.tools.fs import build_tools
from marfim.tools.patch import apply_patch_text, parse_patch
from marfim.util import fuzzy_replace


class FuzzyLadderTest(unittest.TestCase):
    def test_exact(self):
        out = fuzzy_replace("a\nb\nc", "b", "B")
        self.assertEqual(out[0], "a\nB\nc")
        self.assertEqual(out[1], "exact")

    def test_rstrip(self):
        out = fuzzy_replace("def f():    \n    return 1\n", "def f():\n    return 1",
                            "def f():\n    return 2")
        self.assertIn("return 2", out[0])

    def test_outdent(self):
        haystack = "def a():\n    x = 1\n    y = 2\n"
        needle = "x = 1\n    y = 2"          # missing outer indent on first line
        out = fuzzy_replace(haystack, needle, "x = 9\n    y = 2")
        self.assertIn("x = 9", out[0])

    def test_ellipsis(self):
        haystack = "start\nfill1\nfill2\nend\n"
        needle = "start\n...\nend"
        out = fuzzy_replace(haystack, needle, "start\nREPLACED\nend")
        self.assertIn("REPLACED", out[0])

    def test_no_match(self):
        out = fuzzy_replace("abc", "zzz", "y")
        self.assertIsNone(out)


class FsToolsTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.tools = {t.name: t for t in build_tools()}
        self.ctx = ToolCtx(cwd=self.root)

    def tearDown(self):
        self.tmp.cleanup()

    def test_write_read_edit_flow(self):
        write = self.tools["write"]
        read = self.tools["read"]
        edit = self.tools["edit"]
        r1 = write.run({"file_path": "demo.py", "content": "x = 1\ny = 2\n"}, self.ctx)
        self.assertFalse(r1.is_error)
        r2 = read.run({"file_path": "demo.py"}, self.ctx)
        self.assertIn("x = 1", r2.text)
        r3 = edit.run({"file_path": "demo.py", "old_string": "y = 2",
                       "new_string": "y = 3"}, self.ctx)
        self.assertFalse(r3.is_error)
        self.assertEqual(Path(self.root, "demo.py").read_text(), "x = 1\ny = 3\n")

    def test_gateguard_requires_read(self):
        edit = self.tools["edit"]
        Path(self.root, "a.txt").write_text("hello\n")
        fresh_ctx = ToolCtx(cwd=self.root)   # nothing read yet
        r = edit.run({"file_path": "a.txt", "old_string": "hello",
                      "new_string": "nope"}, fresh_ctx)
        self.assertTrue(r.is_error)
        self.assertIn("GateGuard", r.text)

    def test_grep(self):
        Path(self.root, "x.py").write_text("IMPORTANT = True\nother = 2\n")
        grep = self.tools["grep"]
        r = grep.run({"pattern": "IMPORTANT"}, self.ctx)
        self.assertIn("IMPORTANT = True", r.text)

    def test_glob_skips_ignored(self):
        (self.root / "node_modules").mkdir()
        (self.root / "node_modules" / "junk.js").write_text("x")
        (self.root / "keep.js").write_text("x")
        glob = self.tools["glob"]
        r = glob.run({"pattern": "**/*.js"}, self.ctx)
        self.assertIn("keep.js", r.text)
        self.assertNotIn("node_modules", r.text)


class PatchTest(unittest.TestCase):
    def test_parse_and_apply(self):
        patch = """*** Begin Patch
*** Add File: new.py
+alpha = 1
*** Update File: old.py
@@ beta
-beta = 2
+beta = 20
*** End Patch"""
        ops = parse_patch(patch)
        self.assertEqual([o["op"] for o in ops], ["add", "update"])
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "old.py").write_text("# c\nbeta = 2\n")
            from marfim.tools.base import ToolCtx
            ctx = ToolCtx(cwd=root)
            ctx.mark_read(root / "old.py")   # GateGuard: must read first
            report = apply_patch_text(patch, root, ctx)
            self.assertIn("added new.py", report)
            self.assertIn("updated old.py", report)
            self.assertIn("beta = 20", (root / "old.py").read_text())

    def test_gate_guard(self):
        patch = """*** Begin Patch
*** Update File: old.py
@@ beta
-beta = 2
+beta = 20
*** End Patch"""
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "old.py").write_text("beta = 2\n")
            from marfim.tools.base import ToolCtx
            try:
                apply_patch_text(patch, root, ToolCtx(cwd=root))
                self.fail("expected PatchError")
            except Exception as exc:
                self.assertIn("GateGuard", str(exc))


if __name__ == "__main__":
    unittest.main()
