"""Ego graph API routes."""

from typing import Dict, Any, Optional
from fastapi import APIRouter, Depends, HTTPException, Query
from pydantic import BaseModel, Field
from slowapi import Limiter
from slowapi.util import get_remote_address
import json

from ...graph.store import GraphStore
from ..auth import jwt_or_api_key, User

router = APIRouter()
limiter = Limiter(key_func=get_remote_address)


class EgoGraphRequest(BaseModel):
    """Request model for ego graph queries."""

    symbol: str = Field(
        ...,
        min_length=1,
        max_length=512,
        description="Symbol to get ego graph for",
    )
    depth: int = Field(
        default=1,
        ge=1,
        le=3,
        description="Maximum depth to traverse (1-3)",
    )
    limit: int = Field(
        default=100,
        ge=1,
        le=1000,
        description="Maximum number of nodes to return",
    )


class EgoGraphResponse(BaseModel):
    """Response model for ego graph queries."""

    symbol: str
    definition: Optional[Dict[str, Any]]
    nodes_by_depth: Dict[int, list]
    total_nodes: int
    cached: bool = False


@router.post("/", response_model=EgoGraphResponse)
async def get_ego_graph(
    request: EgoGraphRequest,
    user: User = Depends(jwt_or_api_key),
) -> EgoGraphResponse:
    """
    Get ego graph for a symbol.

    Returns the definition and all connected nodes up to the specified depth.
    """
    # Check cache if available
    from ..main import app

    cache_key = f"ego:{user.tenant_id}:{request.symbol}:{request.depth}:{request.limit}"
    cached_result = None

    if hasattr(app.state, "redis") and app.state.redis:
        try:
            cached = app.state.redis.get(cache_key)
            if cached:
                cached_result = json.loads(cached)
        except Exception:
            pass  # Cache errors are non-fatal

    if cached_result:
        return EgoGraphResponse(
            symbol=request.symbol,
            definition=cached_result.get("definition"),
            nodes_by_depth=cached_result.get("nodes_by_depth", {}),
            total_nodes=cached_result.get("total_nodes", 0),
            cached=True,
        )

    # Query database
    store = GraphStore(tenant_id=user.tenant_id)
    result = store.ego_graph(
        symbol=request.symbol,
        depth=request.depth,
        limit=request.limit,
    )

    if "error" in result:
        raise HTTPException(status_code=404, detail=result["error"])

    # Cache result
    if hasattr(app.state, "redis") and app.state.redis:
        try:
            app.state.redis.setex(
                cache_key,
                300,  # 5 minutes
                json.dumps(result, default=str),
            )
        except Exception:
            pass  # Cache errors are non-fatal

    return EgoGraphResponse(
        symbol=request.symbol,
        definition=result.get("definition"),
        nodes_by_depth=result.get("nodes_by_depth", {}),
        total_nodes=result.get("total_nodes", 0),
        cached=False,
    )


@router.get("/definition/{symbol}")
async def get_definition(
    symbol: str,
    user: User = Depends(jwt_or_api_key),
) -> Dict[str, Any]:
    """
    Get definition node for a symbol.

    This is a simpler endpoint that just returns the definition without the full ego graph.
    """
    store = GraphStore(tenant_id=user.tenant_id)
    definition = store.find_definition(symbol)

    if not definition:
        raise HTTPException(
            status_code=404,
            detail=f"Definition not found for symbol: {symbol}",
        )

    return definition