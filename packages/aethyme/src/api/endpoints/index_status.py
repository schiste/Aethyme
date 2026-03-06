import asyncio
from datetime import datetime
from pathlib import Path
from typing import Protocol

from fastapi import APIRouter, HTTPException, Query
from pydantic import BaseModel, Field

from ...auth import RepoReadUser, RepoWriteUser
from ...graph.connection_pool import db_pool
from ...indexing.freshness import FreshnessMonitor, FreshnessStatus, format_staleness
from ...indexing.service import (
    IndexingResult,
    RepositoryIndexRequest,
)
from ...indexing.service import (
    run_indexing as run_repository_indexing,
)

router = APIRouter(prefix="/api/v1/index", tags=["indexing"])


class IndexingUserContext(Protocol):
    """Minimal authenticated context needed to create a service request."""

    tenant_id: str
    org_id: str | None


class IndexStatusResponse(BaseModel):
    """Response model for index status."""
    repo_id: str = Field(..., description="Repository ID")
    repo_name: str = Field(..., description="Repository name")
    last_indexed_at: datetime | None = Field(None, description="Last successful index timestamp")
    is_stale: bool = Field(..., description="Whether index is stale")
    staleness_status: str = Field(..., description="Freshness status: fresh, stale, critical, never_indexed")
    staleness_human: str = Field(..., description="Human-readable staleness")
    symbol_count: int = Field(..., description="Total symbol count")
    language_breakdown: dict[str, int] = Field(..., description="Symbol counts by language")
    errors: list[str] = Field(default_factory=list, description="Recent indexing errors")
    duration_seconds: float | None = Field(None, description="Last index duration when tracked")
    index_status: str = Field(..., description="Current status: pending, indexing, completed, failed")


class RepositoryListItem(BaseModel):
    """List item for repositories."""
    repo_id: str
    repo_name: str
    last_indexed_at: datetime | None
    staleness_status: str
    symbol_count: int


class FreshnessSummaryResponse(BaseModel):
    """Summary of freshness across all repositories."""
    tenant_id: str
    total_repositories: int
    fresh_count: int
    stale_count: int
    critical_count: int
    never_indexed_count: int
    stale_repositories: list[RepositoryListItem]


class IndexRepositoryRequest(BaseModel):
    """Request to register and index a local repository."""

    repository_path: str = Field(..., description="Absolute or relative path to the repository")
    repository_name: str | None = Field(default=None, description="Optional repository display name")
    languages: list[str] = Field(default_factory=lambda: ["python"], description="Languages to index")
    use_fallback: bool = Field(default=False, description="Skip SCIP and use the regex fallback indexer")
    clear_existing: bool = Field(default=True, description="Clear existing graph data before re-indexing")

    def to_service_request(self, user: IndexingUserContext) -> RepositoryIndexRequest:
        """Convert API payload into the canonical indexing service request."""
        return RepositoryIndexRequest(
            repo_path=Path(self.repository_path),
            repo_name=self.repository_name,
            languages=self.languages,
            tenant_id=user.tenant_id,
            org_id=user.org_id,
            use_fallback=self.use_fallback,
            clear_existing=self.clear_existing,
        )


class IndexRepositoryResponse(BaseModel):
    """Response after a repository indexing run."""

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
    language_results: list[dict[str, str | int]]
    errors: list[str]


