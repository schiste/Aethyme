"""Focused tests for engine caching and evaluation runners."""

from __future__ import annotations

import json
import sys
import types
from pathlib import Path

from src.indexing import engine as engine_module
from src.indexing.repository_snapshot import capture_snapshot


def build_demo_repo(root: Path) -> None:
    root.mkdir(parents=True, exist_ok=True)
    (root / "src").mkdir(exist_ok=True)
    (root / "README.md").write_text("# Demo Repo\n", encoding="utf-8")
    (root / "src" / "main.py").write_text("def main():\n    return 1\n", encoding="utf-8")


def test_engine_inspect_uses_snapshot_cache(monkeypatch, tmp_path: Path) -> None:
    repo_path = tmp_path / "repo"
    build_demo_repo(repo_path)
    cache_root = tmp_path / "cache"
    cache_root.mkdir()
    fake_binary = tmp_path / "aethyme-engine-cli"
    fake_binary.write_text("", encoding="utf-8")

    calls: list[tuple[str, ...]] = []

    monkeypatch.setattr(engine_module, "CACHE_ROOT", cache_root)
    monkeypatch.setattr(engine_module, "ensure_engine_binary", lambda: fake_binary)

    def fake_run(*args: str) -> str:
        calls.append(args)
        return json.dumps(
            {
                "snapshot": {
                    "root": str(repo_path),
                    "languages": ["python"],
                    "top_level_dirs": ["src"],
                    "readme_path": "README.md",
                    "files": [{"path": "README.md", "language": None, "line_count": 1, "size_bytes": 12}],
                },
                "symbols": [],
                "edges": [],
                "risk_flags": [],
            }
        )

    monkeypatch.setattr(engine_module, "_run_binary_command", fake_run)

    first = engine_module.inspect_repository(repo_path)
    second = engine_module.inspect_repository(repo_path)

    assert first == second
    assert len(calls) == 1


def test_engine_command_rejects_unknown_transport(monkeypatch) -> None:
    monkeypatch.setenv("AETHYME_ENGINE_TRANSPORT", "unknown")

    try:
        engine_module._run_binary_command("inspect", "--repo", "/tmp/repo")
    except engine_module.EngineError as exc:
        assert "Unsupported engine transport" in str(exc)
    else:
        raise AssertionError("Expected EngineError for unsupported engine transport")


def test_engine_runtime_info_reflects_configured_transport(monkeypatch, tmp_path: Path) -> None:
    fake_binary = tmp_path / "aethyme-engine-cli"
    monkeypatch.setenv("AETHYME_ENGINE_TRANSPORT", "subprocess")
    monkeypatch.setattr(engine_module, "ENGINE_BINARY_PATH", fake_binary)

    info = engine_module.engine_runtime_info()

    assert info["transport"] == "subprocess"
    assert info["transport_source"] == "env"
    assert info["transport_supported"] is True
    assert info["binary_exists"] is False
    assert info["transport_ready"] is False
    assert info["transport_detail"] == "binary not built"
    assert info["resolved_transport"] == "subprocess"
    assert "auto" in info["supported_transports"]
    assert "subprocess" in info["supported_transports"]
    assert "pyo3" in info["supported_transports"]


def test_register_engine_transport_runner(monkeypatch) -> None:
    monkeypatch.setenv("AETHYME_ENGINE_TRANSPORT", "fake")

    engine_module.register_engine_transport("fake", lambda args: json.dumps({"args": list(args)}))
    try:
        payload = json.loads(engine_module._run_binary_command("inspect", "--repo", "/tmp/repo"))
    finally:
        engine_module.ENGINE_TRANSPORT_RUNNERS.pop("fake", None)

    assert payload["args"] == ["inspect", "--repo", "/tmp/repo"]


def test_register_engine_transport_normalizes_name(monkeypatch) -> None:
    monkeypatch.setenv("AETHYME_ENGINE_TRANSPORT", "custom")
    engine_module.register_engine_transport("  CUSTOM  ", lambda _args: "{}")
    try:
        assert "custom" in engine_module.ENGINE_TRANSPORT_RUNNERS
        output = engine_module._run_binary_command("inspect")
    finally:
        engine_module.ENGINE_TRANSPORT_RUNNERS.pop("custom", None)
    assert output == "{}"


def test_register_engine_transport_prevents_accidental_override() -> None:
    try:
        engine_module.register_engine_transport("subprocess", lambda _args: "{}")
    except ValueError as exc:
        assert "already registered" in str(exc)
    else:
        raise AssertionError("Expected ValueError for duplicate transport registration")


def test_engine_command_dispatches_to_pyo3_transport(monkeypatch) -> None:
    monkeypatch.setenv("AETHYME_ENGINE_TRANSPORT", "pyo3")
    fake_module = types.SimpleNamespace(
        run_engine_command=lambda args: json.dumps({"transport": "pyo3", "args": args})
    )
    monkeypatch.setitem(sys.modules, "aethyme_py", fake_module)

    payload = json.loads(engine_module._run_binary_command("inspect", "--repo", "/tmp/repo"))
    assert payload["transport"] == "pyo3"
    assert payload["args"] == ["inspect", "--repo", "/tmp/repo"]


