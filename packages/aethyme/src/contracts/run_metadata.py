"""Versioned run metadata used to trace engine and eval executions."""

from __future__ import annotations

import hashlib
import subprocess
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import TYPE_CHECKING

from src import __version__ as AETHYME_VERSION

from .versions import RUN_METADATA_SCHEMA_VERSION

if TYPE_CHECKING:
    # Import only for annotations: a runtime import here recreates the
    # src.indexing.repository_snapshot -> src.contracts.versions ->
    # src.contracts (package init) -> run_metadata cycle that broke
    # collection after the 2026-07-17 dead-module removal shifted import
    # order (annotations are lazy via `from __future__ import annotations`).
    from src.indexing.repository_snapshot import LocalRepositorySnapshot


def _default_started_at() -> str:
    return datetime.now(UTC).isoformat()


def _get_aethyme_commit(project_root: Path) -> str | None:
    result = subprocess.run(
        ["git", "-C", str(project_root), "rev-parse", "HEAD"],
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        return None
    commit = result.stdout.strip()
    return commit or None


def _hash_config(config: dict[str, object] | None) -> str | None:
    if not config:
        return None
    items = sorted((str(key), repr(value)) for key, value in config.items())
    digest = hashlib.sha256()
    for key, value in items:
        digest.update(key.encode("utf-8"))
        digest.update(b"=")
        digest.update(value.encode("utf-8"))
        digest.update(b"\n")
    return digest.hexdigest()


@dataclass(frozen=True)
class RunMetadata:
    """Trace metadata for a persisted engine or eval run."""

    run_id: str | None
    repo_path: str
    repo_commit: str | None
    repo_dirty: bool
    repo_snapshot_key: str
    repo_fingerprint: str
    repo_file_count: int
    phase: str
    status: str
    started_at: str
    finished_at: str | None
    aethyme_version: str
    aethyme_commit: str | None
    engine_binary: str | None
    engine_binary_mtime_ns: int | None
    config_hash: str | None
    command: list[str] | None = None
    schema_version: str = RUN_METADATA_SCHEMA_VERSION

    def to_dict(self) -> dict[str, object]:
        return {
            "schema_version": self.schema_version,
            "run_id": self.run_id,
            "repo_path": self.repo_path,
            "repo_commit": self.repo_commit,
            "repo_dirty": self.repo_dirty,
            "repo_snapshot_key": self.repo_snapshot_key,
            "repo_fingerprint": self.repo_fingerprint,
            "repo_file_count": self.repo_file_count,
            "phase": self.phase,
            "status": self.status,
            "started_at": self.started_at,
            "finished_at": self.finished_at,
            "aethyme_version": self.aethyme_version,
            "aethyme_commit": self.aethyme_commit,
            "engine_binary": self.engine_binary,
            "engine_binary_mtime_ns": self.engine_binary_mtime_ns,
            "config_hash": self.config_hash,
            "command": list(self.command) if self.command else None,
        }


def build_run_metadata(
    snapshot: LocalRepositorySnapshot,
    *,
    project_root: Path,
    phase: str,
    status: str,
    run_id: str | None = None,
    started_at: str | None = None,
    finished_at: str | None = None,
    engine_binary: Path | None = None,
    command: list[str] | None = None,
    config: dict[str, object] | None = None,
) -> RunMetadata:
    """Build a versioned run metadata record from a repository snapshot."""
    binary_mtime_ns = None
    if engine_binary is not None and engine_binary.exists():
        binary_mtime_ns = engine_binary.stat().st_mtime_ns

    return RunMetadata(
        run_id=run_id,
        repo_path=str(snapshot.repo_path),
        repo_commit=snapshot.commit,
        repo_dirty=snapshot.dirty,
        repo_snapshot_key=snapshot.cache_key,
        repo_fingerprint=snapshot.fingerprint,
        repo_file_count=snapshot.file_count,
        phase=phase,
        status=status,
        started_at=started_at or _default_started_at(),
        finished_at=finished_at,
        aethyme_version=AETHYME_VERSION,
        aethyme_commit=_get_aethyme_commit(project_root),
        engine_binary=str(engine_binary) if engine_binary is not None else None,
        engine_binary_mtime_ns=binary_mtime_ns,
        config_hash=_hash_config(config),
        command=list(command) if command else None,
    )
