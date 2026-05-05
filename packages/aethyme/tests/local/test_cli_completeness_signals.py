"""CLI/rendering regression tests for completeness and confidence signal surfacing."""

from __future__ import annotations

import json
from pathlib import Path

from click.testing import CliRunner

from src.cli import cli
from src.indexing.engine import EngineError
from src.rendering.context_pack import render_pack_summary, render_prompt_pack


def test_graph_node_non_json_surfaces_completeness_signals(
    monkeypatch,
    tmp_path: Path,
) -> None:
    repo_path = tmp_path / "demo"
    repo_path.mkdir(parents=True)

    monkeypatch.setattr(
        "src.cli.graph_node",
        lambda _repo, _target: {
            "id": "fn:demo:main",
            "kind": "function",
            "label": "main",
            "confidence": 920,
            "truncated": True,
            "reason": "result cap reached",
            "caps": {"max_items": 50},
        },
    )

    runner = CliRunner()
    result = runner.invoke(cli, ["graph", "node", str(repo_path), "main"])

    assert result.exit_code == 0, result.output
    assert "Confidence: 920" in result.output
    assert "Truncated: yes" in result.output
    assert "Truncation reason: result cap reached" in result.output
    assert 'Caps: {"max_items": 50}' in result.output


def test_task_scope_non_json_renders_reason_fields(
    monkeypatch,
    tmp_path: Path,
) -> None:
    repo_path = tmp_path / "demo"
    repo_path.mkdir(parents=True)

    monkeypatch.setattr(
        "src.cli.task_scope",
        lambda _repo, _task, **_kwargs: {
            "task": "Update auth flow",
            "in_scope_files": [
                {"value": "src/auth.py", "reason": "anchor file"},
            ],
            "in_scope_areas": [
                {"value": "src", "reason": "contains anchor"},
            ],
            "out_of_scope": [
                {"value": "docs", "reason": "non-runtime"},
            ],
            "risks": ["auth regression"],
        },
    )

    runner = CliRunner()
    result = runner.invoke(
        cli,
        ["task", "scope", "--repo", str(repo_path), "--task", "Update auth flow"],
    )

    assert result.exit_code == 0, result.output
    assert "- src/auth.py (anchor file)" in result.output
    assert "- src (contains anchor)" in result.output
    assert "- docs (non-runtime)" in result.output


def test_analyze_dead_code_eval_json_is_task_ready(
    monkeypatch,
    tmp_path: Path,
) -> None:
    repo_path = tmp_path / "demo"
    repo_path.mkdir(parents=True)

    monkeypatch.setattr(
        "src.cli.analyze_dead_code_answer",
        lambda _repo, _scope, roots, include_methods: {
            "analyzer": "dead-code",
            "version": "2",
            "query": {
                "scope": "src/indexing",
                "searched_roots": roots,
                "include_methods": include_methods,
            },
            "candidates": [
                {
                    "function": {
                        "name": "unused_helper",
                        "defined_in": "src/indexing/service.py",
                    },
                    "status": "Unused",
                    "confidence": 0.95,
                    "evidence": {
                        "searched_roots": roots,
                        "external_callers": [],
                        "internal_callers": [],
                        "docs_config_references": ["doc:docs/service.md"],
                    },
                    "ambiguity": [],
                    "rationale": "No external callers found under [src].",
                }
            ],
            "excluded": [],
            "summary": {"total_candidates": 1, "unused": 1, "ambiguous": 0, "used": 0},
            "observability": {
                "graph_counts": {"functions": 1, "docs": 1, "configs": 0, "edges": 2},
                "fact_counts": {
                    "public_functions": 1,
                    "usage_facts": 1,
                    "internal_callers": 0,
                    "external_callers": 0,
                    "docs_config_references": 1,
                },
                "confidence_summary": {
                    "high": 1,
                    "medium": 0,
                    "low": 0,
                    "min": 0.95,
                    "max": 0.95,
                },
                "degraded_reasons": [],
            },
        },
    )

    runner = CliRunner()
    result = runner.invoke(
        cli,
        [
            "analyze",
            "dead-code",
            "--repo",
            str(repo_path),
            "--scope",
            "src/indexing",
            "--roots",
            "src,tests",
            "--format",
            "eval-json",
            "--show-observability",
        ],
    )

    assert result.exit_code == 0, result.output
    payload = json.loads(result.output)
    assert payload["unused_functions"] == [
        {
            "function_name": "unused_helper",
            "defined_in": "src/indexing/service.py",
            "status": "Unused",
            "external_callers": [],
            "internal_callers": [],
            "evidence": {
                "searched_roots": ["src", "tests"],
                "external_callers": [],
                "internal_callers": [],
                "docs_config_references": ["doc:docs/service.md"],
                "ambiguity": [],
            },
            "confidence": 0.95,
            "reason": "No external callers found under [src].",
        }
    ]
    assert payload["excluded_functions"] == []
    assert payload["observability"]["command"] == "analyze dead-code"
    assert (
        payload["observability"]["index_freshness"]["status"]
        == "fresh_for_current_snapshot"
    )
    assert (
        payload["observability"]["graph_fact_count"]["facts"]["public_functions"] == 1
    )


