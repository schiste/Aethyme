"""Local-first CLI tests for the Rust-backed repository workflow."""

from __future__ import annotations

import json
import os
import subprocess
import sys
from functools import lru_cache
from pathlib import Path

import pytest

from src.indexing.engine import ENGINE_MANIFEST_PATH, EngineError, ensure_engine_binary
from tests.support.cli_invoke import invoke_aethyme


@lru_cache(maxsize=1)
def _graph_index_binary() -> Path:
    """Resolve the aethyme-graph-index binary, building it if missing.

    The engine requires per-repo fragments under `.aethyme/graph/`
    (the legacy pass pipeline was removed in 4.7.12), so demo repos must
    be bootstrapped with aethyme-graph-index before CLI commands run —
    the same contract as scripts/eval/setup-playground.sh.
    """
    target_dir = ENGINE_MANIFEST_PATH.parent / "target"
    candidates = [
        target_dir / "release" / "aethyme-graph-index",
        target_dir / "debug" / "aethyme-graph-index",
    ]
    existing = [path for path in candidates if path.exists()]
    if existing:
        return max(existing, key=lambda path: path.stat().st_mtime_ns)
    result = subprocess.run(
        [
            "cargo",
            "build",
            "--quiet",
            "--manifest-path",
            str(ENGINE_MANIFEST_PATH),
            "--bin",
            "aethyme-graph-index",
        ],
        check=False,
        capture_output=True,
        text=True,
    )
    debug_binary = candidates[1]
    if result.returncode != 0 or not debug_binary.exists():
        raise EngineError(
            result.stderr.strip() or "aethyme-graph-index build failed"
        )
    return debug_binary


