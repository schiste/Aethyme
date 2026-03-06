"""Unit tests for fallback graph construction and import edge semantics."""

from __future__ import annotations

from pathlib import Path
from typing import Any, cast

from src.indexer.fallback_indexer import FallbackIndexer
from src.indexer.graph_builder import GraphBuilder
from src.models.graph import Edge, EdgeType, Node, NodeKind


class RecordingStore:
    """Minimal store that captures persisted fallback graphs."""

    def __init__(self) -> None:
        self.nodes: list[Node] = []
        self.edges: list[Edge] = []

    def insert_nodes(self, nodes: list[Node], repository_id: str) -> int:
        del repository_id
        self.nodes.extend(nodes)
        return len(nodes)

    def insert_edges(self, edges: list[Edge], repository_id: str) -> int:
        del repository_id
        self.edges.extend(edges)
        return len(edges)


def _write(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content)


def _build_fallback_graph(repo_path: Path, language: str) -> RecordingStore:
    indexer = FallbackIndexer(language)
    nodes, edges = indexer.index(repo_path)
    store = RecordingStore()
    builder = GraphBuilder(store=cast(Any, store))
    builder.build_from_fallback(nodes, edges, repository_id="repo-test")
    return store


def test_python_from_import_targets_only_requested_symbol(tmp_path: Path) -> None:
    repo_path = tmp_path / "python-repo"
    _write(
        repo_path / "helper.py",
        "def helper() -> str:\n    return 'ok'\n\n\ndef unused_helper() -> str:\n    return 'skip'\n",
    )
    _write(
        repo_path / "service.py",
        "from helper import helper\n\n\ndef run_service() -> str:\n    return helper()\n",
    )

    store = _build_fallback_graph(repo_path, "python")

    helper_def = next(node for node in store.nodes if node.symbol == "helper.py:helper")
    unused_def = next(node for node in store.nodes if node.symbol == "helper.py:unused_helper")
    service_file = next(
        node for node in store.nodes if node.kind == NodeKind.FILE and node.file_path == "service.py"
    )
    import_targets = {
        edge.to_node_id
        for edge in store.edges
        if edge.edge_type == EdgeType.IMPORT and edge.from_node_id == service_file.id
    }

    assert helper_def.id in import_targets
    assert unused_def.id not in import_targets


def test_python_relative_import_resolves_cross_file_edge(tmp_path: Path) -> None:
    repo_path = tmp_path / "package-repo"
    _write(repo_path / "pkg" / "__init__.py", "")
    _write(repo_path / "pkg" / "helper.py", "def helper() -> str:\n    return 'ok'\n")
    _write(
        repo_path / "pkg" / "service.py",
        "from .helper import helper\n\n\ndef run_service() -> str:\n    return helper()\n",
    )

    store = _build_fallback_graph(repo_path, "python")

    helper_def = next(node for node in store.nodes if node.symbol == "pkg/helper.py:helper")
    service_file = next(
        node for node in store.nodes if node.kind == NodeKind.FILE and node.file_path == "pkg/service.py"
    )
    import_targets = {
        edge.to_node_id
        for edge in store.edges
        if edge.edge_type == EdgeType.IMPORT and edge.from_node_id == service_file.id
    }

    assert helper_def.id in import_targets


def test_typescript_named_import_targets_only_requested_symbol(tmp_path: Path) -> None:
    repo_path = tmp_path / "ts-repo"
    _write(
        repo_path / "helper.ts",
        "export function helper(): string { return 'ok'; }\n"
        "export function unusedHelper(): string { return 'skip'; }\n",
    )
    _write(
        repo_path / "service.ts",
        "import { helper } from './helper';\n\nexport function runService(): string { return helper(); }\n",
    )

    store = _build_fallback_graph(repo_path, "typescript")

    helper_def = next(node for node in store.nodes if node.symbol == "helper.ts:helper")
    unused_def = next(node for node in store.nodes if node.symbol == "helper.ts:unusedHelper")
    service_file = next(
        node for node in store.nodes if node.kind == NodeKind.FILE and node.file_path == "service.ts"
    )
    import_targets = {
        edge.to_node_id
        for edge in store.edges
        if edge.edge_type == EdgeType.IMPORT and edge.from_node_id == service_file.id
    }

    assert helper_def.id in import_targets
    assert unused_def.id not in import_targets


