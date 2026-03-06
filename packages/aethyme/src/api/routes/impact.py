"""Impact analysis API routes."""

import json
from typing import TypeAlias, cast

import structlog
from fastapi import APIRouter, HTTPException
from pydantic import BaseModel, Field

from ...auth import RepoReadUser
from ...graph.store import GraphStore

logger = structlog.get_logger(__name__)
router = APIRouter()

ImpactDepthMap: TypeAlias = dict[int, list[dict[str, str]]]
BulkImpactEntry: TypeAlias = dict[str, int | str]


class ImpactAnalysisRequest(BaseModel):
    """Request model for impact analysis."""

    symbol: str = Field(
        ...,
        min_length=1,
        max_length=512,
        description="Symbol to analyze impact for",
    )
    max_depth: int = Field(
        default=10,
        ge=1,
        le=20,
        description="Maximum depth to traverse",
    )
    limit: int = Field(
        default=1000,
        ge=1,
        le=5000,
        description="Maximum number of results",
    )


class ImpactAnalysisResponse(BaseModel):
    """Response model for impact analysis."""

    symbol: str
    total_impacted: int
    max_depth_reached: int
    by_depth: ImpactDepthMap
    cached: bool = False


@router.post("/", response_model=ImpactAnalysisResponse)
async def analyze_impact(
    request: ImpactAnalysisRequest,
    user: RepoReadUser,
) -> ImpactAnalysisResponse:
    """
    Analyze the impact of changes to a symbol.

    Finds all symbols that would be affected by changes to the specified symbol,
    traversing the dependency graph up to the specified depth.
    """
    # Check cache
    from ..main import app

    cache_key = f"impact:{user.tenant_id}:{request.symbol}:{request.max_depth}:{request.limit}"
    cached_result = None

    if hasattr(app.state, "redis") and app.state.redis:
        try:
            cached = app.state.redis.get(cache_key)
            if cached:
                cached_result = json.loads(cached)
        except Exception as exc:
            logger.warning("Impact cache read failed", cache_key=cache_key, error=str(exc))

    if cached_result:
        return ImpactAnalysisResponse(
            symbol=request.symbol,
            total_impacted=cached_result.get("total_impacted", 0),
            max_depth_reached=cached_result.get("max_depth_reached", 0),
            by_depth=cached_result.get("by_depth", {}),
            cached=True,
        )

    # Query database
    store = GraphStore(tenant_id=user.tenant_id)
    result = store.impact_analysis(
        symbol=request.symbol,
        max_depth=request.max_depth,
        limit=request.limit,
    )

    if "error" in result:
        raise HTTPException(status_code=404, detail=result["error"])

    # Cache result
    if hasattr(app.state, "redis") and app.state.redis:
        try:
            app.state.redis.setex(
                cache_key,
                600,  # 10 minutes (longer cache for expensive query)
                json.dumps(result, default=str),
            )
        except Exception as exc:
            logger.warning("Impact cache write failed", cache_key=cache_key, error=str(exc))

    return ImpactAnalysisResponse(
        symbol=request.symbol,
        total_impacted=result.get("total_impacted", 0),
        max_depth_reached=result.get("max_depth_reached", 0),
        by_depth=result.get("by_depth", {}),
        cached=False,
    )


class BulkImpactRequest(BaseModel):
    """Request model for bulk impact analysis."""

    symbols: list[str] = Field(
        ...,
        min_length=1,
        max_length=50,
        description="List of symbols to analyze",
    )
    max_depth: int = Field(
        default=5,
        ge=1,
        le=10,
        description="Maximum depth to traverse",
    )


class BulkImpactResponse(BaseModel):
    """Response model for bulk impact analysis."""

    results: dict[str, BulkImpactEntry]
    total_unique_impacted: int
    overlapping_impacts: list[str]


@router.post("/bulk", response_model=BulkImpactResponse)
async def bulk_impact_analysis(
    request: BulkImpactRequest,
    user: RepoReadUser,
) -> BulkImpactResponse:
    """
    Analyze impact for multiple symbols.

    Useful for understanding the combined impact of multiple changes.
    """
    store = GraphStore(tenant_id=user.tenant_id)
    results: dict[str, BulkImpactEntry] = {}
    all_impacted: set[str] = set()
    symbol_impacts: dict[str, set[str]] = {}

    for symbol in request.symbols:
        result = store.impact_analysis(
            symbol=symbol,
            max_depth=request.max_depth,
            limit=1000,
        )

        if "error" not in result:
            results[symbol] = {
                "total_impacted": result.get("total_impacted", 0),
                "max_depth_reached": result.get("max_depth_reached", 0),
            }

            # Track impacted symbols
            impacted: set[str] = set()
            raw_by_depth = result.get("by_depth", {})
            by_depth: dict[object, object]
            if isinstance(raw_by_depth, dict):
                by_depth = cast(dict[object, object], raw_by_depth)
            else:
                by_depth = {}
            for raw_depth_nodes in by_depth.values():
                if not isinstance(raw_depth_nodes, list):
                    continue
                depth_nodes = cast(list[object], raw_depth_nodes)
                for raw_node in depth_nodes:
                    if not isinstance(raw_node, dict):
                        continue
                    typed_node = cast(dict[str, object], raw_node)
                    symbol_value = typed_node.get("symbol")
                    if isinstance(symbol_value, str):
                        impacted.add(symbol_value)

            symbol_impacts[symbol] = impacted
            all_impacted.update(impacted)
        else:
            results[symbol] = {"error": result["error"]}

    # Find overlapping impacts (symbols impacted by multiple changes)
    overlapping: list[str] = []
    for symbol in all_impacted:
        affecting_symbols = [
            s for s, impacts in symbol_impacts.items() if symbol in impacts
        ]
        if len(affecting_symbols) > 1:
            overlapping.append(symbol)

    return BulkImpactResponse(
        results=results,
        total_unique_impacted=len(all_impacted),
        overlapping_impacts=overlapping[:20],  # Limit to top 20
    )
