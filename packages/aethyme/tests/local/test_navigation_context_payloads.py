"""Regression tests for navigation context completeness signal propagation."""

from __future__ import annotations

from pathlib import Path
from typing import Any

from src.eval import bug_fix, explain_repo, navigation_ctf
from src.eval._self import self_tool_name
from src.eval.navigation_context import build_scope_view


def test_explain_repo_navigation_context_includes_detailed_scope_signals(tmp_path: Path) -> None:
    pack: dict[str, Any] = {
        "anchors": [{"id": "src/auth.py", "kind": "file", "reason": "entry"}],
        "in_scope": {
            "files": [{"value": "src/auth.py", "reason": "anchor"}],
            "symbols": [{"value": "src/auth.py::validate", "reason": "call path"}],
            "areas": [{"value": "src", "reason": "contains anchor"}],
        },
        "out_of_scope": {
            "areas": [{"value": "docs", "reason": "non-runtime"}],
        },
        "risk_flags": [{"scope": "src/auth.py", "reason": "auth-critical"}],
        "navigation_order": ["src/auth.py"],
        "confidence": {"anchor_confidence": 0.9, "scope_confidence": 0.7},
        "budget": {"max_files": 5},
    }

    context = explain_repo._build_explain_repo_navigation_context(tmp_path, "Explain", pack)
    scope = context["scope"]

    assert scope["in_scope_files"] == ["src/auth.py"]
    assert scope["in_scope_files_detailed"][0]["reason"] == "anchor"
    assert scope["risk_flags"][0]["scope"] == "src/auth.py"
    assert scope["confidence"]["anchor_confidence"] == 0.9
    assert scope["budget"]["max_files"] == 5


def test_bug_fix_navigation_context_uses_file_contents_and_detailed_scope(
    tmp_path: Path,
) -> None:
    """`_build_navigation_context` shapes adapter output into the leverage dict.

    Uses a stub adapter to avoid spawning the real Aethyme CLI — the
    function under test only cares that ``run_condition`` returns
    JSON-parseable ``raw_output`` for ``leverage`` and ``task-conditioned``.
    """
    import json as _json

    from src.eval.tools.base import ConditionResult

    leverage_payload = {
        "anchors": [{"id": "src/auth.py"}],
        "navigation_order": ["src/auth.py"],
        "in_scope": {
            "files": [{"value": "src/auth.py", "reason": "anchor"}],
            "symbols": [],
            "areas": [],
        },
        "out_of_scope": {"areas": []},
        "risk_flags": [{"scope": "src/auth.py", "reason": "critical"}],
        "confidence": {"anchor_confidence": 0.88, "scope_confidence": 0.66},
        "budget": {"max_content_files": 2},
    }
    task_conditioned_payload = {
        "file_contents": [{"path": "src/auth.py", "end_line": 80, "total_lines": 120}],
    }

    class _StubAdapter:
        # Mirror the framework's self-tool name so the adapter trips the
        # self-tool privilege branch (structured task_pack flow). Hard-
        # coding "aethyme" would skip the branch under a fork that runs
        # with AETHYMEBENCH_SELF_TOOL=lapidary.
        name = self_tool_name()
        display_name = "Aethyme (stub)"

        def version(self) -> str:
            return "stub"

        def install(self, target_repo: Path) -> None:
            return None

        def warm(self, target_repo: Path) -> None:
            return None

        def implements(self, condition: str) -> bool:
            return True

        def run_condition(
            self,
            condition: str,
            target_repo: Path,
            task: str,
        ) -> ConditionResult:
            payload = leverage_payload if condition == "leverage" else task_conditioned_payload
            return ConditionResult(prompt_addendum="", raw_output=_json.dumps(payload))

    context = bug_fix._build_navigation_context(tmp_path, "Fix auth bug", tool=_StubAdapter())

    assert context["scope"]["in_scope_files"] == ["src/auth.py"]
    assert context["scope"]["in_scope_files_detailed"][0]["reason"] == "anchor"
    assert context["scope"]["confidence"]["anchor_confidence"] == 0.88
    assert context["scope"]["budget"]["max_content_files"] == 2
    assert context["file_contents"][0]["path"] == "src/auth.py"


