"""Code search API endpoints."""

from typing import Optional
from fastapi import APIRouter, Depends, Query
from sqlalchemy.orm import Session

from app.core.database import get_db
from app.core.auth import get_current_user
from app.models.user import User
from app.services.elasticsearch import ElasticsearchService

router = APIRouter()


@router.get("/search")
async def search_code(
    q: str = Query(..., description="Search query"),
    repository_id: Optional[str] = Query(None, description="Filter by repository ID"),
    kind: Optional[str] = Query(None, description="Filter by symbol kind (function, class, method, etc.)"),
    language: Optional[str] = Query(None, description="Filter by programming language"),
    file_path: Optional[str] = Query(None, description="Filter by file path"),
    limit: int = Query(100, ge=1, le=1000, description="Number of results to return"),
    offset: int = Query(0, ge=0, description="Pagination offset"),
    current_user: User = Depends(get_current_user),
    db: Session = Depends(get_db),
):
    """
    Search for code symbols across repositories.

    Supports:
    - Full-text search on symbol names, signatures, and documentation
    - Fuzzy matching for typos
    - Filtering by repository, kind, language, and file path
    - Pagination with limit and offset

    Returns a list of matching symbols with their metadata and relevance scores.
    """
    es_service = ElasticsearchService()

    try:
        result = await es_service.search_symbols(
            query=q,
            repository_id=repository_id,
            kind=kind,
            language=language,
            file_path=file_path,
            limit=limit,
            offset=offset,
        )

        return {
            "query": q,
            "results": result["symbols"],
            "total": result["total"],
            "limit": result["limit"],
            "offset": result["offset"],
            "filters": {
                "repository_id": repository_id,
                "kind": kind,
                "language": language,
                "file_path": file_path,
            },
        }
    finally:
        await es_service.close()


@router.get("/symbols/{symbol_id}")
async def get_symbol(
    symbol_id: str,
    current_user: User = Depends(get_current_user),
    db: Session = Depends(get_db),
):
    """
    Get detailed information about a specific symbol by its ID.

    The symbol ID format is: `repo:path:kind:name[:parent]`

    Returns the symbol's metadata, location, documentation, and relationships.
    """
    es_service = ElasticsearchService()

    try:
        symbol = await es_service.get_symbol_by_id(symbol_id)

        if not symbol:
            return {"error": "Symbol not found"}, 404

        return symbol
    finally:
        await es_service.close()


@router.get("/repositories/{repository_id}/statistics")
async def get_repository_statistics(
    repository_id: str,
    current_user: User = Depends(get_current_user),
    db: Session = Depends(get_db),
):
    """
    Get code statistics for a repository.

    Returns:
    - Total symbol count
    - Breakdown by symbol kind (function, class, method, etc.)
    - Breakdown by programming language
    - File count
    - Symbol counts per file
    """
    es_service = ElasticsearchService()

    try:
        stats = await es_service.get_repository_statistics(repository_id)

        return {
            "repository_id": repository_id,
            "statistics": stats,
        }
    finally:
        await es_service.close()


@router.get("/health")
async def elasticsearch_health():
    """Check if Elasticsearch is healthy."""
    es_service = ElasticsearchService()

    try:
        healthy = await es_service.health_check()

        return {
            "elasticsearch": "healthy" if healthy else "unhealthy",
            "status": "ok" if healthy else "error",
        }
    finally:
        await es_service.close()
