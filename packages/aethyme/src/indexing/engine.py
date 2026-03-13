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
ENGINE_BINARY_PATH = ENGINE_BINARY_RELEASE if ENGINE_BINARY_RELEASE.exists() else ENGINE_BINARY_DEBUG
CACHE_ROOT = Path(os.getenv("AETHYME_CACHE_DIR", "/tmp/aethyme-cache"))


class EngineError(RuntimeError):
    """Raised when the Rust engine command fails."""


def ensure_engine_binary() -> Path:
    """Build the Rust engine binary if it is missing or stale."""
    needs_build = not ENGINE_BINARY_PATH.exists()
    if not needs_build:
        binary_mtime = ENGINE_BINARY_PATH.stat().st_mtime_ns
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
        result = subprocess.run(command, check=False, capture_output=True, text=True)
        if result.returncode != 0:
            raise EngineError(result.stderr.strip() or result.stdout.strip() or "Rust engine build failed")

    return ENGINE_BINARY_PATH


def _run_binary_command(*args: str) -> str:
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


def _cache_directory(snapshot: LocalRepositorySnapshot) -> Path:
    engine_key = str(ensure_engine_binary().stat().st_mtime_ns)
    cache_hash = hashlib.sha256(
        f"{snapshot.repo_path}:{snapshot.cache_key}:{engine_key}".encode()
    ).hexdigest()
    cache_dir = CACHE_ROOT / cache_hash
    cache_dir.mkdir(parents=True, exist_ok=True)
    return cache_dir


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


def inspect_repository(repo_path: Path) -> dict[str, Any]:
    """Return the repository map emitted by the Rust engine."""
    snapshot = capture_snapshot(repo_path)
    output = _cached_text(
        snapshot,
        "inspect",
        lambda: _run_binary_command("inspect", "--repo", str(snapshot.repo_path)),
    )
    return json.loads(output)


def inspect_repository_brief(repo_path: Path) -> dict[str, Any]:
    """Return a compact repository overview (areas, signals, entrypoints, key configs)."""
    snapshot = capture_snapshot(repo_path)
    output = _cached_text(
        snapshot,
        "inspect_brief",
        lambda: _run_binary_command("inspect", "--repo", str(snapshot.repo_path), "--mode", "brief"),
    )
    return json.loads(output)


def inspect_repository_structure(repo_path: Path) -> dict[str, Any]:
    """Return a structural repository view (areas, files, configs, docs — no edges/graph)."""
    snapshot = capture_snapshot(repo_path)
    output = _cached_text(
        snapshot,
        "inspect_structure",
        lambda: _run_binary_command("inspect", "--repo", str(snapshot.repo_path), "--mode", "structure"),
    )
    return json.loads(output)


def search_symbol(repo_path: Path, query: str) -> list[dict[str, Any]]:
    """Return symbol search results from the Rust engine."""
    snapshot = capture_snapshot(repo_path)
    cache_key = f"symbol_{_stable_hash(query)}"
    output = _cached_text(
        snapshot,
        cache_key,
        lambda: _run_binary_command("symbol", "--repo", str(snapshot.repo_path), "--query", query),
    )
    return json.loads(output)


def graph_node(repo_path: Path, target: str) -> dict[str, Any]:
    """Return a graph node view for a resolved target."""
    snapshot = capture_snapshot(repo_path)
    cache_key = f"graph_node_{_stable_hash(target)}"
    output = _cached_text(
        snapshot,
        cache_key,
        lambda: _run_binary_command("graph-node", "--repo", str(snapshot.repo_path), "--target", target),
    )
    return json.loads(output)


def graph_children(repo_path: Path, target: str) -> dict[str, Any]:
    """Return structural children for a target."""
    return _graph_relation(repo_path, "graph-children", target)


def graph_parents(repo_path: Path, target: str) -> dict[str, Any]:
    """Return structural parents for a target."""
    return _graph_relation(repo_path, "graph-parents", target)


def graph_callers(repo_path: Path, target: str) -> dict[str, Any]:
    """Return caller relations for a target."""
    return _graph_relation(repo_path, "graph-callers", target)


def graph_callees(repo_path: Path, target: str) -> dict[str, Any]:
    """Return callee relations for a target."""
    return _graph_relation(repo_path, "graph-callees", target)


def graph_docs(repo_path: Path, target: str) -> dict[str, Any]:
    """Return documentation relations for a target."""
    return _graph_relation(repo_path, "graph-docs", target)


def graph_configs(repo_path: Path, target: str) -> dict[str, Any]:
    """Return config relations for a target."""
    return _graph_relation(repo_path, "graph-configs", target)


def graph_expand(repo_path: Path, target: str) -> dict[str, Any]:
    """Return a compact graph slice for iterative navigation."""
    return _graph_relation(repo_path, "graph-expand", target)


def graph_overview(repo_path: Path) -> dict[str, Any]:
    """Return a repo-level navigation overview derived from the graph."""
    snapshot = capture_snapshot(repo_path)
    output = _cached_text(
        snapshot,
        "graph_overview",
        lambda: _run_binary_command("graph-overview", "--repo", str(snapshot.repo_path)),
    )
    return json.loads(output)


def dependency_frontier(repo_path: Path, target: str) -> list[str]:
    """Return dependency frontier values for a symbol or file."""
    snapshot = capture_snapshot(repo_path)
    cache_key = f"deps_{_stable_hash(target)}"
    output = _cached_text(
        snapshot,
        cache_key,
        lambda: _run_binary_command("deps", "--repo", str(snapshot.repo_path), "--target", target),
    )
    return json.loads(output)