def test_python_module_import_keeps_cross_file_edges(tmp_path: Path) -> None:
    repo_path = tmp_path / "python-module-repo"
    _write(
        repo_path / "helper.py",
        "def helper() -> str:\n    return 'ok'\n\n\ndef helper_extra() -> str:\n    return 'extra'\n",
    )
    _write(
        repo_path / "service.py",
        "import helper\n\n\ndef run_service() -> str:\n    return helper.helper()\n",
    )

    store = _build_fallback_graph(repo_path, "python")

    service_file = next(
        node for node in store.nodes if node.kind == NodeKind.FILE and node.file_path == "service.py"
    )
    import_targets = {
        edge.to_node_id
        for edge in store.edges
        if edge.edge_type == EdgeType.IMPORT and edge.from_node_id == service_file.id
    }

    assert import_targets


def test_typescript_namespace_import_keeps_cross_file_edges(tmp_path: Path) -> None:
    repo_path = tmp_path / "ts-namespace-repo"
    _write(
        repo_path / "helper.ts",
        "export function helper(): string { return 'ok'; }\n"
        "export function helperExtra(): string { return 'extra'; }\n",
    )
    _write(
        repo_path / "service.ts",
        "import * as helperApi from './helper';\n\n"
        "export function runService(): string { return helperApi.helper(); }\n",
    )

    store = _build_fallback_graph(repo_path, "typescript")

    service_file = next(
        node for node in store.nodes if node.kind == NodeKind.FILE and node.file_path == "service.ts"
    )
    import_targets = {
        edge.to_node_id
        for edge in store.edges
        if edge.edge_type == EdgeType.IMPORT and edge.from_node_id == service_file.id
    }

    assert import_targets


def test_python_named_import_creates_invoke_edge(tmp_path: Path) -> None:
    repo_path = tmp_path / "python-invoke-repo"
    _write(repo_path / "helper.py", "def helper() -> str:\n    return 'ok'\n")
    _write(
        repo_path / "service.py",
        "from helper import helper\n\n\ndef run_service() -> str:\n    return helper()\n",
    )

    store = _build_fallback_graph(repo_path, "python")

    helper_def = next(node for node in store.nodes if node.symbol == "helper.py:helper")
    invoke_edges = [edge for edge in store.edges if edge.edge_type == EdgeType.INVOKE]

    assert any(edge.to_node_id == helper_def.id for edge in invoke_edges)


def test_typescript_namespace_import_creates_invoke_edge(tmp_path: Path) -> None:
    repo_path = tmp_path / "ts-invoke-repo"
    _write(repo_path / "helper.ts", "export function helper(): string { return 'ok'; }\n")
    _write(
        repo_path / "service.ts",
        "import * as helperApi from './helper';\n\n"
        "export function runService(): string { return helperApi.helper(); }\n",
    )

    store = _build_fallback_graph(repo_path, "typescript")

    helper_def = next(node for node in store.nodes if node.symbol == "helper.ts:helper")
    invoke_edges = [edge for edge in store.edges if edge.edge_type == EdgeType.INVOKE]

    assert any(edge.to_node_id == helper_def.id for edge in invoke_edges)


def test_same_file_reference_creates_local_invoke_edge(tmp_path: Path) -> None:
    repo_path = tmp_path / "local-invoke-repo"
    _write(
        repo_path / "service.py",
        "def helper() -> str:\n    return 'ok'\n\n\ndef run_service() -> str:\n    return helper()\n",
    )

    store = _build_fallback_graph(repo_path, "python")

    helper_def = next(node for node in store.nodes if node.symbol == "service.py:helper")
    invoke_edges = [edge for edge in store.edges if edge.edge_type == EdgeType.INVOKE]

    assert any(edge.to_node_id == helper_def.id for edge in invoke_edges)