def test_intents_compact_json_lists_default_task_localization_query() -> None:
    runner = CliRunner()
    result = runner.invoke(cli, ["intents", "--format", "compact-json"])

    assert result.exit_code == 0, result.output
    payload = json.loads(result.output)
    explore_mode = next(
        mode for mode in payload["modes"] if mode["mode"] == "explore"
    )
    intent = explore_mode["intents"][0]
    assert intent["intent"] == "task_localization_query"
    assert intent["required_params"] == []
    assert intent["default_for_explore"] is True
    assert "answer_schema" in intent
    assert "observability" in intent
    intent_names = {item["intent"] for item in explore_mode["intents"]}
    assert "behavior_localization_query" in intent_names
    assert "usage_boundary_query" in intent_names


def test_explore_usage_boundary_query_returns_task_ready_answer(
    monkeypatch,
    tmp_path: Path,
) -> None:
    repo_path = tmp_path / "demo"
    repo_path.mkdir(parents=True)

    monkeypatch.setattr(
        "src.cli.usage_boundary_query_answer",
        lambda _repo, _scope, roots, include_methods, budget_ms, max_evidence_per_symbol: {
            "analyzer": "dead-code",
            "version": "2",
            "query": {
                "scope": "src/indexing",
                "searched_roots": roots,
                "include_methods": include_methods,
            },
            "candidates": [
                {
                    "function": {
                        "name": "unused_helper",
                        "defined_in": "src/indexing/service.py",
                    },
                    "status": "Unused",
                    "confidence": 0.95,
                    "evidence": {
                        "searched_roots": roots,
                        "external_callers": [],
                        "internal_callers": [],
                        "docs_config_references": [],
                    },
                    "ambiguity": [],
                    "rationale": "No callers outside src/indexing.",
                }
            ],
            "excluded": [
                {
                    "function": {
                        "name": "used_helper",
                        "defined_in": "src/indexing/service.py",
                    },
                    "status": "Used",
                    "confidence": 0.99,
                    "evidence": {
                        "searched_roots": roots,
                        "external_callers": ["fn:src/api/routes.py:handler"],
                        "internal_callers": [],
                        "docs_config_references": [],
                    },
                    "ambiguity": [],
                    "rationale": "External caller found.",
                }
            ],
            "summary": {"total_candidates": 2, "unused": 1, "ambiguous": 0, "used": 1},
            "observability": {
                "graph_counts": {"functions": 2, "docs": 0, "configs": 0, "edges": 1},
                "fact_counts": {
                    "public_functions": 2,
                    "usage_facts": 2,
                    "internal_callers": 0,
                    "external_callers": 1,
                    "docs_config_references": 0,
                },
                "confidence_summary": {
                    "high": 2,
                    "medium": 0,
                    "low": 0,
                    "min": 0.95,
                    "max": 0.99,
                },
                "degraded_reasons": [],
            },
        },
    )

    runner = CliRunner()
    result = runner.invoke(
        cli,
        [
            "explore",
            "--repo",
            str(repo_path),
            "--intent",
            "usage_boundary_query",
            "--request",
            "Find public top-level functions with no outside callers.",
            "--params",
            json.dumps(
                {
                    "scope": "src/indexing",
                    "symbol_kind": "public_top_level_function",
                    "boundary": {
                        "type": "outside_directory",
                        "path": "src/indexing",
                    },
                    "search_roots": ["src", "tests"],
                }
            ),
            "--format",
            "answer-json",
            "--show-observability",
        ],
    )

    assert result.exit_code == 0, result.output
    payload = json.loads(result.output)
    assert payload["schema_version"] == "aethyme-explore-v1"
    assert payload["mode"] == "explore"
    assert payload["intent"] == "usage_boundary_query"
    assert payload["resolved_parameters"]["scope"] == "src/indexing"
    assert payload["answer"][0]["function_name"] == "unused_helper"
    assert payload["excluded"][0]["function_name"] == "used_helper"
    assert payload["output_adapters"]["dead_code_eval_json"] == {
        "unused_functions": payload["answer"]
    }
    assert payload["confidence"]["overall"] == 0.95
    assert payload["confidence"]["answer_summary"]["high"] == 1
    assert payload["observability"]["command"] == "explore"
    assert payload["observability"]["internal_analyzer"] == "analyze usage-boundary"
    assert payload["observability"]["budget_ms"] == 10000
    assert payload["observability"]["max_evidence_per_symbol"] == 5
    assert payload["observability"]["output_size_bytes"] == len(
        json.dumps(payload, indent=2).encode("utf-8")
    )