def impact_frontier(repo_path: Path, target: str) -> list[str]:
    """Return impact frontier values for a symbol or file."""
    snapshot = capture_snapshot(repo_path)
    cache_key = f"impact_{_stable_hash(target)}"
    output = _cached_text(
        snapshot,
        cache_key,
        lambda: _run_binary_command("impact", "--repo", str(snapshot.repo_path), "--target", target),
    )
    return json.loads(output)


def build_task_pack(repo_path: Path, task: str) -> dict[str, Any]:
    """Return a deterministic task-context pack."""
    snapshot = capture_snapshot(repo_path)
    cache_key = f"pack_{_stable_hash(task)}"
    output = _cached_text(
        snapshot,
        cache_key,
        lambda: _run_binary_command("pack", "--repo", str(snapshot.repo_path), "--task", task),
    )
    return json.loads(output)


def build_task_context(repo_path: Path, task: str, content_budget: int = 80_000) -> dict[str, Any]:
    """Return a task-context pack including file contents."""
    snapshot = capture_snapshot(repo_path)
    cache_key = f"context_{_stable_hash(task)}_{content_budget}"
    output = _cached_text(
        snapshot,
        cache_key,
        lambda: _run_binary_command(
            "context", "--repo", str(snapshot.repo_path),
            "--task", task, "--content-budget", str(content_budget),
        ),
    )
    return json.loads(output)


def task_anchors(repo_path: Path, task: str) -> dict[str, Any]:
    """Return task anchors derived from the graph."""
    return _task_view(repo_path, "task-anchors", task)


def task_scope(repo_path: Path, task: str) -> dict[str, Any]:
    """Return task scope derived from the graph."""
    return _task_view(repo_path, "task-scope", task)


def task_next(repo_path: Path, task: str) -> dict[str, Any]:
    """Return the next navigation steps for a task."""
    return _task_view(repo_path, "task-next", task)


def task_expand(repo_path: Path, target: str) -> dict[str, Any]:
    """Expand a graph node into related navigation context."""
    snapshot = capture_snapshot(repo_path)
    cache_key = f"task_expand_{_stable_hash(target)}"
    output = _cached_text(
        snapshot,
        cache_key,
        lambda: _run_binary_command("task-expand", "--repo", str(snapshot.repo_path), "--target", target),
    )
    return json.loads(output)


def activate(repo_path: Path, task: str) -> dict[str, Any]:
    """Return the activation map for a task, showing which graph nodes are relevant."""
    snapshot = capture_snapshot(repo_path)
    cache_key = f"activate_{_stable_hash(task)}"
    output = _cached_text(
        snapshot,
        cache_key,
        lambda: _run_binary_command("activate", "--repo", str(snapshot.repo_path), "--task", task),
    )
    return json.loads(output)


def activate_from(repo_path: Path, seed: str, hops: int = 3) -> dict[str, Any]:
    """Return a focused activation wave from a single seed node."""
    snapshot = capture_snapshot(repo_path)
    cache_key = f"activate_from_{_stable_hash(seed)}_{hops}"
    output = _cached_text(
        snapshot,
        cache_key,
        lambda: _run_binary_command(
            "activate-from", "--repo", str(snapshot.repo_path), "--seed", seed, "--hops", str(hops),
        ),
    )
    return json.loads(output)


def explain_task(repo_path: Path, task: str) -> str:
    """Return a deterministic text explanation from the Rust engine."""
    snapshot = capture_snapshot(repo_path)
    cache_key = f"explain_{_stable_hash(task)}"
    return _cached_text(
        snapshot,
        cache_key,
        lambda: _run_binary_command("explain", "--repo", str(snapshot.repo_path), "--task", task),
    )


def workspace_inspect(repo_paths: list[Path]) -> dict[str, Any]:
    """Return a workspace graph analyzing relationships between multiple repositories."""
    repos_str = ",".join(str(p.resolve()) for p in repo_paths)
    output = _run_binary_command("workspace-inspect", "--repos", repos_str)
    return json.loads(output)


def workspace_blast_radius(
    repo_paths: list[Path], repo_name: str, file_path: str
) -> list[dict[str, Any]]:
    """Return cross-repo blast radius for a file in a workspace."""
    repos_str = ",".join(str(p.resolve()) for p in repo_paths)
    output = _run_binary_command(
        "workspace-blast-radius",
        "--repos", repos_str,
        "--repo", repo_name,
        "--file", file_path,
    )
    return json.loads(output)


def warm_repository(repo_path: Path) -> None:
    """Pre-build the repository map cache by invoking the warm command."""
    _run_binary_command("warm", "--repo", str(repo_path))


def clear_repository_cache(repo_path: Path) -> None:
    """Remove cached engine artifacts for the current repository snapshot."""
    snapshot = capture_snapshot(repo_path)
    cache_dir = _cache_directory(snapshot)
    for child in cache_dir.iterdir():
        if child.is_file():
            child.unlink()


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


def _task_view(repo_path: Path, command: str, task: str) -> dict[str, Any]:
    snapshot = capture_snapshot(repo_path)
    cache_key = f"{command}_{_stable_hash(task)}"
    output = _cached_text(
        snapshot,
        cache_key,
        lambda: _run_binary_command(command, "--repo", str(snapshot.repo_path), "--task", task),
    )
    return json.loads(output)
