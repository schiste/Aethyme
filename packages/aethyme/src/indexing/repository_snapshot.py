"""Local repository snapshot helpers for the local-first workflow."""

from __future__ import annotations

import hashlib
import subprocess
from dataclasses import dataclass
from pathlib import Path

EXCLUDED_DIRS = {
    ".git",
    ".venv",
    "venv",
    "node_modules",
    "dist",
    "build",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    "target",
}


@dataclass(frozen=True)
class LocalRepositorySnapshot:
    repo_path: Path
    repo_name: str
    commit: str | None
    fingerprint: str
    file_count: int
    dirty: bool

    @property
    def cache_key(self) -> str:
        """Return the stable cache key for the current repository snapshot."""
        return self.commit if self.commit and not self.dirty else self.fingerprint


def capture_snapshot(repo_path: Path) -> LocalRepositorySnapshot:
    """Capture a stable local repository snapshot."""
    resolved = repo_path.expanduser().resolve()
    commit = None
    dirty = False
    git_dir = resolved / ".git"
    if git_dir.exists():
        commit, dirty, fingerprint, file_count = _git_snapshot_metadata(resolved)
    else:
        fingerprint, file_count = _fingerprint_repository(resolved)
    return LocalRepositorySnapshot(
        repo_path=resolved,
        repo_name=resolved.name,
        commit=commit,
        fingerprint=fingerprint,
        file_count=file_count,
        dirty=dirty,
    )


def _git_snapshot_metadata(repo_path: Path) -> tuple[str | None, bool, str, int]:
    commit = None
    commit_result = subprocess.run(
        ["git", "-C", str(repo_path), "rev-parse", "HEAD"],
        check=False,
        capture_output=True,
        text=True,
    )
    if commit_result.returncode == 0:
        commit = commit_result.stdout.strip() or None

    status_result = subprocess.run(
        ["git", "-C", str(repo_path), "status", "--porcelain", "--untracked-files=all"],
        check=False,
        capture_output=True,
        text=True,
    )
    status_output = status_result.stdout.splitlines() if status_result.returncode == 0 else []
    dirty = bool(status_output)

    files_result = subprocess.run(
        ["git", "-C", str(repo_path), "ls-files", "--cached", "--others", "--exclude-standard"],
        check=False,
        capture_output=True,
        text=True,
    )
    file_paths = [line for line in files_result.stdout.splitlines() if line] if files_result.returncode == 0 else []
    file_count = len(file_paths)

    digest = hashlib.sha256()
    digest.update((commit or "no-commit").encode())
    for line in status_output:
        digest.update(line.encode())
    if not status_output:
        digest.update(b"clean")
    return commit, dirty, digest.hexdigest(), file_count


def _fingerprint_repository(repo_path: Path) -> tuple[str, int]:
    digest = hashlib.sha256()
    file_count = 0
    for file_path in iter_repository_files(repo_path):
        relative = file_path.relative_to(repo_path).as_posix()
        stat_result = file_path.stat()
        digest.update(relative.encode("utf-8"))
        digest.update(str(stat_result.st_size).encode("utf-8"))
        digest.update(str(stat_result.st_mtime_ns).encode("utf-8"))
        file_count += 1
    return digest.hexdigest(), file_count


def iter_repository_files(repo_path: Path) -> list[Path]:
    """Return deterministic repository files for snapshotting."""
    files: list[Path] = []
    for path in sorted(repo_path.rglob("*")):
        if not path.is_file():
            continue
        relative_parts = path.relative_to(repo_path).parts
        if any(part in EXCLUDED_DIRS for part in relative_parts):
            continue
        files.append(path)
    return files
