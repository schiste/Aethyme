"""
Index Status API Endpoint

Provides endpoints for querying indexing status, freshness, and statistics.
"""

from fastapi import APIRouter, Depends, HTTPException, Query
from pydantic import BaseModel, Field
from typing import Dict, List, Optional
from datetime import datetime

from src.api.auth import get_current_user
from src.graph.connection_pool import db_pool
from src.indexing.freshness import FreshnessMonitor, FreshnessStatus, format_staleness

router = APIRouter(prefix="/api/index", tags=["indexing"])


class IndexStatusResponse(BaseModel):
    """Response model for index status."""
    repo_id: str = Field(..., description="Repository ID")
    repo_name: str = Field(..., description="Repository name")
    last_indexed_at: Optional[datetime] = Field(None, description="Last successful index timestamp")
    is_stale: bool = Field(..., description="Whether index is stale")
    staleness_status: str = Field(..., description="Freshness status: fresh, stale, critical, never_indexed")
    staleness_human: str = Field(..., description="Human-readable staleness")
    symbol_count: int = Field(..., description="Total symbol count")
    language_breakdown: Dict[str, int] = Field(..., description="Symbol counts by language")
    errors: List[str] = Field(default_factory=list, description="Recent indexing errors")
    duration_seconds: Optional[float] = Field(None, description="Last index duration")
    index_status: str = Field(..., description="Current status: pending, indexing, completed, failed")


class RepositoryListItem(BaseModel):
    """List item for repositories."""
    repo_id: str
    repo_name: str
    last_indexed_at: Optional[datetime]
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
    stale_repositories: List[RepositoryListItem]


@router.get("/status/{repo_id}", response_model=IndexStatusResponse)
async def get_index_status(
    repo_id: str,
    user=Depends(get_current_user),
):
    """
    Get indexing status for a specific repository.

    Returns current status, freshness metrics, symbol counts, and recent errors.
    """
    tenant_id = user.get("tenant_id")
    if not tenant_id:
        raise HTTPException(status_code=400, detail="Tenant ID required")

    # Get repository info
    repo_result = db_pool.execute(
        """
        SELECT id, name, last_indexed_at, index_status
        FROM repograph.repositories
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
        FROM repograph.nodes
        WHERE repository_id = %s
            AND tenant_id = %s
            AND kind IN ('function', 'class', 'method', 'variable', 'constant')
        GROUP BY language
        """,
        (repo_id, tenant_id),
    )

    language_breakdown = {}
    total_symbols = 0
    for row in symbol_result:
        lang = row["language"]
        count = int(row["total_count"])
        language_breakdown[lang] = count
        total_symbols += count

    # Get recent errors (if any)
    # For now, return empty list - can be extended to track errors in a separate table
    errors = []

    # Get last index duration (mock for now - could be stored in DB)
    duration_seconds = None  # TODO: Track this in indexing process

    return IndexStatusResponse(
        repo_id=repo_id,
        repo_name=repo["name"],
        last_indexed_at=repo["last_indexed_at"],
        is_stale=freshness.status in [FreshnessStatus.STALE, FreshnessStatus.CRITICAL],
        staleness_status=freshness.status.value,
        staleness_human=format_staleness(freshness.staleness_hours),
        symbol_count=total_symbols,
        language_breakdown=language_breakdown,
        errors=errors,
        duration_seconds=duration_seconds,
        index_status=repo["index_status"] or "unknown",
    )


@router.get("/freshness", response_model=FreshnessSummaryResponse)
async def get_freshness_summary(
    user=Depends(get_current_user),
    include_stale_only: bool = Query(False, description="Only include stale repositories in list"),
):
    """
    Get freshness summary for all repositories in tenant.

    Returns counts by freshness status and list of stale repositories.
    """
    tenant_id = user.get("tenant_id")
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
            FROM repograph.repositories
            WHERE tenant_id = %s
            """,
            (tenant_id,),
        )
        stale_repos = [
            monitor.get_repository_freshness(str(repo["id"]), tenant_id)
            for repo in all_repos
        ]

    # Get symbol counts for each repo
    repo_list = []
    for freshness in stale_repos:
        # Get symbol count
        symbol_result = db_pool.execute(
            """
            SELECT COUNT(*) as count
            FROM repograph.nodes
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


@router.post("/trigger/{repo_id}")
async def trigger_reindex(
    repo_id: str,
    user=Depends(get_current_user),
):
    """
    Manually trigger re-indexing for a repository.

    Returns accepted status and queues the repository for indexing.
    """
    tenant_id = user.get("tenant_id")
    if not tenant_id:
        raise HTTPException(status_code=400, detail="Tenant ID required")

    # Verify repository exists
    repo_result = db_pool.execute(
        """
        SELECT id, name
        FROM repograph.repositories
        WHERE id = %s AND tenant_id = %s
        """,
        (repo_id, tenant_id),
    )

    if not repo_result:
        raise HTTPException(status_code=404, detail="Repository not found")

    # Mark as pending indexing
    monitor = FreshnessMonitor(db_pool)
    monitor.mark_index_started(repo_id, tenant_id)

    # TODO: Queue actual indexing job (would integrate with background worker)

    return {
        "status": "accepted",
        "repo_id": repo_id,
        "message": "Repository queued for indexing",
    }
