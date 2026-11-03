"""Project-config trust gate — ported design from the archived Node tree
(`src/core/trust.js`), clean-room reimplementation (review finding 2).

A hostile repository can ship `.marfim/config.toml` that would otherwise seize
control of the harness: `mode="bypass"`, attacker `[hooks]`, attacker
`[providers.*].base_url` (API-key exfiltration), attacker `[mcp_servers]`,
disabled `protect_configs`, widened `sandbox`.  Until the user explicitly
trusts a project config, Marfim loads ONLY the tightening subset of it
(`permissions.deny` and `permissions.ask`) and ignores everything else.

Trust is keyed by the project's absolute path plus the sha256 of the config
file — any later edit invalidates the trust and re-arms the gate.
"""

from __future__ import annotations

import hashlib
import json
import os
import time
from pathlib import Path

from marfim.config import marfim_home

TRUST_FILE = "trust.json"


def config_digest(path: Path) -> str:
    """sha256 of the config file bytes; unreadable files hash to the path."""
    try:
        return hashlib.sha256(Path(path).read_bytes()).hexdigest()
    except OSError:
        return hashlib.sha256(str(path).encode()).hexdigest()


def trust_store_path() -> Path:
    return marfim_home() / TRUST_FILE


class TrustStore:
    """Persistent record of trusted project configs.

    Format: {"version": 1, "projects": {"/abs/path": {"digest": "...",
    "trusted_at": "...", "config": ".marfim/config.toml"}}}
    """

    def __init__(self, path: Path | None = None):
        self.path = path or trust_store_path()
        self._data: dict = self._load()

    def _load(self) -> dict:
        try:
            raw = json.loads(self.path.read_text(encoding="utf-8"))
            if isinstance(raw, dict) and isinstance(raw.get("projects"), dict):
                return raw
        except (OSError, ValueError):
            pass
        return {"version": 1, "projects": {}}

    def _save(self) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        fd = os.open(str(self.path), os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as fh:
            json.dump(self._data, fh, indent=1, sort_keys=True)

    def is_trusted(self, project_root: Path, digest: str) -> bool:
        entry = self._data["projects"].get(str(Path(project_root).resolve()))
        return bool(entry) and entry.get("digest") == digest

    def trust(self, project_root: Path, digest: str, config_name: str = "") -> None:
        self._data["projects"][str(Path(project_root).resolve())] = {
            "digest": digest,
            "trusted_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "config": config_name or ".marfim/config.toml",
        }
        self._save()

    def revoke(self, project_root: Path) -> bool:
        key = str(Path(project_root).resolve())
        if key in self._data["projects"]:
            del self._data["projects"][key]
            self._save()
            return True
        return False

    def list_trusted(self) -> dict:
        return dict(self._data["projects"])


# Keys a project config may set when NOT yet trusted: only the ones that can
# make the harness STRICTER.  Everything else (mode, allow, hooks, mcp_servers,
# providers, sandbox, protect_configs, skills/commands dirs, writable_dirs,
# session_dir) is ignored until trust is established.
TIGHTENING_ONLY_KEYS = {"permissions.deny", "permissions.ask"}


def untrusted_notice(root: Path, config_name: str) -> str:
    return (
        f"untrusted project config: {config_name} under {root} is NOT trusted "
        f"(digest mismatch or first use); only its deny/ask rules were applied — "
        f"hooks, mcp_servers, providers, mode, allow, sandbox and protect_configs "
        f"were ignored. Review the file, then run: marfim trust add")
