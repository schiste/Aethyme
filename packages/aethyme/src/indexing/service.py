"""Shared indexing service used by the CLI and API."""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, TypeAlias

import structlog

from ..graph.connection_pool import db_pool
from ..graph.store import GraphStore
from ..indexer.fallback_indexer import FallbackIndexer
from ..indexer.graph_builder import GraphBuilder
from ..indexer.scip_wrapper import SCIPIndexer

logger = structlog.get_logger(__name__)

DEFAULT_ORG_ID = "00000000-0000-0000-0000-000000000001"
DEFAULT_TENANT_ID = "00000000-0000-0000-0000-000000000101"
ProgressCallback: TypeAlias = Callable[[int, int, str], None]
DEFAULT_EXCLUDE_DIRS = [
    "node_modules",
    "__pycache__",
    ".git",
    "venv",
    ".venv",
    "dist",
    "build",
    ".pytest_cache",
    ".mypy_cache",
    "site-packages",
    "proc",
    "sys",
    "dev",
    "run",
    "tmp",
]


@dataclass
class IndexingLanguageResult:
    language: str
    engine: str
    nodes: int
    edges: int
    files: int

    def to_dict(self) -> dict[str, str | int]:
        """Return a serializable representation of the language result."""
        return {
            "language": self.language,
            "engine": self.engine,
            "nodes": self.nodes,
            "edges": self.edges,
            "files": self.files,
        }


def _new_language_results() -> list[IndexingLanguageResult]:
    """Return a fresh list for language results."""
    return []


def _new_errors() -> list[str]:
    """Return a fresh list for indexing errors."""
    return []


@dataclass
class RepositoryIndexRequest:
    """Canonical repository indexing request shared by API and CLI."""

    repo_path: Path
    repo_name: str | None = None
    languages: list[str] | None = None
    tenant_id: str | None = None
    org_id: str | None = None
    use_fallback: bool = False
    clear_existing: bool = True

    def resolved_repo_path(self) -> Path:
        """Return the normalized repository path."""
        return Path(self.repo_path).expanduser().resolve()

    def resolved_languages(self) -> list[str]:
        """Return normalized language identifiers."""
        resolved = [language.strip() for language in (self.languages or ["python", "typescript"])]
        return [language for language in resolved if language]


@dataclass
class IndexingResult:
    repository_id: str
    repository_name: str
    repository_path: str
    org_id: str | None
    tenant_id: str
    languages: list[str]
    total_nodes: int
    total_edges: int
    total_files: int
    graph_statistics: dict[str, int]
    language_results: list[IndexingLanguageResult] = field(default_factory=_new_language_results)
    errors: list[str] = field(default_factory=_new_errors)

    def to_dict(self) -> dict[str, Any]:
        """Return a serializable representation of the indexing result."""
        return {
            "repository_id": self.repository_id,
            "repository_name": self.repository_name,
            "repository_path": self.repository_path,
            "org_id": self.org_id,
            "tenant_id": self.tenant_id,
            "languages": list(self.languages),
            "total_nodes": self.total_nodes,
            "total_edges": self.total_edges,
            "total_files": self.total_files,
            "graph_statistics": dict(self.graph_statistics),
            "language_results": [item.to_dict() for item in self.language_results],
            "errors": list(self.errors),
        }


def ensure_default_scope() -> tuple[str, str]:
    """Return a stable default org/tenant pair for local workflows."""
    org_result = db_pool.execute(
        "SELECT id FROM aethyme.orgs WHERE slug = %s LIMIT 1",
        ("default",),
    )
    if org_result:
        org_id = str(org_result[0]["id"])
    else:
        db_pool.execute(
            """
            INSERT INTO aethyme.orgs (id, name, slug, description)
            VALUES (%s, %s, %s, %s)
            """,
            (DEFAULT_ORG_ID, "Default Org", "default", "Default local organization"),
            fetch=False,
        )
        org_id = DEFAULT_ORG_ID

    tenant_result = db_pool.execute(
        "SELECT id FROM aethyme.tenants WHERE org_id = %s AND slug = %s LIMIT 1",
        (org_id, "default"),
    )
    if tenant_result:
        return org_id, str(tenant_result[0]["id"])

    db_pool.execute(
        """
        INSERT INTO aethyme.tenants (id, org_id, name, slug, description)
        VALUES (%s, %s, %s, %s, %s)
        """,
        (DEFAULT_TENANT_ID, org_id, "Default Tenant", "default", "Default local tenant"),
        fetch=False,
    )
    return org_id, DEFAULT_TENANT_ID


def resolve_scope(tenant_id: str | None, org_id: str | None = None) -> tuple[str | None, str]:
    """Resolve the effective org and tenant for indexing."""
    if tenant_id:
        if org_id:
            return org_id, tenant_id
        result = db_pool.execute(
            "SELECT org_id FROM aethyme.tenants WHERE id = %s",
            (tenant_id,),
        )
        if result and result[0].get("org_id"):
            return str(result[0]["org_id"]), tenant_id
        return None, tenant_id
    return ensure_default_scope()


