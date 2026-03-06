"""Search API routes."""

from fastapi import APIRouter, Query
from pydantic import BaseModel, Field

from ...auth import RepoReadUser
from ...graph.connection_pool import db_pool
from ...graph.store import GraphStore

router = APIRouter()


class SearchRequest(BaseModel):
    """Request model for search queries."""

    query: str = Field(
        ...,
        min_length=1,
        max_length=256,
        description="Search query",
    )
    limit: int = Field(
        default=20,
        ge=1,
        le=100,
        description="Maximum number of results",
    )
    search_type: str = Field(
        default="hybrid",
        description="Search type: 'exact', 'fuzzy', or 'hybrid'",
    )


class SearchResult(BaseModel):
    """Model for a single search result."""

    id: str
    symbol: str
    file_path: str
    line_number: int
    kind: str
    language: str
    score: float
    documentation: str | None = None


class SearchResponse(BaseModel):
    """Response model for search queries."""

    query: str
    results: list[SearchResult]
    total_results: int
    search_type: str


@router.post("/", response_model=SearchResponse)
async def search_symbols(
    request: SearchRequest,
    user: RepoReadUser,
) -> SearchResponse:
    """
    Search for symbols in the code graph.

    Supports three search types:
    - **exact**: Exact symbol matching
    - **fuzzy**: Fuzzy matching using trigram similarity
    - **hybrid**: Combines full-text search with fuzzy matching (default)
    """
    store = GraphStore(tenant_id=user.tenant_id)

    results = store.search(
        query=request.query,
        limit=request.limit,
        search_type=request.search_type,
    )

    # Convert to response model
    search_results: list[SearchResult] = []
    for result in results:
        search_results.append(
            SearchResult(
                id=result["id"],
                symbol=result["symbol"],
                file_path=result["file_path"],
                line_number=result["line_number"],
                kind=result["kind"],
                language=result["language"],
                score=float(result.get("score", 0)),
                documentation=result.get("documentation"),
            )
        )

    return SearchResponse(
        query=request.query,
        results=search_results,
        total_results=len(search_results),
        search_type=request.search_type,
    )


@router.get("/suggest")
async def suggest_symbols(
    user: RepoReadUser,
    prefix: str = Query(..., min_length=2, max_length=100),
    limit: int = Query(default=10, ge=1, le=50),
) -> list[str]:
    """
    Get symbol suggestions based on prefix.

    Useful for autocomplete functionality.
    """
    store = GraphStore(tenant_id=user.tenant_id)

    # Use fuzzy search with the prefix
    results = store.search(
        query=prefix,
        limit=limit,
        search_type="fuzzy",
    )

    # Return just the symbol names for suggestions
    suggestions = [r["symbol"] for r in results]

    return suggestions


class AdvancedSearchRequest(BaseModel):
    """Request model for advanced search with filters."""

    query: str = Field(
        ...,
        min_length=1,
        max_length=256,
        description="Search query",
    )
    file_path_pattern: str | None = Field(
        default=None,
        description="Filter by file path pattern (e.g., '*.py', 'src/*.ts')",
    )
    kinds: list[str] | None = Field(
        default=None,
        description="Filter by node kinds (e.g., ['function', 'class'])",
    )
    languages: list[str] | None = Field(
        default=None,
        description="Filter by languages (e.g., ['python', 'typescript'])",
    )
    limit: int = Field(
        default=20,
        ge=1,
        le=100,
    )


@router.post("/advanced", response_model=SearchResponse)
async def advanced_search(
    request: AdvancedSearchRequest,
    user: RepoReadUser,
) -> SearchResponse:
    """
    Advanced search with filtering options.

    Allows filtering by file path, node kind, and language.
    """
    # Build SQL query with filters
    query_parts = ["SELECT * FROM aethyme.nodes WHERE tenant_id = %s"]
    params: list[str | int] = [user.tenant_id]

    # Add search condition
    query_parts.append("AND (symbol ILIKE %s OR documentation ILIKE %s)")
    search_pattern = f"%{request.query}%"
    params.extend([search_pattern, search_pattern])

    # Add filters
    if request.file_path_pattern:
        query_parts.append("AND file_path LIKE %s")
        params.append(request.file_path_pattern.replace("*", "%"))

    if request.kinds:
        placeholders = ",".join(["%s"] * len(request.kinds))
        query_parts.append(f"AND kind IN ({placeholders})")
        params.extend(request.kinds)

    if request.languages:
        placeholders = ",".join(["%s"] * len(request.languages))
        query_parts.append(f"AND language IN ({placeholders})")
        params.extend(request.languages)

    query_parts.append("LIMIT %s")
    params.append(request.limit)

    results = db_pool.execute(" ".join(query_parts), tuple(params))

    # Convert to response
    search_results: list[SearchResult] = []
    for result in results or []:
        search_results.append(
            SearchResult(
                id=result["id"],
                symbol=result["symbol"],
                file_path=result["file_path"],
                line_number=result["line_number"],
                kind=result["kind"],
                language=result["language"],
                score=1.0,  # No scoring for advanced search
                documentation=result.get("documentation"),
            )
        )

    return SearchResponse(
        query=request.query,
        results=search_results,
        total_results=len(search_results),
        search_type="advanced",
    )
