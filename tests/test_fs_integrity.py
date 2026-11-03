"""P1 regression tests — review findings 12–19 (data loss & silent edits)."""

import tempfile
import time
import unittest
from pathlib import Path

from marfim.tools.base import ToolCtx, validate_args
from marfim.tools.fs import build_tools
from marfim.util import (FileIntegrityError, atomic_write_text, fuzzy_replace,
                         nearest_hint, read_text_preserve)


class FsIntegrityTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.tools = {t.name: t for t in build_tools()}
        self.ctx = ToolCtx(cwd=self.root)

    def tearDown(self):
        self.tmp.cleanup()

    # finding 12 ------------------------------------------------------------
    def test_outdent_applies_replacement(self):
        edit = self.tools["edit"]
        (self.root / "a.py").write_text("def a():\n    x = 1\n    y = 2\n")
        self.ctx.mark_read(self.root / "a.py")
        r = edit.run({"file_path": "a.py", "old_string": "x = 1\n    y = 2",
                      "new_string": "x = 9\n    y = 2"}, self.ctx)
        self.assertFalse(r.is_error, r.text)
        self.assertEqual((self.root / "a.py").read_text(),
                         "def a():\n    x = 9\n    y = 2\n")

    # finding 13 ------------------------------------------------------------
    def test_nearest_never_applies(self):
        edit = self.tools["edit"]
        original = "def one():\n    pass\n\ndef two():\n    pass\n"
        (self.root / "b.py").write_text(original)
        self.ctx.mark_read(self.root / "b.py")
        r = edit.run({"file_path": "b.py", "old_string": "def ONE():\n    pas",
                      "new_string": "def DESTROYED()"}, self.ctx)
        self.assertTrue(r.is_error)
        self.assertEqual((self.root / "b.py").read_text(), original)  # unchanged
        self.assertIn("closest match", r.text)                        # hint shown

    def test_no_fuzzy_with_replace_all(self):
        edit = self.tools["edit"]
        (self.root / "c.py").write_text("alpha = 1\n")
        self.ctx.mark_read(self.root / "c.py")
        r = edit.run({"file_path": "c.py", "old_string": "alpha  =  1",
                      "new_string": "x", "replace_all": True}, self.ctx)
        self.assertTrue(r.is_error)

    # finding 14 ------------------------------------------------------------
    def test_ambiguous_match_errors_with_count(self):
        edit = self.tools["edit"]
        (self.root / "d.py").write_text("x = 1\nx = 1\nx = 1\n")
        self.ctx.mark_read(self.root / "d.py")
        r = edit.run({"file_path": "d.py", "old_string": "x = 1",
                      "new_string": "y = 1"}, self.ctx)
        self.assertTrue(r.is_error)
        self.assertIn("3 locations", r.text)
        # replace_all works
        r2 = edit.run({"file_path": "d.py", "old_string": "x = 1",
                       "new_string": "y = 1", "replace_all": True}, self.ctx)
        self.assertFalse(r2.is_error, r2.text)
        self.assertEqual((self.root / "d.py").read_text(), "y = 1\n" * 3)

    # finding 15 ------------------------------------------------------------
    def test_write_requires_content_and_read_evidence(self):
        write = self.tools["write"]
        (self.root / "e.txt").write_text("13 bytes!!")
        fresh = ToolCtx(cwd=self.root)
        r = write.run({"file_path": "e.txt"}, fresh)
        self.assertTrue(r.is_error)
        self.assertIn("content is required", r.text)
        r2 = write.run({"file_path": "e.txt", "content": "overwritten"}, fresh)
        self.assertTrue(r2.is_error)
        self.assertIn("GateGuard", r2.text)
        self.assertEqual((self.root / "e.txt").read_text(), "13 bytes!!")

    def test_write_is_atomic_and_keeps_mode(self):
        write = self.tools["write"]
        target = self.root / "f.txt"
        target.write_text("v1")
        target.chmod(0o600)
        self.ctx.mark_read(target)
        r = write.run({"file_path": "f.txt", "content": "v2"}, self.ctx)
        self.assertFalse(r.is_error)
        self.assertEqual(target.read_text(), "v2")
        self.assertEqual(target.stat().st_mode & 0o777, 0o600)
        self.assertEqual(list(self.root.glob("*.marfim-tmp")), [])

    def test_schema_validation(self):
        from marfim.tools.base import Tool
        tool = Tool(name="x", description="", parameters={
            "type": "object",
            "properties": {"file_path": {"type": "string"},
                           "limit": {"type": "integer"},
                           "flag": {"type": "boolean", "enum": None} or {"type": "boolean"}},
            "required": ["file_path"]})
        self.assertIn("missing required", validate_args(tool, {}) or "")
        self.assertIn("must be integer", validate_args(tool, {"file_path": "a", "limit": "5"}) or "")
        err = validate_args(tool, {"file_path": 5})
        self.assertIn("must be string", err or "")

    # finding 16 ------------------------------------------------------------
    def test_stale_read_rejected(self):
        edit = self.tools["edit"]
        target = self.root / "g.txt"
        target.write_text("hello\n")
        self.ctx.mark_read(target)
        time.sleep(0.01)
        target.write_text("externally changed\n")   # the model did NOT do this
        r = edit.run({"file_path": "g.txt", "old_string": "hello",
                      "new_string": "bye"}, self.ctx)
        self.assertTrue(r.is_error)
        self.assertIn("stale read", r.text)
        self.assertEqual(target.read_text(), "externally changed\n")

    # finding 17 ------------------------------------------------------------
    def test_crlf_roundtrip(self):
        edit = self.tools["edit"]
        target = self.root / "crlf.txt"
        target.write_bytes(b"a\r\nb\r\n")
        self.ctx.mark_read(target)
        r = edit.run({"file_path": "crlf.txt", "old_string": "a",
                      "new_string": "A"}, self.ctx)
        self.assertFalse(r.is_error, r.text)
        self.assertEqual(target.read_bytes(), b"A\r\nb\r\n")

    def test_latin1_roundtrip_no_replacement_char(self):
        edit = self.tools["edit"]
        target = self.root / "latin.txt"
        target.write_bytes(b"caf\xe9 = 1\n")
        self.ctx.mark_read(target)
        r = edit.run({"file_path": "latin.txt", "old_string": "1",
                      "new_string": "2"}, self.ctx)
        self.assertFalse(r.is_error, r.text)
        self.assertEqual(target.read_bytes(), b"caf\xe9 = 2\n")
        self.assertNotIn(b"\xef\xbf\xbd", target.read_bytes())

    def test_mixed_newlines_refused(self):
        target = self.root / "mixed.txt"
        target.write_bytes(b"a\r\nb\n")
        with self.assertRaises(FileIntegrityError):
            read_text_preserve(target)
        edit = self.tools["edit"]
        self.ctx.mark_read(target)
        r = edit.run({"file_path": "mixed.txt", "old_string": "a",
                      "new_string": "A"}, self.ctx)
        self.assertTrue(r.is_error)

    # finding 18 ------------------------------------------------------------
    def test_glob_works_inside_ignored_dirname_path(self):
        # the PROJECT PATH contains "build" — absolute parts must not poison it
        base = self.root / "build" / "proj"
        base.mkdir(parents=True)
        (base / "keep.py").write_text("x = 1\n")
        (base / "node_modules").mkdir()
        (base / "node_modules" / "junk.py").write_text("y\n")
        glob = self.tools["glob"]
        ctx = ToolCtx(cwd=base)
        r = glob.run({"pattern": "**/*.py"}, ctx)
        self.assertIn("keep.py", r.text)
        self.assertNotIn("node_modules", r.text)

    # finding 19 ------------------------------------------------------------
    def test_repair_json_lenient(self):
        from marfim.util import repair_json_args
        args = repair_json_args('{"file_path": "a.py",\n "content": "x"}')
        self.assertEqual(args["file_path"], "a.py")
        args = repair_json_args('{"a": 1,}')       # trailing comma
        self.assertEqual(args["a"], 1)
        args = repair_json_args("totally not json")
        self.assertEqual(args, {"raw": "totally not json"})

    def test_read_reports_encoding(self):
        read = self.tools["read"]
        (self.root / "lat.txt").write_bytes(b"caf\xe9\n")
        r = read.run({"file_path": "lat.txt"}, self.ctx)
        self.assertIn("latin-1", r.text)
        self.assertIn("caf\xe9", r.text)


if __name__ == "__main__":
    unittest.main()