def test_explore_without_intent_runs_default_task_localization_query(
    monkeypatch,
    tmp_path: Path,
) -> None:
    repo_path = tmp_path / "demo"
    repo_path.mkdir(parents=True)

    monkeypatch.setattr(
        "src.cli.task_localize",
        lambda _repo, _task, **_kwargs: {
            "task": "Find the files impacted by this change.",
            "anchors": {
                "task": "Find the files impacted by this change.",
                "anchors": [
                    {
                        "kind": "symbol",
                        "id": "fn:src/auth.py:validate",
                        "file": "src/auth.py",
                        "reason": "Matched task terms.",
                    }
                ],
            },
            "scope": {
                "task": "Find the files impacted by this change.",
                "navigation_order": ["src/auth.py", "src/session.py"],
                "in_scope_files": ["src/auth.py"],
                "in_scope_symbols": ["fn:src/auth.py:validate"],
                "in_scope_areas": ["src"],
                "out_of_scope": ["docs"],
                "risks": ["authentication regression"],
            },
            "next": {
                "target": "Find the files impacted by this change.",
                "relation": "next",
                "items": [
                    {
                        "id": "file:src/session.py",
                        "kind": "file",
                        "display": "src/session.py",
                        "relation": "next",
                        "confidence": 880,
                    }
                ],
            },
        },
    )
    monkeypatch.setattr(
        "src.cli.task_expand",
        lambda _repo, _target, **_kwargs: {
            "node": "fn:src/auth.py:validate",
            "dependencies": ["src/config.py"],
            "impact": ["src/session.py"],
            "docs": [],
            "configs": [],
            "risks": [],
        },
    )
    captured_symbol_queries = []

    def fake_search_symbols(_repo, queries, *, limit, timeout_seconds):
        captured_symbol_queries.extend(queries)
        return {
            "impacted": [
                {
                    "id": "fn:src/auth.py:validate",
                    "name": "validate",
                    "kind": "function",
                    "file": "src/auth.py",
                    "line": 10,
                    "score": 200,
                    "reason": "function-name-match",
                }
            ]
        }

    monkeypatch.setattr("src.cli.search_symbols", fake_search_symbols)

    runner = CliRunner()
    result = runner.invoke(
        cli,
        [
            "explore",
            "--repo",
            str(repo_path),
            "--request",
            "Find the files impacted by this change.",
            "--format",
            "answer-json",
            "--show-observability",
        ],
    )

    assert result.exit_code == 0, result.output
    payload = json.loads(result.output)
    assert payload["schema_version"] == "aethyme-explore-v1"
    assert payload["status"] == "complete"
    assert payload["intent"] == "task_localization_query"
    assert payload["intent_source"] == "default"
    assert payload["answer"][0]["kind"] == "symbol_search_file"
    assert payload["answer"][0]["target"] == "src/auth.py"
    assert payload["answer"][1]["target"] == "fn:src/auth.py:validate"
    assert payload["answer"][0]["path"] == "src/auth.py"
    assert payload["answer"][2]["evidence"]["expansion"]["impact"] == ["src/session.py"]
    assert payload["excluded"][0]["target"] == "docs"
    assert payload["output_adapters"]["task_localization_json"]["candidate_files"]
    assert payload["output_adapters"]["task_localization_json"]["candidate_symbols"]
    assert payload["observability"]["internal_analyzers"] == [
        "filesystem-filename",
        "search-symbol",
        "source-text-search",
        "source-callsite",
        "task-localize",
        "task-expand",
    ]
    assert payload["observability"]["command"] == "explore"
    assert payload["observability"]["graph_fact_count"]["graph"]["anchors"] == 1
    assert payload["observability"]["graph_fact_count"]["graph"]["symbol_matches"] == 1
    assert captured_symbol_queries == ["impacted", "change"]
    assert payload["observability"]["output_size_bytes"] == len(
        json.dumps(payload, indent=2).encode("utf-8")
    )


