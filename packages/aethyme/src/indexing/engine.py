"""Adapters for the local Rust engine used by the local-first repo workflow."""

from __future__ import annotations

import hashlib
import json
import os
import subprocess
from collections.abc import Callable
from pathlib import Path
from typing import Any

from .repository_snapshot import LocalRepositorySnapshot, capture_snapshot

ENGINE_MANIFEST_PATH = Path(__file__).resolve().parents[2] / "rust" / "Cargo.toml"
ENGINE_WORKSPACE_PATH = ENGINE_MANIFEST_PATH.parent
ENGINE_BINARY_DEBUG = ENGINE_WORKSPACE_PATH / "target" / "debug" / "aethyme-engine-cli"
ENGINE_BINARY_RELEASE = ENGINE_WORKSPACE_PATH / "target" / "release" / "aethyme-engine-cli"
ENGINE_BINARY_PATH: Path | None = None
CACHE_ROOT = Path(os.getenv("AETHYME_CACHE_DIR", "/tmp/aethyme-cache"))


class EngineError(RuntimeError):
    """Raised when the Rust engine command fails."""


def engine_runtime_info() -> dict[str, Any]:
    """Minimal runtime info (subprocess-only since the PyO3 retirement).

    Kept solely for the analyze dead-code observability block; retires
    with it when the analyze group flips.
    """
    binary_path = _configured_engine_binary_path()
    return {
        "transport": "subprocess",
        "binary_path": str(binary_path),
        "binary_exists": binary_path.exists(),
    }


def ensure_engine_binary() -> Path:
    """Build the Rust engine binary if it is missing or stale."""
    binary_path = _configured_engine_binary_path()
    needs_build = not binary_path.exists()
    if not needs_build:
        binary_mtime = binary_path.stat().st_mtime_ns
        for source_path in [ENGINE_MANIFEST_PATH, *ENGINE_WORKSPACE_PATH.rglob("*.rs")]:
            if source_path.stat().st_mtime_ns > binary_mtime:
                needs_build = True
                break

    if needs_build:
        command = [
            "cargo",
            "build",
            "--quiet",
            "--manifest-path",
            str(ENGINE_MANIFEST_PATH),
            "--bin",
            "aethyme-engine-cli",
        ]
        if binary_path == ENGINE_BINARY_RELEASE:
            command.insert(2, "--release")
        result = subprocess.run(command, check=False, capture_output=True, text=True)
        if result.returncode != 0:
            raise EngineError(result.stderr.strip() or result.stdout.strip() or "Rust engine build failed")

    if not binary_path.exists():
        raise EngineError(f"Rust engine binary missing after build: {binary_path}")
    return binary_path


def _preferred_engine_binary() -> Path:
    if ENGINE_BINARY_RELEASE.exists() and ENGINE_BINARY_DEBUG.exists():
        if ENGINE_BINARY_RELEASE.stat().st_mtime_ns >= ENGINE_BINARY_DEBUG.stat().st_mtime_ns:
            return ENGINE_BINARY_RELEASE
        return ENGINE_BINARY_DEBUG
    if ENGINE_BINARY_RELEASE.exists():
        return ENGINE_BINARY_RELEASE
    return ENGINE_BINARY_DEBUG


def _configured_engine_binary_path() -> Path:
    """Return an explicit test/config override or the preferred built binary path."""
    if ENGINE_BINARY_PATH is not None:
        return ENGINE_BINARY_PATH
    return _preferred_engine_binary()


def _run_subprocess_transport(args: tuple[str, ...]) -> str:
    """Execute engine command using subprocess transport."""
    binary_path = ensure_engine_binary()
    result = subprocess.run(
        [str(binary_path), *args],
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise EngineError(result.stderr.strip() or result.stdout.strip() or "Rust engine failed")
    return result.stdout.strip()


def _run_binary_command(*args: str) -> str:
    return _run_subprocess_transport(tuple(args))


def _cache_directory(snapshot: LocalRepositorySnapshot) -> Path:
    engine_key = _engine_cache_identity()
    cache_hash = hashlib.sha256(
        f"{snapshot.repo_path}:{snapshot.cache_key}:{engine_key}".encode()
    ).hexdigest()
    cache_dir = CACHE_ROOT / cache_hash
    cache_dir.mkdir(parents=True, exist_ok=True)
    return cache_dir


def _engine_cache_identity() -> str:
    binary_path = ensure_engine_binary()
    return f"transport=subprocess:path={binary_path}:mtime={binary_path.stat().st_mtime_ns}"


def _load_cached_text(cache_path: Path) -> str | None:
    if not cache_path.exists():
        return None
    return cache_path.read_text(encoding="utf-8")


def _store_cached_text(cache_path: Path, payload: str) -> str:
    cache_path.write_text(payload, encoding="utf-8")
    return payload


def _cached_text(snapshot: LocalRepositorySnapshot, name: str, producer: Callable[[], str]) -> str:
    cache_path = _cache_directory(snapshot) / f"{name}.json"
    cached = _load_cached_text(cache_path)
    if cached is not None:
        return cached
    payload = producer()
    return _store_cached_text(cache_path, payload)


def _run_binary_command_with_timeout(
    *args: str,
    timeout_seconds: float | None = None,
) -> str:
    """Run an engine command with an optional timeout for subprocess transport."""
    if timeout_seconds is None:
        return _run_binary_command(*args)

    info = engine_runtime_info()
    if info["resolved_transport"] != "subprocess":
        return _run_binary_command(*args)

    binary_path = ensure_engine_binary()
    try:
        result = subprocess.run(
            [str(binary_path), *args],
            check=False,
            capture_output=True,
            text=True,
            timeout=timeout_seconds,
        )
    except subprocess.TimeoutExpired as exc:
        raise EngineError(
            f"Rust engine timed out after {timeout_seconds:.1f}s: {' '.join(args)}"
        ) from exc
    if result.returncode != 0:
        raise EngineError(result.stderr.strip() or result.stdout.strip() or "Rust engine failed")
    return result.stdout.strip()


def analyze_dead_code(
    repo_path: Path,
    scope: str,
    *,
    roots: list[str] | None = None,
    include_methods: bool = False,
) -> dict[str, Any]:
    """Return a typed dead-code answer with evidence and ambiguity markers."""
    snapshot = capture_snapshot(repo_path)
    roots = roots or []
    roots_value = ",".join(roots)
    cache_key = f"analyze_dead_code_{_stable_hash(f'{scope}:{roots_value}:{include_methods}')}"
    output = _cached_text(
        snapshot,
        cache_key,
        lambda: _run_binary_command(
            "analyze-dead-code",
            "--repo",
            str(snapshot.repo_path),
            "--scope",
            scope,
            *(["--roots", roots_value] if roots_value else []),
            *(["--include-methods"] if include_methods else []),
        ),
    )
    return json.loads(output)


def _stable_hash(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8")).hexdigest()[:16]


def _graph_relation(repo_path: Path, command: str, target: str) -> dict[str, Any]:
    snapshot = capture_snapshot(repo_path)
    cache_key = f"{command}_{_stable_hash(target)}"
    output = _cached_text(
        snapshot,
        cache_key,
        lambda: _run_binary_command(command, "--repo", str(snapshot.repo_path), "--target", target),
    )
    return json.loads(output)