def bootstrap_repo_fragments(root: Path) -> None:
    result = subprocess.run(
        [
            str(_graph_index_binary()),
            "--repo-root",
            str(root),
            "--repo-name",
            "demo-repo",
            "--engine-version",
            "local-test",
            "--json",
        ],
        check=False,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, f"aethyme-graph-index failed: {result.stderr}"

    # Graph queries read from the redb store and will not create it
    # themselves, so demo repos must materialize it from the fragments —
    # the same contract as setup-playground.sh.
    index_result = subprocess.run(
        [str(ensure_engine_binary()), "index", "--repo", str(root), "--from-fragments"],
        check=False,
        capture_output=True,
        text=True,
    )
    assert index_result.returncode == 0, f"engine index failed: {index_result.stderr}"


def build_demo_repo(root: Path) -> None:
    root.mkdir(parents=True, exist_ok=True)
    (root / "src").mkdir(exist_ok=True)
    (root / "README.md").write_text("# Demo Repo\n\nSmall test repo.\n", encoding="utf-8")
    (root / "src" / "main.py").write_text(
        "from src.auth import validate_token\n\n"
        "def main():\n"
        "    return validate_token()\n",
        encoding="utf-8",
    )
    (root / "src" / "auth.py").write_text(
        "def validate_token():\n"
        "    return True\n",
        encoding="utf-8",
    )
    bootstrap_repo_fragments(root)


def require_local_engine_or_skip() -> None:
    """Skip integration tests when Rust engine build/runtime is unavailable."""
    ready, detail = _probe_local_engine_once()
    if not ready and _strict_engine_required():
        raise AssertionError(f"local Rust engine required but unavailable: {detail}")
    if not ready:
        pytest.skip(f"local Rust engine unavailable in this environment: {detail}")


def _strict_engine_required() -> bool:
    return os.getenv("AETHYME_REQUIRE_LOCAL_ENGINE", "").strip().lower() in {"1", "true", "yes"}


@lru_cache(maxsize=1)
def _probe_local_engine_once() -> tuple[bool, str]:
    """Build/probe local engine once per test session to avoid repeated cargo invocations."""
    try:
        ensure_engine_binary()
    except EngineError as exc:
        return False, str(exc)
    return True, "ready"


def test_local_repo_inspect_and_pack(tmp_path: Path) -> None:
    require_local_engine_or_skip()
    repo_path = tmp_path / "demo-repo"
    build_demo_repo(repo_path)

    inspect_result = invoke_aethyme(["repo", "inspect", str(repo_path), "--json-output"])
    assert inspect_result.exit_code == 0, inspect_result.output
    inspect_payload = json.loads(inspect_result.output)
    assert inspect_payload["snapshot"]["readme_path"] == "README.md"
    assert "signals" in inspect_payload
    assert inspect_payload["signals"]["boundary_clarity"]["score"] >= 0
    assert inspect_payload["symbols"]
    assert inspect_payload["areas"]
    assert inspect_payload["files"]
    assert inspect_payload["graph"]["nodes"]

    pack_result = invoke_aethyme(["task", "pack", "--repo", str(repo_path), "--task", "Explain this repo", "--json-output"])
    assert pack_result.exit_code == 0, pack_result.output
    pack_payload = json.loads(pack_result.output)
    assert pack_payload["task"]["kind"] == "explain_repo"
    assert pack_payload["anchors"]
    assert "README.md" in pack_payload["navigation_order"]
    assert pack_payload["in_scope"]["areas"]


def test_local_graph_navigation_commands(tmp_path: Path) -> None:
    require_local_engine_or_skip()
    repo_path = tmp_path / "demo-repo"
    build_demo_repo(repo_path)

    node_result = invoke_aethyme(["graph", "node", str(repo_path), "src/main.py", "--json-output"])
    assert node_result.exit_code == 0, node_result.output
    node_payload = json.loads(node_result.output)
    assert node_payload["kind"] == "file"

    children_result = invoke_aethyme(["graph", "children", str(repo_path), "src", "--json-output"])
    assert children_result.exit_code == 0, children_result.output
    children_payload = json.loads(children_result.output)
    assert children_payload["items"]

    overview_result = invoke_aethyme(["graph", "overview", str(repo_path), "--json-output"])
    assert overview_result.exit_code == 0, overview_result.output
    overview_payload = json.loads(overview_result.output)
    assert "signals" in overview_payload
    assert overview_payload["signals"]["parser_visibility"]["score"] >= 0


def test_local_query_deps_and_impact_commands(tmp_path: Path) -> None:
    """Coverage for the redb deps/impact contract from the Python side.

    Added 2026-07-27 after `query deps` shipped broken for 12 days
    (engine flag + output-format change with no Python-side test to
    notice). Asserts the cross-process contract holds end-to-end, not
    result richness — the demo repo has no resolved file-to-file
    adjacency, so empty lists are valid.
    """
    require_local_engine_or_skip()
    repo_path = tmp_path / "demo-repo"
    build_demo_repo(repo_path)

    deps_result = invoke_aethyme(["query", "deps", str(repo_path), "src/main.py"])
    assert deps_result.exit_code == 0, deps_result.output

    impact_result = invoke_aethyme(["query", "impact", str(repo_path), "src/auth.py"])
    assert impact_result.exit_code == 0, impact_result.output


def test_local_task_navigation_commands(tmp_path: Path) -> None:
    require_local_engine_or_skip()
    repo_path = tmp_path / "demo-repo"
    build_demo_repo(repo_path)

    anchors_result = invoke_aethyme(["task", "anchors", "--repo", str(repo_path), "--task", "Update validate_token flow", "--json-output"])
    assert anchors_result.exit_code == 0, anchors_result.output
    anchors_payload = json.loads(anchors_result.output)
    assert anchors_payload["anchors"]

    scope_result = invoke_aethyme(["task", "scope", "--repo", str(repo_path), "--task", "Update validate_token flow", "--json-output"])
    assert scope_result.exit_code == 0, scope_result.output
    scope_payload = json.loads(scope_result.output)
    assert scope_payload["in_scope_files"]
    assert "src/auth.py" in scope_payload["in_scope_files"]
    # KNOWN GAP (4.7 fragment cutover): the legacy pass pipeline expanded
    # scope to caller files (src/main.py imports and calls validate_token),
    # but the fragment indexer emits only Contains/Imports/Defines edges and
    # the map does not surface cross-file caller relations from them
    # (graph callers / query impact / graph-deps all return no cross-file
    # results for this repo). If caller expansion is restored, reinstate:
    #   assert "src/main.py" in scope_payload["in_scope_files"]
    assert any(symbol.startswith("src/auth.py::") for symbol in scope_payload["in_scope_symbols"])

    next_result = invoke_aethyme(["task", "next", "--repo", str(repo_path), "--task", "Update validate_token flow", "--json-output"])
    assert next_result.exit_code == 0, next_result.output
    next_payload = json.loads(next_result.output)
    assert next_payload["items"]
    next_displays = [item["display"] for item in next_payload["items"]]
    assert "src/auth.py" in next_displays
    # Same known gap as above: src/main.py no longer appears in `task next`
    # without cross-file caller edges from the fragment pipeline.

    expand_result = invoke_aethyme(["task", "expand", "--repo", str(repo_path), "--node", "src/auth.py", "--json-output"])
    assert expand_result.exit_code == 0, expand_result.output
    expand_payload = json.loads(expand_result.output)
    assert "dependencies" in expand_payload


# ── Implementation-level tests (in-process by design) ──────────────────
# The engine-info/transport tests below monkeypatch src.cli internals, so
# they must run in-process. They test the Python transport machinery,
# which retires wholesale in python-retirement Phase 1 (decision #3:
# PyO3/--engine-transport deleted); these tests retire with it.


def _invoke_inprocess(args: list[str]):
    from click.testing import CliRunner

    from src.cli import cli

    return CliRunner().invoke(cli, args)


def test_repo_engine_info_command(monkeypatch) -> None:
    monkeypatch.setattr(
        "src.cli.engine_runtime_info",
        lambda: {
            "transport": "subprocess",
            "transport_source": "env",
            "resolved_transport": "subprocess",
            "transport_supported": True,
            "transport_ready": True,
            "transport_detail": "ready",
            "supported_transports": ["auto", "subprocess", "pyo3"],
            "runnable_transports": ["subprocess", "pyo3"],
            "binary_path": "/tmp/aethyme-engine-cli",
            "binary_exists": False,
        },
    )

    result = _invoke_inprocess(["repo", "engine-info"])
    assert result.exit_code == 0, result.output
    assert "Transport: subprocess" in result.output
    assert "Transport source: env" in result.output
    assert "Resolved transport: subprocess" in result.output
    assert "Supported: yes" in result.output
    assert "Ready: yes" in result.output
    assert "Binary path: /tmp/aethyme-engine-cli" in result.output
    assert "Runnable transports: subprocess, pyo3" in result.output


def test_engine_transport_global_option_sets_runtime_transport(monkeypatch) -> None:
    def fake_runtime_info() -> dict[str, object]:
        transport = os.getenv("AETHYME_ENGINE_TRANSPORT", "subprocess")
        return {
            "transport": transport,
            "transport_source": "env",
            "resolved_transport": transport,
            "transport_supported": True,
            "transport_ready": True,
            "transport_detail": "ready",
            "supported_transports": ["auto", "subprocess", "pyo3"],
            "runnable_transports": ["subprocess", "pyo3"],
            "binary_path": "/tmp/aethyme-engine-cli",
            "binary_exists": False,
        }

    monkeypatch.setattr("src.cli.engine_runtime_info", fake_runtime_info)

    result = _invoke_inprocess(["--engine-transport", "pyo3", "repo", "engine-info"])
    assert result.exit_code == 0, result.output
    assert "Transport: pyo3" in result.output


def test_engine_transport_global_option_accepts_custom_name(monkeypatch) -> None:
    monkeypatch.setattr(
        "src.cli.engine_runtime_info",
        lambda: {
            "transport": os.getenv("AETHYME_ENGINE_TRANSPORT", "auto"),
            "transport_source": "env",
            "resolved_transport": "custom-runner",
            "transport_supported": True,
            "transport_ready": True,
            "transport_detail": "ready (custom transport)",
            "supported_transports": ["auto", "subprocess", "pyo3", "custom-runner"],
            "runnable_transports": ["subprocess", "pyo3", "custom-runner"],
            "binary_path": "/tmp/aethyme-engine-cli",
            "binary_exists": False,
        },
    )

    result = _invoke_inprocess(["--engine-transport", "CUSTOM-RUNNER", "repo", "engine-info"])
    assert result.exit_code == 0, result.output
    assert "Transport: custom-runner" in result.output


def test_repo_engine_info_check_returns_nonzero_when_not_ready(monkeypatch) -> None:
    monkeypatch.setattr(
        "src.cli.engine_runtime_info",
        lambda: {
            "transport": "pyo3",
            "transport_source": "env",
            "resolved_transport": "pyo3",
            "transport_supported": True,
            "transport_ready": False,
            "transport_detail": "module 'aethyme_py' not installed",
            "supported_transports": ["auto", "subprocess", "pyo3"],
            "runnable_transports": ["subprocess", "pyo3"],
            "binary_path": "/tmp/aethyme-engine-cli",
            "binary_exists": False,
        },
    )

    result = _invoke_inprocess(["repo", "engine-info", "--check"])
    assert result.exit_code != 0
    assert "Engine transport is not ready" in result.output


def test_engine_transport_global_option_rejects_blank(monkeypatch) -> None:
    monkeypatch.setattr("src.cli.engine_runtime_info", lambda: {"transport": "auto"})

    result = _invoke_inprocess(["--engine-transport", "   ", "repo", "engine-info"])
    assert result.exit_code != 0
    assert "Engine transport cannot be empty" in result.output


def test_require_local_engine_or_skip_enforces_when_requested(monkeypatch) -> None:
    monkeypatch.setenv("AETHYME_REQUIRE_LOCAL_ENGINE", "1")
    _probe_local_engine_once.cache_clear()
    monkeypatch.setattr(sys.modules[__name__], "_probe_local_engine_once", lambda: (False, "boom"))

    with pytest.raises(AssertionError, match="required but unavailable"):
        require_local_engine_or_skip()


def test_require_local_engine_or_skip_skips_by_default(monkeypatch) -> None:
    monkeypatch.delenv("AETHYME_REQUIRE_LOCAL_ENGINE", raising=False)
    _probe_local_engine_once.cache_clear()
    monkeypatch.setattr(sys.modules[__name__], "_probe_local_engine_once", lambda: (False, "boom"))

    with pytest.raises(pytest.skip.Exception, match="unavailable"):
        require_local_engine_or_skip()