def test_explore_filters_noisy_symbol_queries_and_degrades_without_blocking(
    monkeypatch,
    tmp_path: Path,
) -> None:
    repo_path = tmp_path / "demo"
    repo_path.mkdir(parents=True)

    monkeypatch.setattr(
        "src.cli.task_localize",
        lambda _repo, _task, **_kwargs: {
            "task": "Bug report T419918: fix watchlist diff behavior.",
            "anchors": {
                "task": "Bug report T419918: fix watchlist diff behavior.",
                "anchors": [
                    {
                        "kind": "folder",
                        "id": "includes/Watchlist",
                        "file": None,
                        "reason": "area match",
                    }
                ],
            },
            "scope": {
                "task": "Bug report T419918: fix watchlist diff behavior.",
                "navigation_order": ["includes/Watchlist"],
                "in_scope_files": [],
                "in_scope_symbols": [],
                "in_scope_areas": ["includes/Watchlist"],
                "out_of_scope": [],
                "risks": [],
            },
            "next": {
                "target": "Bug report T419918: fix watchlist diff behavior.",
                "relation": "next",
                "items": [],
            },
        },
    )
    monkeypatch.setattr(
        "src.cli.task_expand",
        lambda _repo, _target, **_kwargs: {
            "node": "includes/Watchlist",
            "dependencies": [],
            "impact": [],
            "docs": [],
            "configs": [],
            "risks": [],
        },
    )

    captured_symbol_queries = []

    def timeout_search_symbols(_repo, queries, *, limit, timeout_seconds):
        captured_symbol_queries.extend(queries)
        raise EngineError("Rust engine timed out after 0.1s: symbol-batch")

    monkeypatch.setattr("src.cli.search_symbols", timeout_search_symbols)

    runner = CliRunner()
    result = runner.invoke(
        cli,
        [
            "explore",
            "--repo",
            str(repo_path),
            "--request",
            "Bug report T419918: fix watchlist diff behavior.",
            "--params",
            '{"symbol_query_timeout_ms":100}',
            "--format",
            "answer-json",
            "--show-observability",
        ],
    )

    assert result.exit_code == 0, result.output
    payload = json.loads(result.output)
    assert payload["status"] == "degraded"
    assert payload["answer"][0]["target"] == "includes/Watchlist"
    assert payload["safe_to_use_as_answer"] is False
    assert payload["trust_policy"]["safe_to_use_as_answer"] is False
    assert payload["trust_policy"]["trust_policy"] == "needs_verification"
    assert payload["trust_policy"]["verification_required"] is True
    assert payload["trust_policy"]["degraded"] is True
    assert "T419918" not in captured_symbol_queries
    assert "report" not in captured_symbol_queries
    assert "symbol batch search skipped" in payload["degraded_reasons"][0]
    assert payload["observability"]["graph_fact_count"]["graph"]["symbol_matches"] == 0


