"""Session persistence — JSONL rollouts, date-sharded.

Codex doctrine (human-readable ordinal JSONL) + goose ideas (usage ledger,
resume) + Oh My Pi (branch-lite fork).  One line per record:
  {"t":"meta"|"message"|"todo"|"summary"|"truncate"|"compact"|"usage"|
   "grant"|"snapshot"|"file_create", ...}

Review fixes:
- finding 21: `summary`+`truncate` (and the newer single `compact`) records
  are REPLAYED on load, so compaction and /clear survive resume.
- finding 26: todo writes update memory and file together (set_todos).
- finding 31: session files are 0600; snapshots carry a batch id; rewind
  restores absolute-path targets correctly AND deletes files the agent
  created (file_create records) within the rewound batches.
"""

from __future__ import annotations

import json
import os
import shutil
import time
from dataclasses import dataclass, field
from pathlib import Path

from marfim.util import gen_id, json_dumps, now_iso, sha_short, truncate_tail


def _sessions_root(session_dir: str) -> Path:
    return Path(session_dir).expanduser()


def _summary_message(summary: str) -> dict:
    """The canonical compaction boundary message (must match the writer)."""
    return {"role": "user",
            "content": f"<compaction-summary>\n{summary}\n</compaction-summary>\n\n"
                       f"Continue from this summary. The messages above the summary "
                       f"were compressed away; the tail below is verbatim."}


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
    snapshots: list = field(default_factory=list)     # {"rel","hash","snap","batch"}
    creations: list = field(default_factory=list)     # {"rel","batch"}
    batch: int = 0

    # ------------------------------------------------------------------ io

    @classmethod
    def create(cls, session_dir: str, project_root: str, title: str = "") -> "Session":
        sid = time.strftime("%Y%m%d") + "-" + gen_id("s")
        path = _sessions_root(session_dir) / time.strftime("%Y") / time.strftime("%m") / f"{sid}.jsonl"
        path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        # 0600: session transcripts may quote secret-adjacent material (finding 31)
        fd = os.open(str(path), os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        os.close(fd)
        session = cls(sid=sid, path=path, project_root=project_root, title=title)
        session.record("meta", {"created": now_iso(), "project_root": project_root,
                                "title": title, "version": 1})
        return session

    @classmethod
    def load(cls, path: Path) -> "Session":
        session = cls(sid=path.stem, path=path)
        first_user = ""
        pending_summary: str | None = None
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
            elif t == "file_create":
                session.creations.append(rec)
            elif t == "summary":
                pending_summary = rec.get("summary", "")
            elif t == "compact":
                keep = int(rec.get("keep", 1))
                summary = rec.get("summary", "")
                if summary:
                    kept = session.messages[-(keep - 1):] if keep > 1 else []
                    session.messages = [_summary_message(summary)] + kept
                else:
                    session.messages = session.messages[:keep]
                pending_summary = None
            elif t == "truncate":
                keep = int(rec.get("keep", 0))
                if keep == 0:
                    session.messages = []          # /clear (finding 21)
                elif pending_summary:
                    kept = session.messages[-(keep - 1):] if keep > 1 else []
                    session.messages = [_summary_message(pending_summary)] + kept
                    pending_summary = None
                else:
                    session.messages = session.messages[:keep]
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

    def set_todos(self, items: list) -> None:
        """Memory and file together (review finding 26)."""
        self.todos = list(items)
        self.record("todo", {"items": list(items)})

    def last_todo(self) -> list:
        return self.todos

    def next_batch(self) -> int:
        self.batch += 1
        return self.batch

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
        path = Path(path)
        try:
            rel = str(path.resolve().relative_to(project_root.resolve()))
        except ValueError:
            # absolute paths restore to their absolute location (finding 31:
            # they were previously rewritten into the project wrongly)
            rel = str(path.resolve())
        try:
            content = path.read_bytes()
        except OSError:
            return None
        digest = sha_short(content, 16)
        snap_dir = self.path.parent / f"{self.sid}.snaps"
        snap_dir.mkdir(parents=True, exist_ok=True, mode=0o700)
        safe_rel = rel.replace("/", "__").replace("..", "__")
        snap_name = f"{digest}__{safe_rel}"
        snap_path = snap_dir / snap_name
        if not snap_path.exists():
            fd = os.open(str(snap_path), os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(fd, "wb") as fh:
                fh.write(content)
        self.snapshots.append({"rel": rel, "hash": digest, "snap": snap_name,
                               "batch": self.batch})
        self.record("snapshot", {"rel": rel, "hash": digest, "snap": snap_name,
                                 "batch": self.batch})
        return snap_name

    def record_creation(self, path: Path, project_root: Path) -> None:
        """Track files the agent CREATED so rewind can delete them (finding 31)."""
        try:
            rel = str(Path(path).resolve().relative_to(project_root.resolve()))
        except ValueError:
            rel = str(Path(path).resolve())
        self.creations.append({"rel": rel, "batch": self.batch})
        self.record("file_create", {"rel": rel, "batch": self.batch})

    def rewind(self, project_root: Path, steps: int = 1) -> list[str]:
        """Restore files from the last `steps` checkpoint batches; delete files
        the agent created in those batches (finding 31)."""
        if not self.snapshots and not self.creations:
            return []
        batches = sorted({int(s.get("batch", 0)) for s in self.snapshots} |
                         {int(c.get("batch", 0)) for c in self.creations})
        if not batches:
            return []
        target_batches = set(batches[-steps:]) if steps >= 1 else set()
        in_scope_snaps = [s for s in self.snapshots if int(s.get("batch", 0)) in target_batches]
        # most recent snapshot version wins per rel
        targets: dict[str, dict] = {}
        for snap in in_scope_snaps:
            targets[snap["rel"]] = snap
        restored: list[str] = []
        snap_dir = self.path.parent / f"{self.sid}.snaps"
        for rel, snap in targets.items():
            src = snap_dir / snap["snap"]
            dst = Path(rel) if os.path.isabs(rel) else (project_root / rel).resolve()
            if src.exists():
                dst.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(src, dst)
                restored.append(rel)
        # delete files the agent created in the rewound batches that have no
        # pre-existing snapshot (there was nothing to restore over)
        created = {c["rel"] for c in self.creations
                   if int(c.get("batch", 0)) in target_batches}
        for rel in sorted(created):
            if rel in targets:
                continue
            p = Path(rel) if os.path.isabs(rel) else (project_root / rel).resolve()
            try:
                if p.exists():
                    p.unlink()
                    restored.append(f"-{rel}")
            except OSError:
                continue
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
