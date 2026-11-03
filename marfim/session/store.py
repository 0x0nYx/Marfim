"""Session persistence — JSONL rollouts, date-sharded.

Codex doctrine (human-readable ordinal JSONL) + goose ideas (usage ledger,
resume) + Oh My Pi (branch-lite fork).  One line per record:
  {"t":"meta"|"message"|"todo"|"summary"|"usage"|"grant"|"snapshot", ...}
"""

from __future__ import annotations

import json
import shutil
import time
from dataclasses import dataclass, field
from pathlib import Path

from marfim.util import gen_id, json_dumps, now_iso, sha_short, truncate_tail


def _sessions_root(session_dir: str) -> Path:
    return Path(session_dir).expanduser()


@dataclass
class Session:
    sid: str
    path: Path
    project_root: str = ""
    title: str = ""
    messages: list = field(default_factory=list)
    todos: list = field(default_factory=list)
    grants: list = field(default_factory=list)
    usage_in: int = 0
    usage_out: int = 0
    snapshots: list = field(default_factory=list)  # {"rel","hash","snap_path"}

    # ------------------------------------------------------------------ io

    @classmethod
    def create(cls, session_dir: str, project_root: str, title: str = "") -> "Session":
        sid = time.strftime("%Y%m%d") + "-" + gen_id("s")
        path = _sessions_root(session_dir) / time.strftime("%Y") / time.strftime("%m") / f"{sid}.jsonl"
        path.parent.mkdir(parents=True, exist_ok=True)
        session = cls(sid=sid, path=path, project_root=project_root, title=title)
        session.record("meta", {"created": now_iso(), "project_root": project_root,
                                "title": title, "version": 1})
        return session

    @classmethod
    def load(cls, path: Path) -> "Session":
        session = cls(sid=path.stem, path=path)
        first_user = ""
        for line in path.read_text(encoding="utf-8").splitlines():
            if not line.strip():
                continue
            try:
                rec = json.loads(line)
            except json.JSONDecodeError:
                continue
            t = rec.get("t")
            if t == "meta":
                session.project_root = rec.get("project_root", "")
                session.title = rec.get("title", "")
            elif t == "message":
                session.messages.append(rec.get("message", {}))
                if not first_user and rec.get("message", {}).get("role") == "user":
                    c = rec["message"].get("content")
                    if isinstance(c, str):
                        first_user = c
            elif t == "todo":
                session.todos = rec.get("items", [])
            elif t == "grant":
                if rec.get("rule") and rec["rule"] not in session.grants:
                    session.grants.append(rec["rule"])
            elif t == "usage":
                session.usage_in += rec.get("input", 0)
                session.usage_out += rec.get("output", 0)
            elif t == "snapshot":
                session.snapshots.append(rec)
        if not session.title and first_user:
            session.title = truncate_tail(first_user.replace("\n", " ").strip(), 60)
        return session

    def record(self, kind: str, data: dict) -> None:
        rec = {"t": kind, "ts": now_iso(), **data}
        with self.path.open("a", encoding="utf-8") as fh:
            fh.write(json_dumps(rec) + "\n")

    def append_message(self, message: dict) -> None:
        self.messages.append(message)
        self.record("message", {"message": message})

    def add_usage(self, usage) -> None:
        self.usage_in += usage.input_tokens
        self.usage_out += usage.output_tokens
        self.record("usage", {"input": usage.input_tokens, "output": usage.output_tokens})

    def last_todo(self) -> list:
        return self.todos

    # ------------------------------------------------------------ lifecycle

    def fork(self, session_dir: str) -> "Session":
        new = Session.create(session_dir, self.project_root,
                             title=f"fork of {self.sid}")
        for msg in self.messages:
            new.append_message(msg)
        new.todos = list(self.todos)
        return new

    def export_markdown(self) -> str:
        lines = [f"# Session {self.sid}", f"- project: {self.project_root}",
                 f"- title: {self.title}", ""]
        for msg in self.messages:
            role = msg.get("role", "?")
            content = msg.get("content", "")
            if isinstance(content, list):
                content = json.dumps(content, ensure_ascii=False, default=str)
            if msg.get("tool_calls"):
                calls = ", ".join(tc.get("name", "?") for tc in msg["tool_calls"])
                lines.append(f"**assistant** (tools: {calls})")
                if content:
                    lines.append(content)
            elif role == "tool":
                lines.append(f"  → tool result: {truncate_tail(str(content), 400)}")
            else:
                lines.append(f"**{role}**: {content}")
            lines.append("")
        if self.usage_in or self.usage_out:
            lines.append(f"_usage: {self.usage_in} in / {self.usage_out} out tokens_")
        return "\n".join(lines)

    # ---------------------------------------------------------- checkpoints

    def snapshot_file(self, path: Path, project_root: Path) -> str | None:
        """Copy a file into the session's snapshot dir before mutation
        (Claude Code checkpoint doctrine). Returns snapshot relative name."""
        try:
            rel = str(path.resolve().relative_to(project_root.resolve()))
        except ValueError:
            rel = path.name
        try:
            content = path.read_bytes()
        except OSError:
            return None
        digest = sha_short(content, 16)
        snap_dir = self.path.parent / f"{self.sid}.snaps"
        snap_dir.mkdir(parents=True, exist_ok=True)
        safe_rel = rel.replace("/", "__").replace("..", "__")
        snap_name = f"{digest}__{safe_rel}"
        snap_path = snap_dir / snap_name
        if not snap_path.exists():
            snap_path.write_bytes(content)
        self.snapshots.append({"rel": rel, "hash": digest, "snap": snap_name})
        self.record("snapshot", {"rel": rel, "hash": digest, "snap": snap_name})
        return snap_name

    def rewind(self, project_root: Path, steps: int = 1) -> list[str]:
        """Restore files from the last `steps` snapshot batch (one batch =
        consecutive snapshots recorded in the same second-bucket)."""
        if not self.snapshots:
            return []
        recent = self.snapshots[-50:]
        # take the last `steps` distinct file targets, most recent wins
        targets: dict[str, dict] = {}
        for snap in reversed(recent):
            if snap["rel"] not in targets:
                targets[snap["rel"]] = snap
            if len(targets) >= steps * 8 and steps <= 1:
                break
        restored = []
        snap_dir = self.path.parent / f"{self.sid}.snaps"
        for rel, snap in targets.items():
            src = snap_dir / snap["snap"]
            dst = (project_root / rel).resolve()
            if src.exists() and dst.parent.exists():
                shutil.copy2(src, dst)
                restored.append(rel)
        return restored


def list_sessions(session_dir: str, project_root: str | None = None,
                  limit: int = 20) -> list[Session]:
    root = _sessions_root(session_dir)
    if not root.exists():
        return []
    found: list[Session] = []
    for path in sorted(root.rglob("*.jsonl"), key=lambda p: p.stat().st_mtime, reverse=True):
        try:
            s = Session.load(path)
        except OSError:
            continue
        if project_root and s.project_root and s.project_root != project_root:
            continue
        found.append(s)
        if len(found) >= limit:
            break
    return found


def find_session(session_dir: str, ref: str) -> Session | None:
    """Resolve a session by id, prefix, or 'last'."""
    if ref == "last":
        sessions = list_sessions(session_dir, limit=1)
        return sessions[0] if sessions else None
    root = _sessions_root(session_dir)
    exact = list(root.rglob(f"*{ref}*.jsonl"))
    if exact:
        matches = sorted(exact, key=lambda p: p.stat().st_mtime, reverse=True)
        return Session.load(matches[0])
    return None
