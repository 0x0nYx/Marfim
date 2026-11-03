"""pty-based REPL test — review finding 28.

The reviewer's repro: "In a real pty, 5 prompts sent 1 s apart produced 2
replies" (reader threads competing in input()).  With the single-input-owner
design, all 5 prompts must be answered, each exactly once.
"""

import os
import pty
import select
import sys
import tempfile
import time
import unittest
from pathlib import Path

MARFIM_ROOT = Path(__file__).resolve().parents[1]
MOCK_REPLY = b"(mock: no scripted turn left)"


class PtyReplTest(unittest.TestCase):
    def test_five_prompts_one_second_apart_all_answered(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp) / "home"
            home.mkdir()
            (home / "config.toml").write_text(
                'default_provider = "mock"\ndefault_model = "mock-1"\nmode = "bypass"\n')
            env = {**os.environ, "MARFIM_HOME": str(home), "TERM": "dumb",
                   "NO_COLOR": "1", "PYTHONPATH": str(MARFIM_ROOT)}
            for key in ("MARFIM_MODE", "MARFIM_PROVIDER", "MARFIM_MODEL"):
                env.pop(key, None)

            pid, fd = pty.fork()
            if pid == 0:                       # child: run the real CLI
                os.chdir(tmp)
                os.execvpe(sys.executable, [sys.executable, "-m", "marfim"], env)

            def read_all(timeout: float) -> bytes:
                out = b""
                while True:
                    r, _, _ = select.select([fd], [], [], timeout)
                    if not r:
                        break
                    try:
                        chunk = os.read(fd, 65536)
                    except OSError:
                        break
                    if not chunk:
                        break
                    out += chunk
                return out

            try:
                banner = read_all(10)
                self.assertIn(b"Marfim", banner)
                for i in range(1, 6):          # 5 prompts, 1 s apart
                    os.write(fd, f"message number {i}\r".encode())
                    time.sleep(1.0)
                deadline = time.time() + 20
                buf = banner
                while time.time() < deadline:
                    buf += read_all(0.5)
                    if buf.count(MOCK_REPLY) >= 5:
                        break
                replies = buf.count(MOCK_REPLY)
                self.assertEqual(replies, 5,
                                 f"expected 5 replies, got {replies}\n{buf[-1200:]}")
                self.assertNotIn(b"turn crashed", buf)
            finally:
                try:
                    os.write(fd, b"/quit\r")
                except OSError:
                    pass
                time.sleep(0.3)
                try:
                    os.close(fd)
                except OSError:
                    pass
                try:
                    os.waitpid(pid, 0)
                except ChildProcessError:
                    pass


if __name__ == "__main__":
    unittest.main()