def run_indexing(
    request: RepositoryIndexRequest,
    progress_callback: ProgressCallback | None = None,
) -> IndexingResult:
    """Index a repository from the canonical request contract."""
    repo_path = request.resolved_repo_path()
    if not repo_path.exists():
        raise FileNotFoundError(f"Repository path does not exist: {repo_path}")

    languages = request.resolved_languages()
    resolved_org_id, resolved_tenant_id = resolve_scope(request.tenant_id, request.org_id)
    repository_name = request.repo_name or repo_path.name

    store = GraphStore(tenant_id=resolved_tenant_id, org_id=resolved_org_id)
    repository = store.create_repository(
        name=repository_name,
        path=str(repo_path),
        languages=languages,
    )
    repository_id = str(repository["id"])

    logger.info(
        "Starting repository indexing",
        repository_id=repository_id,
        repository_name=repository_name,
        repository_path=str(repo_path),
        tenant_id=resolved_tenant_id,
        languages=languages,
        use_fallback=request.use_fallback,
    )

    db_pool.execute(
        """
        UPDATE aethyme.repositories
        SET index_status = 'indexing'
        WHERE id = %s AND tenant_id = %s
        """,
        (repository_id, resolved_tenant_id),
        fetch=False,
    )

    if request.clear_existing:
        store.clear_repository(repository_id)

    language_results: list[IndexingLanguageResult] = []
    errors: list[str] = []
    total_nodes = 0
    total_edges = 0
    total_files = 0

    for language in languages:
        builder = GraphBuilder(
            store=store,
            batch_size=1000,
            progress_callback=progress_callback,
        )

        try:
            if not request.use_fallback:
                scip_indexer = SCIPIndexer(language)
                if scip_indexer.is_available():
                    scip_data = scip_indexer.index(repo_path, repository_name)
                    stats = builder.build_from_scip(scip_data, repository_id, language)
                    language_results.append(
                        IndexingLanguageResult(
                            language=language,
                            engine="scip",
                            nodes=stats.nodes_processed,
                            edges=stats.edges_created,
                            files=stats.files_processed,
                        )
                    )
                    total_nodes += stats.nodes_processed
                    total_edges += stats.edges_created
                    total_files += stats.files_processed
                    continue

            fallback_indexer = FallbackIndexer(language)
            nodes, edges = fallback_indexer.index(repo_path, exclude_dirs=DEFAULT_EXCLUDE_DIRS)
            stats = builder.build_from_fallback(nodes, edges, repository_id)
            language_results.append(
                IndexingLanguageResult(
                    language=language,
                    engine="fallback",
                    nodes=stats.nodes_processed,
                    edges=stats.edges_created,
                    files=stats.files_processed,
                )
            )
            total_nodes += stats.nodes_processed
            total_edges += stats.edges_created
            total_files += stats.files_processed
        except Exception as exc:
            logger.error("Language indexing failed", language=language, error=str(exc))
            errors.append(f"{language}: {exc}")
            db_pool.execute(
                """
                UPDATE aethyme.repositories
                SET index_status = 'failed'
                WHERE id = %s AND tenant_id = %s
                """,
                (repository_id, resolved_tenant_id),
                fetch=False,
            )
            raise RuntimeError(f"Indexing failed for {language}: {exc}") from exc

    db_pool.execute(
        """
        UPDATE aethyme.repositories
        SET index_status = 'completed',
            last_indexed_at = CURRENT_TIMESTAMP
        WHERE id = %s AND tenant_id = %s
        """,
        (repository_id, resolved_tenant_id),
        fetch=False,
    )

    graph_statistics = store.get_statistics()
    return IndexingResult(
        repository_id=repository_id,
        repository_name=repository_name,
        repository_path=str(repo_path),
        org_id=resolved_org_id,
        tenant_id=resolved_tenant_id,
        languages=languages,
        total_nodes=total_nodes,
        total_edges=total_edges,
        total_files=total_files,
        graph_statistics=graph_statistics,
        language_results=language_results,
        errors=errors,
    )


def index_repository(
    repo_path: Path,
    repo_name: str | None = None,
    languages: list[str] | None = None,
    tenant_id: str | None = None,
    org_id: str | None = None,
    use_fallback: bool = False,
    clear_existing: bool = True,
    progress_callback: ProgressCallback | None = None,
) -> IndexingResult:
    """Backward-compatible convenience wrapper over the canonical request contract."""
    request = RepositoryIndexRequest(
        repo_path=repo_path,
        repo_name=repo_name,
        languages=languages,
        tenant_id=tenant_id,
        org_id=org_id,
        use_fallback=use_fallback,
        clear_existing=clear_existing,
    )
    return run_indexing(request, progress_callback=progress_callback)
