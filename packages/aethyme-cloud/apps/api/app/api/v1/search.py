"""Search API endpoints backed by Elasticsearch indexes."""

from __future__ import annotations

import time
from collections import Counter
from typing import Any, Optional

from fastapi import APIRouter, Depends, HTTPException, Query, status
from sqlalchemy import select
from sqlalchemy.ext.asyncio import AsyncSession

from app.core.deps import get_current_user, get_db
from app.core.elasticsearch import CodeIndexer
from app.models.repository import Repository
from app.models.user import User

router = APIRouter()


def _paginate(results: list[dict[str, Any]], page: int, per_page: int) -> dict[str, Any]:
    total = len(results)
    start = (page - 1) * per_page
    end = start + per_page
    total_pages = (total + per_page - 1) // per_page if total else 0
    return {
        "results": results[start:end],
        "total": total,
        "page": page,
        "per_page": per_page,
        "total_pages": total_pages,
    }


def _build_facets(results: list[dict[str, Any]]) -> dict[str, dict[str, int]]:
    language_counts = Counter()
    repository_counts = Counter()

    for result in results:
        language = result.get("language")
        repository = result.get("repository_id")
        if language:
            language_counts[str(language)] += 1
        if repository:
            repository_counts[str(repository)] += 1

    return {
        "languages": dict(language_counts),
        "repositories": dict(repository_counts),
    }


@router.get("/")
async def search_code(
    q: str = Query(..., min_length=1, description="Search query"),
    languages: Optional[str] = Query(None, description="Language filter"),
    file_path: Optional[str] = Query(None, description="File path filter"),
    page: int = Query(1, ge=1),
    per_page: int = Query(10, ge=1, le=100),
    current_user: User = Depends(get_current_user),
    db: AsyncSession = Depends(get_db),
):
    """Search across all repositories in the user's organization."""
    started_at = time.perf_counter()

    repo_result = await db.execute(
        select(Repository).where(Repository.organization_id == current_user.organization_id)
    )
    repositories = list(repo_result.scalars().all())

    indexer = CodeIndexer()
    aggregated_results: list[dict[str, Any]] = []

    for repository in repositories:
        try:
            search_result = indexer.search_code(
                repository_id=repository.id,
                query=q,
                language=languages,
                file_path=file_path,
                limit=100,
            )
        except Exception:
            continue

        for item in search_result.get("results", []):
            normalized = dict(item)
            normalized.setdefault("repository_id", repository.id)
            aggregated_results.append(normalized)

    aggregated_results.sort(key=lambda item: float(item.get("score", 0)), reverse=True)
    pagination = _paginate(aggregated_results, page=page, per_page=per_page)

    return {
        "query": q,
        "results": pagination["results"],
        "total": pagination["total"],
        "page": pagination["page"],
        "per_page": pagination["per_page"],
        "total_pages": pagination["total_pages"],
        "facets": _build_facets(aggregated_results),
        "took_ms": round((time.perf_counter() - started_at) * 1000, 2),
    }


@router.get("/repositories/{repository_id}")
async def search_repository(
    repository_id: str,
    q: str = Query(..., min_length=1, description="Search query"),
    language: Optional[str] = Query(None, description="Language filter"),
    file_path: Optional[str] = Query(None, description="File path filter"),
    current_user: User = Depends(get_current_user),
    db: AsyncSession = Depends(get_db),
):
    """Search within a single repository owned by the current organization."""
    repo_result = await db.execute(
        select(Repository).where(
            Repository.id == repository_id,
            Repository.organization_id == current_user.organization_id,
        )
    )
    repository = repo_result.scalar_one_or_none()

    if repository is None:
        raise HTTPException(
            status_code=status.HTTP_404_NOT_FOUND,
            detail="Repository not found",
        )

    started_at = time.perf_counter()
    indexer = CodeIndexer()

    try:
        search_result = indexer.search_code(
            repository_id=repository.id,
            query=q,
            language=language,
            file_path=file_path,
            limit=100,
        )
        results = list(search_result.get("results", []))
    except Exception:
        results = []

    return {
        "query": q,
        "repository_id": repository.id,
        "results": results,
        "total": len(results),
        "language": language,
        "file_path": file_path,
        "took_ms": round((time.perf_counter() - started_at) * 1000, 2),
    }