@router.get("/status/{repo_id}", response_model=IndexStatusResponse)
async def get_index_status(
    repo_id: str,
    user: RepoReadUser,
):
    """
    Get indexing status for a specific repository.

    Returns current status, freshness metrics, symbol counts, and recent errors.
    """
    tenant_id = user.tenant_id
    if not tenant_id:
        raise HTTPException(status_code=400, detail="Tenant ID required")

    # Get repository info
    repo_result = db_pool.execute(
        """
        SELECT id, name, last_indexed_at, index_status
        FROM aethyme.repositories
        WHERE id = %s AND tenant_id = %s
        """,
        (repo_id, tenant_id),
    )

    if not repo_result:
        raise HTTPException(status_code=404, detail="Repository not found")

    repo = repo_result[0]

    # Get freshness metrics
    monitor = FreshnessMonitor(db_pool)
    freshness = monitor.get_repository_freshness(repo_id, tenant_id)

    # Get symbol counts
    symbol_result = db_pool.execute(
        """
        SELECT
            COUNT(*) as total_count,
            language
        FROM aethyme.nodes
        WHERE repository_id = %s
            AND tenant_id = %s
            AND kind IN ('function', 'class', 'method', 'variable', 'constant')
        GROUP BY language
        """,
        (repo_id, tenant_id),
    )

    language_breakdown: dict[str, int] = {}
    total_symbols = 0
    for row in symbol_result or []:
        lang = row["language"]
        count = int(row["total_count"])
        language_breakdown[str(lang)] = count
        total_symbols += count

    return IndexStatusResponse(
        repo_id=repo_id,
        repo_name=repo["name"],
        last_indexed_at=repo["last_indexed_at"],
        is_stale=freshness.status in [FreshnessStatus.STALE, FreshnessStatus.CRITICAL],
        staleness_status=freshness.status.value,
        staleness_human=format_staleness(freshness.staleness_hours),
        symbol_count=total_symbols,
        language_breakdown=language_breakdown,
        errors=[],
        duration_seconds=None,
        index_status=repo["index_status"] or "unknown",
    )


@router.post("/repositories", response_model=IndexRepositoryResponse)
async def run_indexing(
    request: IndexRepositoryRequest,
    user: RepoWriteUser,
):
    """Register and index a repository inside the authenticated tenant."""
    service_request = request.to_service_request(user)
    repo_path = service_request.resolved_repo_path()
    if not repo_path.exists():
        raise HTTPException(status_code=404, detail=f"Repository path not found: {repo_path}")
    if not repo_path.is_dir():
        raise HTTPException(status_code=400, detail="Repository path must be a directory")

    result = await asyncio.to_thread(run_repository_indexing, service_request)
    return _to_index_response(result)


@router.get("/freshness", response_model=FreshnessSummaryResponse)
async def get_freshness_summary(
    user: RepoReadUser,
    include_stale_only: bool = Query(False, description="Only include stale repositories in list"),
):
    """
    Get freshness summary for all repositories in tenant.

    Returns counts by freshness status and list of stale repositories.
    """
    tenant_id = user.tenant_id
    if not tenant_id:
        raise HTTPException(status_code=400, detail="Tenant ID required")

    monitor = FreshnessMonitor(db_pool)

    # Get summary
    summary = monitor.get_freshness_summary(tenant_id)

    # Get repository list
    if include_stale_only:
        stale_repos = monitor.get_stale_repositories(tenant_id)
    else:
        # Get all repositories
        all_repos = db_pool.execute(
            """
            SELECT id
            FROM aethyme.repositories
            WHERE tenant_id = %s
            """,
            (tenant_id,),
        )
        stale_repos = [
            monitor.get_repository_freshness(str(repo["id"]), tenant_id)
            for repo in (all_repos or [])
        ]

    # Get symbol counts for each repo
    repo_list: list[RepositoryListItem] = []
    for freshness in stale_repos:
        # Get symbol count
        symbol_result = db_pool.execute(
            """
            SELECT COUNT(*) as count
            FROM aethyme.nodes
            WHERE repository_id = %s
                AND tenant_id = %s
                AND kind IN ('function', 'class', 'method', 'variable', 'constant')
            """,
            (freshness.repository_id, tenant_id),
        )

        symbol_count = int(symbol_result[0]["count"]) if symbol_result else 0

        repo_list.append(
            RepositoryListItem(
                repo_id=freshness.repository_id,
                repo_name=freshness.repository_name,
                last_indexed_at=freshness.last_indexed_at,
                staleness_status=freshness.status.value,
                symbol_count=symbol_count,
            )
        )

    return FreshnessSummaryResponse(
        tenant_id=tenant_id,
        total_repositories=len(stale_repos),
        fresh_count=summary["fresh"],
        stale_count=summary["stale"],
        critical_count=summary["critical"],
        never_indexed_count=summary["never_indexed"],
        stale_repositories=repo_list,
    )


def _to_index_response(result: IndexingResult) -> IndexRepositoryResponse:
    return IndexRepositoryResponse(**result.to_dict())
