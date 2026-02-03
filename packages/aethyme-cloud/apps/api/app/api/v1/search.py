"""Search API endpoints."""

from typing import Optional
from fastapi import APIRouter, Depends, Query
from sqlalchemy.ext.asyncio import AsyncSession
from sqlalchemy import select, func

from app.core.deps import get_db, get_current_user
from app.models.user import User
from app.models.code_graph import SCIPSymbol
from app.services.search import (
    AdvancedSearchEngine,
    SearchScope,
    SearchMode,
    SearchFilter,
)

router = APIRouter()


@router.get("/symbols")
async def search_symbols(
    q: str = Query(..., description="Search query", min_length=1),
    scope: str = Query("all", description="Search scope"),
    mode: str = Query("contains", description="Search mode"),
    language: Optional[str] = Query(None),
    kind: Optional[str] = Query(None),
    file_path: Optional[str] = Query(None),
    repository_id: Optional[str] = Query(None),
    page: int = Query(1, ge=1),
    page_size: int = Query(20, ge=1, le=100),
    current_user: User = Depends(get_current_user),
    db: AsyncSession = Depends(get_db),
):
    """Search for code symbols."""
    filters = SearchFilter(
        language=language,
        kind=kind,
        file_path=file_path,
        repository_id=repository_id,
    )

    search_engine = AdvancedSearchEngine(db)
    results = await search_engine.search(
        query=q,
        scope=SearchScope(scope),
        mode=SearchMode(mode),
        filters=filters,
        page=page,
        page_size=page_size,
    )

    return results
