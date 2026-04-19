"""Adapters for the local Rust engine used by the local-first repo workflow."""

from __future__ import annotations

import hashlib
import json
import os
import subprocess
from collections.abc import Callable
from pathlib import Path
from typing import Any

from src.contracts.run_metadata import RunMetadata, build_run_metadata

from .repository_snapshot import LocalRepositorySnapshot, capture_snapshot

ENGINE_MANIFEST_PATH = Path(__file__).resolve().parents[2] / "rust" / "Cargo.toml"
ENGINE_WORKSPACE_PATH = ENGINE_MANIFEST_PATH.parent
ENGINE_BINARY_DEBUG = ENGINE_WORKSPACE_PATH / "target" / "debug" / "aethyme-engine-cli"
ENGINE_BINARY_RELEASE = ENGINE_WORKSPACE_PATH / "target" / "release" / "aethyme-engine-cli"
ENGINE_BINARY_PATH: Path | None = None
CACHE_ROOT = Path(os.getenv("AETHYME_CACHE_DIR", "/tmp/aethyme-cache"))
EngineRunner = Callable[[tuple[str, ...]], str]
SUPPORTED_ENGINE_TRANSPORT_CONFIGS = ("auto", "subprocess", "pyo3")


def _probe_pyo3_transport() -> tuple[bool, str]:
    """Return readiness and detail message for PyO3 transport."""
    try:
        import aethyme_py  # type: ignore[import-not-found]
    except ImportError:
        return False, "module 'aethyme_py' not installed"

    run_engine_command = getattr(aethyme_py, "run_engine_command", None)
    if not callable(run_engine_command):
        return False, "missing callable run_engine_command(args: list[str])"
    return True, "ready"


class EngineError(RuntimeError):
    """Raised when the Rust engine command fails."""


def build_engine_run_metadata(
    snapshot: LocalRepositorySnapshot,
    *,
    phase: str,
    status: str,
    run_id: str | None = None,
    command: list[str] | None = None,
    config: dict[str, object] | None = None,
    finished_at: str | None = None,
) -> RunMetadata:
    """Return versioned metadata for a normal engine invocation."""
    return build_run_metadata(
        snapshot,
        project_root=ENGINE_MANIFEST_PATH.parents[1],
        phase=phase,
        status=status,
        run_id=run_id,
        engine_binary=_metadata_engine_binary(),
        command=command,
        config=config,
        finished_at=finished_at,
    )


def _metadata_engine_binary() -> Path | None:
    """Return binary metadata without forcing a subprocess binary for PyO3 runs."""
    info = engine_runtime_info()
    if info["resolved_transport"] != "subprocess":
        return None
    return ensure_engine_binary()


def engine_runtime_info() -> dict[str, Any]:
    """Return transport/runtime info without triggering a build."""
    raw_transport = os.getenv("AETHYME_ENGINE_TRANSPORT")
    configured_transport = (raw_transport or "auto").strip().lower() or "auto"
    transport_source = "env" if raw_transport is not None and raw_transport.strip() else "default"
    supported_transports = sorted(ENGINE_TRANSPORT_RUNNERS)
    binary_path = _configured_engine_binary_path()
    binary_exists = binary_path.exists()

    if configured_transport == "auto":
        pyo3_ready, pyo3_detail = _probe_pyo3_transport()
        if pyo3_ready:
            resolved_transport = "pyo3"
            transport_ready = True
            transport_detail = "auto->pyo3 (ready)"
        else:
            resolved_transport = "subprocess"
            transport_ready = binary_exists
            fallback_detail = "ready" if binary_exists else "binary not built"
            transport_detail = f"auto->subprocess ({fallback_detail}; pyo3: {pyo3_detail})"
    elif configured_transport == "subprocess":
        resolved_transport = "subprocess"
        transport_ready = binary_exists
        transport_detail = "ready" if binary_exists else "binary not built"
    elif configured_transport == "pyo3":
        resolved_transport = "pyo3"
        transport_ready, transport_detail = _probe_pyo3_transport()
    elif configured_transport in ENGINE_TRANSPORT_RUNNERS:
        resolved_transport = configured_transport
        transport_ready = True
        transport_detail = "ready (custom transport)"
    else:
        resolved_transport = None
        transport_ready = False
        transport_detail = "unsupported transport"

    transport_supported = configured_transport in ENGINE_TRANSPORT_RUNNERS or configured_transport == "auto"

    return {
        "transport": configured_transport,
        "transport_source": transport_source,
        "resolved_transport": resolved_transport,
        "transport_supported": transport_supported,
        "supported_transports": sorted(set(SUPPORTED_ENGINE_TRANSPORT_CONFIGS) | set(ENGINE_TRANSPORT_RUNNERS)),
        "runnable_transports": supported_transports,
        "binary_path": str(binary_path),
        "binary_exists": binary_exists,
        "transport_ready": transport_ready,
        "transport_detail": transport_detail,
    }