def test_pyo3_cache_identity_does_not_require_engine_binary(monkeypatch, tmp_path: Path) -> None:
    repo_path = tmp_path / "repo"
    build_demo_repo(repo_path)
    cache_root = tmp_path / "cache"
    cache_root.mkdir()
    fake_module = types.SimpleNamespace(
        __file__=str(tmp_path / "aethyme_py.so"),
        __version__="test",
        run_engine_command=lambda _args: "{}",
    )

    monkeypatch.setenv("AETHYME_ENGINE_TRANSPORT", "pyo3")
    monkeypatch.setitem(sys.modules, "aethyme_py", fake_module)
    monkeypatch.setattr(engine_module, "CACHE_ROOT", cache_root)

    def fail_engine_binary() -> Path:
        raise AssertionError("subprocess binary should not be required")

    monkeypatch.setattr(engine_module, "ensure_engine_binary", fail_engine_binary)

    cache_dir = engine_module._cache_directory(capture_snapshot(repo_path))

    assert cache_dir.parent == cache_root
    assert cache_dir.exists()


def test_engine_runtime_info_reports_pyo3_readiness(monkeypatch) -> None:
    monkeypatch.setenv("AETHYME_ENGINE_TRANSPORT", "pyo3")
    fake_module = types.SimpleNamespace(run_engine_command=lambda _args: "{}")
    monkeypatch.setitem(sys.modules, "aethyme_py", fake_module)

    info = engine_module.engine_runtime_info()

    assert info["transport"] == "pyo3"
    assert info["resolved_transport"] == "pyo3"
    assert info["transport_supported"] is True
    assert info["transport_ready"] is True
    assert info["transport_detail"] == "ready"


def test_engine_runtime_info_auto_prefers_pyo3_when_available(monkeypatch) -> None:
    monkeypatch.setenv("AETHYME_ENGINE_TRANSPORT", "auto")
    fake_module = types.SimpleNamespace(run_engine_command=lambda _args: "{}")
    monkeypatch.setitem(sys.modules, "aethyme_py", fake_module)

    info = engine_module.engine_runtime_info()

    assert info["transport"] == "auto"
    assert info["resolved_transport"] == "pyo3"
    assert info["transport_ready"] is True
    assert "auto->pyo3" in info["transport_detail"]


def test_engine_runtime_info_auto_falls_back_to_subprocess(monkeypatch, tmp_path: Path) -> None:
    monkeypatch.setenv("AETHYME_ENGINE_TRANSPORT", "auto")
    monkeypatch.delitem(sys.modules, "aethyme_py", raising=False)
    monkeypatch.setattr(engine_module, "ENGINE_BINARY_PATH", tmp_path / "missing-engine")

    info = engine_module.engine_runtime_info()

    assert info["transport"] == "auto"
    assert info["resolved_transport"] == "subprocess"
    assert info["transport_ready"] is False
    assert "auto->subprocess" in info["transport_detail"]


def test_engine_runtime_info_defaults_to_auto(monkeypatch, tmp_path: Path) -> None:
    monkeypatch.delenv("AETHYME_ENGINE_TRANSPORT", raising=False)
    monkeypatch.delitem(sys.modules, "aethyme_py", raising=False)
    monkeypatch.setattr(engine_module, "ENGINE_BINARY_PATH", tmp_path / "missing-engine")

    info = engine_module.engine_runtime_info()

    assert info["transport"] == "auto"
    assert info["transport_source"] == "default"
    assert info["resolved_transport"] == "subprocess"
    assert info["transport_ready"] is False
    assert info["transport_supported"] is True


def test_engine_runtime_info_blank_env_defaults_to_auto(monkeypatch, tmp_path: Path) -> None:
    monkeypatch.setenv("AETHYME_ENGINE_TRANSPORT", "   ")
    monkeypatch.delitem(sys.modules, "aethyme_py", raising=False)
    monkeypatch.setattr(engine_module, "ENGINE_BINARY_PATH", tmp_path / "missing-engine")

    info = engine_module.engine_runtime_info()

    assert info["transport"] == "auto"
    assert info["transport_source"] == "default"
    assert info["resolved_transport"] == "subprocess"
    assert info["transport_supported"] is True


def test_engine_command_pyo3_transport_requires_callable(monkeypatch) -> None:
    monkeypatch.setenv("AETHYME_ENGINE_TRANSPORT", "pyo3")
    monkeypatch.setitem(sys.modules, "aethyme_py", types.SimpleNamespace())

    try:
        engine_module._run_binary_command("inspect")
    except engine_module.EngineError as exc:
        assert "run_engine_command" in str(exc)
    else:
        raise AssertionError("Expected EngineError when PyO3 callable is missing")