def test_explore_skips_symbol_search_after_graph_timeout(
    monkeypatch,
    tmp_path: Path,
) -> None:
    repo_path = tmp_path / "demo"
    watchlist_dir = repo_path / "includes" / "Watchlist"
    watchlist_dir.mkdir(parents=True)
    (watchlist_dir / "WatchlistManager.php").write_text("<?php\n", encoding="utf-8")

    monkeypatch.setattr(
        "src.cli.task_localize",
        lambda _repo, _task, **_kwargs: (_ for _ in ()).throw(
            EngineError("Rust engine timed out after 0.1s: task-localize")
        ),
    )

    def fail_search_symbols(_repo, queries, *, limit, timeout_seconds):
        raise AssertionError(f"symbol search should have been skipped: {queries}")

    monkeypatch.setattr("src.cli.search_symbols", fail_search_symbols)

    runner = CliRunner()
    result = runner.invoke(
        cli,
        [
            "explore",
            "--repo",
            str(repo_path),
            "--request",
            "Bug report T419918: fix watchlist diff behavior.",
            "--params",
            '{"skip_symbols_after_graph_timeout":true}',
            "--format",
            "answer-json",
            "--show-observability",
        ],
    )

    assert result.exit_code == 0, result.output
    payload = json.loads(result.output)
    assert payload["status"] == "degraded"
    assert payload["answer"] == []
    assert payload["navigation_hints"][0]["target"] == (
        "includes/Watchlist/WatchlistManager.php"
    )
    assert payload["navigation_hints"][0]["confidence"] < 0.5
    assert payload["safe_to_use_as_answer"] is False
    assert payload["trust_policy"]["trust_policy"] == "navigation_only"
    assert payload["output_adapters"]["task_localization_json"]["candidate_files"] == []
    assert any(
        "symbol batch search skipped: task-localize exceeded" in reason
        for reason in payload["degraded_reasons"]
    )
    assert payload["observability"]["graph_fact_count"]["graph"]["symbol_matches"] == 0