def register_engine_transport(name: str, runner: EngineRunner, *, override: bool = False) -> None:
    """Register an engine transport runner."""
    normalized = name.strip().lower()
    if not normalized:
        raise ValueError("Engine transport name must be non-empty.")
    if normalized in ENGINE_TRANSPORT_RUNNERS and not override:
        raise ValueError(
            f"Engine transport {normalized!r} is already registered. "
            "Use override=True to replace it."
        )
    ENGINE_TRANSPORT_RUNNERS[normalized] = runner


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


def _run_pyo3_transport(args: tuple[str, ...]) -> str:
    """Execute engine command through an in-process PyO3 binding."""
    try:
        import aethyme_py  # type: ignore[import-not-found]
    except ImportError as exc:  # pragma: no cover - exercised in tests via message check
        raise EngineError(
            "PyO3 engine transport selected, but module 'aethyme_py' is not available."
        ) from exc

    run_engine_command = getattr(aethyme_py, "run_engine_command", None)
    if not callable(run_engine_command):
        raise EngineError(
            "PyO3 engine transport requires callable 'aethyme_py.run_engine_command(args: list[str])'."
        )

    result = run_engine_command(list(args))
    if not isinstance(result, str):
        raise EngineError("PyO3 engine transport returned non-string output.")
    return result.strip()


ENGINE_TRANSPORT_RUNNERS: dict[str, EngineRunner] = {
    "subprocess": _run_subprocess_transport,
    "pyo3": _run_pyo3_transport,
}


def _run_binary_command(*args: str) -> str:
    info = engine_runtime_info()
    transport = info["resolved_transport"]
    runner = ENGINE_TRANSPORT_RUNNERS.get(transport)
    if runner is None:
        supported = ", ".join(info["runnable_transports"])
        raise EngineError(
            "Unsupported engine transport: "
            f"{info['transport']!r} (resolved={transport!r}). Supported runnable values: {supported!r}."
        )
    return runner(tuple(args))


def _cache_directory(snapshot: LocalRepositorySnapshot) -> Path:
    engine_key = _engine_cache_identity()
    cache_hash = hashlib.sha256(
        f"{snapshot.repo_path}:{snapshot.cache_key}:{engine_key}".encode()
    ).hexdigest()
    cache_dir = CACHE_ROOT / cache_hash
    cache_dir.mkdir(parents=True, exist_ok=True)
    return cache_dir


def _engine_cache_identity() -> str:
    """Return a transport-specific cache identity without forcing unused transports."""
    info = engine_runtime_info()
    transport = info["resolved_transport"]
    if transport == "subprocess":
        binary_path = ensure_engine_binary()
        return f"transport=subprocess:path={binary_path}:mtime={binary_path.stat().st_mtime_ns}"
    if transport == "pyo3":
        return f"transport=pyo3:{_pyo3_cache_identity()}"
    return f"transport={transport!r}:configured={info['transport']!r}:ready={info['transport_ready']!r}"


def _pyo3_cache_identity() -> str:
    """Return a stable-enough identity for the active PyO3 module."""
    try:
        import aethyme_py  # type: ignore[import-not-found]
    except ImportError:
        return "module=missing"

    module_file = getattr(aethyme_py, "__file__", None)
    version = getattr(aethyme_py, "__version__", None)
    parts = [f"version={version!r}"]
    if module_file:
        module_path = Path(str(module_file))
        parts.append(f"path={module_path}")
        if module_path.exists():
            parts.append(f"mtime={module_path.stat().st_mtime_ns}")
    return ":".join(parts)


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


def derived_public_functions(
    repo_path: Path,
    scope: str,
    *,
    include_methods: bool = False,
) -> list[dict[str, Any]]:
    """Return derived public or exported function facts within a scope."""
    snapshot = capture_snapshot(repo_path)
    cache_key = f"facts_public_functions_{_stable_hash(f'{scope}:{include_methods}')}"
    output = _cached_text(
        snapshot,
        cache_key,
        lambda: _run_binary_command(
            "facts-public-functions",
            "--repo",
            str(snapshot.repo_path),
            "--scope",
            scope,
            *(["--include-methods"] if include_methods else []),
        ),
    )
    return json.loads(output)


def derived_function_usage(
    repo_path: Path,
    target: str,
    *,
    boundary: str,
    roots: list[str] | None = None,
) -> dict[str, Any]:
    """Return deterministic usage facts for one function relative to a boundary."""
    snapshot = capture_snapshot(repo_path)
    roots = roots or []
    roots_value = ",".join(roots)
    cache_key = f"facts_function_usage_{_stable_hash(f'{target}:{boundary}:{roots_value}')}"
    output = _cached_text(
        snapshot,
        cache_key,
        lambda: _run_binary_command(
            "facts-function-usage",
            "--repo",
            str(snapshot.repo_path),
            "--target",
            target,
            "--boundary",
            boundary,
            *(["--roots", roots_value] if roots_value else []),
        ),
    )
    return json.loads(output)


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
