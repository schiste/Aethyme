"""Main FastAPI application for Aethyme."""

from collections.abc import AsyncIterator, Awaitable, Callable
from contextlib import asynccontextmanager
from typing import Any, TypeAlias, cast

import structlog
from fastapi import FastAPI, Request, status
from fastapi.middleware.cors import CORSMiddleware
from fastapi.middleware.trustedhost import TrustedHostMiddleware
from fastapi.responses import JSONResponse, Response
from slowapi import Limiter, _rate_limit_exceeded_handler
from slowapi.errors import RateLimitExceeded
from slowapi.util import get_remote_address

from ..config import settings
from ..graph.connection_pool import db_pool
from .endpoints import index_status
from .routes import (
    auth as auth_routes,
)
from .routes import (
    ego,
    health,
    impact,
    scorecard,
    search,
)

logger = structlog.get_logger(__name__)
ASGIApp: TypeAlias = Callable[[Any, Any, Any], Awaitable[None]]


# Rate limiting
limiter = Limiter(key_func=get_remote_address)


def create_redis_client(url: str) -> Any:
    """Create a Redis client without leaking third-party partial types."""
    from redis.utils import from_url as redis_from_url

    typed_from_url = cast(Callable[..., Any], redis_from_url)
    return typed_from_url(url, decode_responses=True)


def create_metrics_app() -> ASGIApp:
    """Create the Prometheus ASGI application."""
    from prometheus_client import CONTENT_TYPE_LATEST, generate_latest

    async def metrics_app(scope: Any, receive: Any, send: Any) -> None:
        del receive
        if scope.get("type") != "http":
            await send(
                {
                    "type": "http.response.start",
                    "status": 404,
                    "headers": [(b"content-length", b"0")],
                }
            )
            await send({"type": "http.response.body", "body": b""})
            return

        payload = generate_latest()
        headers = [
            (b"content-type", CONTENT_TYPE_LATEST.encode("ascii")),
            (b"content-length", str(len(payload)).encode("ascii")),
        ]
        await send(
            {
                "type": "http.response.start",
                "status": 200,
                "headers": headers,
            }
        )
        await send({"type": "http.response.body", "body": payload})

    return metrics_app


@asynccontextmanager
async def lifespan(app: FastAPI) -> AsyncIterator[None]:
    """Manage application lifecycle with proper cleanup."""
    # Startup
    logger.info("Starting Aethyme API")

    # Initialize Redis if configured
    if settings.redis_url_str:
        try:
            redis_client = create_redis_client(settings.redis_url_str)
            redis_client.ping()
            app.state.redis = redis_client
            logger.info("Redis connected", url=settings.redis_url_str)
        except Exception as e:
            logger.warning("Redis not available, caching disabled", error=str(e))
            app.state.redis = None
    else:
        app.state.redis = None

    # Check database connection
    health = db_pool.health_check()
    if health["status"] != "healthy":
        logger.error("Database not healthy", health=health)
        raise RuntimeError("Database connection failed")

    logger.info("Aethyme API started successfully")

    yield

    # Shutdown
    logger.info("Shutting down Aethyme API")

    # Close Redis connection
    if hasattr(app.state, "redis") and app.state.redis:
        app.state.redis.close()

    # Close database connections
    db_pool.close_all()

    logger.info("Aethyme API shutdown complete")


# Create FastAPI app
app = FastAPI(
    title="Aethyme API",
    description="Graph-based code intelligence system",
    version="2.0.0",
    lifespan=lifespan,
    docs_url="/docs",
    redoc_url="/redoc",
    openapi_url="/openapi.json",
)

# Security middleware
app.add_middleware(
    TrustedHostMiddleware,
    allowed_hosts=settings.allowed_hosts,
)

# CORS middleware
app.add_middleware(
    CORSMiddleware,
    allow_origins=settings.cors_origins,
    allow_credentials=True,
    allow_methods=["GET", "POST"],
    allow_headers=["Authorization", "Content-Type"],
)

# Rate limiting
app.state.limiter = limiter


def rate_limit_exception_handler(request: Request, exc: Exception) -> Response:
    """Bridge slowapi's specific handler signature to FastAPI's generic contract."""
    rate_limit_exc = cast(RateLimitExceeded, exc)
    return _rate_limit_exceeded_handler(request, rate_limit_exc)


app.add_exception_handler(RateLimitExceeded, rate_limit_exception_handler)


# Exception handlers
@app.exception_handler(Exception)
async def global_exception_handler(request: Request, exc: Exception):
    """Global exception handler for unhandled errors."""
    logger.error(
        "Unhandled exception",
        error=str(exc),
        path=request.url.path,
        method=request.method,
    )
    return JSONResponse(
        status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
        content={"detail": "Internal server error"},
    )


# Include routers
app.include_router(
    health.router,
    prefix="/health",
    tags=["health"],
)

app.include_router(
    auth_routes.router,
    prefix="/api/v1/auth",
    tags=["authentication"],
)

app.include_router(
    ego.router,
    prefix="/api/v1/ego",
    tags=["ego-graph"],
)

app.include_router(
    impact.router,
    prefix="/api/v1/impact",
    tags=["impact-analysis"],
)

app.include_router(
    search.router,
    prefix="/api/v1/search",
    tags=["search"],
)

app.include_router(
    index_status.router,
    tags=["indexing"],
)

app.include_router(
    scorecard.router,
    prefix="/api/v1/scorecard",
    tags=["scorecard"],
)


# Mount Prometheus metrics endpoint
if settings.metrics_enabled:
    app.mount("/metrics", create_metrics_app())


@app.get("/")
async def root():
    """Root endpoint."""
    return {
        "name": "Aethyme API",
        "version": "2.0.0",
        "status": "running",
        "docs": "/docs",
    }


@app.get("/api/v1/info")
async def info():
    """Get API information."""
    return {
        "name": "Aethyme",
        "version": "2.0.0",
        "features": [
            "ego-graphs",
            "impact-analysis",
            "hybrid-search",
            "multi-tenant",
            "jwt-authentication",
        ],
        "languages": ["python", "typescript", "javascript"],
    }