def test_explore_source_fallback_bridges_behavior_call_chain_after_graph_timeout(
    monkeypatch,
    tmp_path: Path,
) -> None:
    repo_path = tmp_path / "demo"
    (repo_path / "includes" / "Watchlist").mkdir(parents=True)
    (repo_path / "includes" / "Page").mkdir(parents=True)

    (repo_path / "includes" / "Watchlist" / "WatchlistManager.php").write_text(
        """<?php
class WatchlistManager {
    public function clearTitleUserNotifications( $performer, $title, $oldRev = null ) {
        $oldid = $oldRev ? $oldRev->getId() : 0;
        // Clear the watchlist notification timestamp for only the viewed revision.
        $this->watchedItemStore->resetNotificationTimestamp( $performer, $title, '', $oldid );
    }
}
""",
        encoding="utf-8",
    )
    (repo_path / "includes" / "Page" / "WikiPage.php").write_text(
        """<?php
class WikiPage {
    public function doViewUpdates( $performer, $oldid = 0, $oldRev = null ) {
        // Update newtalk and watchlist notification status after viewing a revision.
        $this->watchlistManager->clearTitleUserNotifications( $performer, $this, $oldRev );
    }
}
""",
        encoding="utf-8",
    )
    (repo_path / "includes" / "Page" / "Article.php").write_text(
        """<?php
class Article {
    public function showDiffPage() {
        $diff = $this->request->getVal( 'diff' );
        $oldid = $this->getOldID();
        // Run view updates for the newer revision being diffed, not all revisions.
        $this->mPage->doViewUpdates( $this->context->getAuthority(), (int)$diff );
    }
}
""",
        encoding="utf-8",
    )

    monkeypatch.setattr(
        "src.cli.task_localize",
        lambda _repo, _task, **_kwargs: (_ for _ in ()).throw(
            EngineError("Rust engine timed out after 0.1s: task-localize")
        ),
    )
    monkeypatch.setattr("src.cli.search_symbols", lambda *_args, **_kwargs: {})

    runner = CliRunner()
    result = runner.invoke(
        cli,
        [
            "explore",
            "--repo",
            str(repo_path),
            "--request",
            (
                "Bug report: viewing a diff revision on a watchlisted page marks "
                "all revisions as seen instead of only the viewed revision."
            ),
            "--format",
            "answer-json",
            "--show-observability",
        ],
    )

    assert result.exit_code == 0, result.output
    payload = json.loads(result.output)
    paths = [item.get("path") for item in payload["answer"]]
    assert payload["status"] == "degraded"
    assert payload["safe_to_use_as_answer"] is False
    assert payload["trust_policy"]["trust_policy"] == "needs_verification"
    assert payload["trust_policy"]["evidence_level"] == "text"
    assert payload["verification_steps"]
    assert "includes/Watchlist/WatchlistManager.php" in paths
    assert "includes/Page/WikiPage.php" in paths
    assert "includes/Page/Article.php" in paths
    article = next(
        item for item in payload["answer"]
        if item.get("path") == "includes/Page/Article.php"
        and item.get("kind") == "call_site_file"
    )
    assert article["kind"] == "call_site_file"
    assert "doViewUpdates" in article["evidence"]["symbols"]
    assert any(
        chain.get("source_symbol") == "doViewUpdates"
        or chain.get("bridge_symbol") == "doViewUpdates"
        for chain in article["evidence"]["chains"]
    )
    graph_counts = payload["observability"]["graph_fact_count"]["graph"]
    assert graph_counts["source_text_candidates"] >= 2
    assert graph_counts["callsite_candidates"] >= 2
    assert (
        payload["observability"]["degradation_guidance"]["status"]
        == "recovered"
    )


def test_render_pack_summary_includes_confidence_caps_and_truncation() -> None:
    summary = render_pack_summary(
        {
            "task": {"raw": "Explain this repo"},
            "confidence": {"anchor_confidence": 0.91, "scope_confidence": 0.73},
            "budget": {
                "max_anchors": 3,
                "max_files": 5,
                "max_snippets": 8,
                "dependency_depth": 2,
                "impact_depth": 3,
                "content_budget": 4096,
                "max_content_files": 2,
                "max_lines_per_file": 120,
            },
            "file_contents": [
                {"path": "src/main.py", "end_line": 120, "total_lines": 280},
                {"path": "src/auth.py", "end_line": 40, "total_lines": 40},
            ],
        }
    )

    assert "Confidence: anchor=0.91, scope=0.73" in summary
    assert "Caps:" in summary
    assert "max_anchors=3" in summary
    assert "Truncated content:" in summary
    assert "src/main.py (120/280 lines)" in summary


def test_render_prompt_pack_handles_scope_items_without_value_key() -> None:
    prompt = render_prompt_pack(
        {
            "task": {"kind": "change_task"},
            "in_scope": {
                "files": [{"value": "src/main.py"}, {"bad": "skip"}],
                "areas": ["src"],
            },
            "out_of_scope": {
                "areas": [{"value": "docs"}, {"bad": "skip"}],
            },
            "navigation_order": ["src/main.py", "src/auth.py"],
        }
    )

    assert "Scope: src/main.py" in prompt
    assert "Avoid: docs" in prompt