def test_navigation_ctf_scope_view_passes_detailed_signals(monkeypatch, tmp_path: Path) -> None:
    captured: dict[str, Any] = {}

    monkeypatch.setattr(navigation_ctf, "inspect_repository", lambda _repo: {"signals": {}})
    monkeypatch.setattr(
        navigation_ctf,
        "_build_navigation_case",
        lambda _inspect: (
            {"task": "Find config", "management_area": "src"},
            {"config_target": {}, "code_target": {}, "management_area": {}, "relationship_chain": []},
        ),
    )
    monkeypatch.setattr(
        navigation_ctf,
        "build_task_pack",
        lambda *_args, **_kwargs: {
            "anchors": [{"id": "src/main.py"}],
            "navigation_order": ["src/main.py"],
            "in_scope": {
                "files": [{"value": "src/main.py", "reason": "entrypoint"}],
                "symbols": [],
                "areas": [{"value": "src", "reason": "owner area"}],
            },
            "out_of_scope": {"areas": [{"value": "docs", "reason": "not runtime"}]},
            "risk_flags": [{"scope": "src/main.py", "reason": "critical"}],
            "confidence": {"anchor_confidence": 0.92, "scope_confidence": 0.78},
            "budget": {"max_files": 4},
        },
    )

    def fake_build_navigation_context(
        _repo_path: Path,
        task_spec: dict[str, Any],
        anchors_view: dict[str, Any],
        scope_view: dict[str, Any],
    ) -> dict[str, Any]:
        captured["task_spec"] = task_spec
        captured["anchors_view"] = anchors_view
        captured["scope_view"] = scope_view
        return {"mode": "iterative_navigation", "scope": scope_view}

    monkeypatch.setattr(navigation_ctf, "_build_navigation_context", fake_build_navigation_context)
    monkeypatch.setattr(
        navigation_ctf,
        "write_navigation_ctf_markdown_report",
        lambda **_kwargs: tmp_path / "navigation-ctf-report.md",
    )

    result = navigation_ctf.run_navigation_ctf_evaluation(tmp_path)

    assert result["navigation_context"]["mode"] == "iterative_navigation"
    scope = captured["scope_view"]
    assert scope["in_scope_files"] == ["src/main.py"]
    assert scope["in_scope_files_detailed"][0]["reason"] == "entrypoint"
    assert scope["out_of_scope_detailed"][0]["value"] == "docs"
    assert scope["confidence"]["anchor_confidence"] == 0.92
    assert scope["budget"]["max_files"] == 4


def test_build_scope_view_preserves_flat_and_detailed_shapes() -> None:
    scope = build_scope_view(
        "Investigate auth",
        {
            "navigation_order": ["src/auth.py"],
            "in_scope": {
                "files": [{"value": "src/auth.py", "reason": "anchor"}],
                "symbols": [{"value": "src/auth.py::validate", "reason": "call graph"}],
                "areas": [{"value": "src", "reason": "top area"}],
            },
            "out_of_scope": {"areas": [{"value": "docs", "reason": "not runtime"}]},
            "risk_flags": [{"scope": "src/auth.py", "reason": "critical"}],
            "confidence": {"anchor_confidence": 0.81, "scope_confidence": 0.64},
            "budget": {"max_files": 4},
        },
    )

    assert scope["task"] == "Investigate auth"
    assert scope["in_scope_files"] == ["src/auth.py"]
    assert scope["in_scope_symbols"] == ["src/auth.py::validate"]
    assert scope["in_scope_areas"] == ["src"]
    assert scope["out_of_scope"] == ["docs"]
    assert scope["in_scope_files_detailed"][0]["reason"] == "anchor"
    assert scope["risk_flags"][0]["scope"] == "src/auth.py"
    assert scope["confidence"]["scope_confidence"] == 0.64
    assert scope["budget"]["max_files"] == 4


def test_build_scope_view_accepts_mixed_item_shapes() -> None:
    scope = build_scope_view(
        "Mixed scope",
        {
            "in_scope": {
                "files": [{"value": "src/a.py"}, "src/b.py", {"bad": "skip"}],
                "symbols": ["src/a.py::fn"],
                "areas": [{"value": "src"}],
            },
            "out_of_scope": {"areas": ["docs", {"value": "generated"}]},
        },
    )

    assert scope["in_scope_files"] == ["src/a.py", "src/b.py"]
    assert scope["in_scope_symbols"] == ["src/a.py::fn"]
    assert scope["in_scope_areas"] == ["src"]
    assert scope["out_of_scope"] == ["docs", "generated"]
