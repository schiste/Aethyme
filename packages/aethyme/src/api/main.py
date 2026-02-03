"""Main FastAPI application for Aethyme."""

from contextlib import asynccontextmanager
from typing import Dict, Any
from fastapi import FastAPI, Request, status
from fastapi.middleware.cors import CORSMiddleware
from fastapi.middleware.trustedhost import TrustedHostMiddleware
from fastapi.responses import JSONResponse
from slowapi import Limiter, _rate_limit_exceeded_handler
from slowapi.util import get_remote_address
from slowapi.errors import RateLimitExceeded
from prometheus_client import make_asgi_app
import structlog
import redis

from ..config import settings
from ..graph.connection_pool import db_pool
from .routes import (
    ego,
    impact,
    search,
    health,
    auth as auth_routes,
    scorecard,
    autofix,
    telemetry,
    guardrails,
    unified,
)
from .endpoints import index_status

logger = structlog.get_logger(__name__)


# Rate limiting
limiter = Limiter(key_func=get_remote_address)


@asynccontextmanager
async def lifespan(app: FastAPI):
    """Manage application lifecycle with proper cleanup."""
    # Startup
    logger.info("Starting Aethyme API")

    # Initialize Redis if configured
    if settings.redis_url_str:
        try:
            app.state.redis = redis.Redis.from_url(
                settings.redis_url_str,
                decode_responses=True,
            )
            app.state.redis.ping()
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
app.add_exception_handler(RateLimitExceeded, _rate_limit_exceeded_handler)


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

app.include_router(
    autofix.router,
    prefix="/api/v1/autofix",
    tags=["autofix"],
)

app.include_router(
    telemetry.router,
    prefix="/api/v1/telemetry",
    tags=["telemetry"],
)

app.include_router(
    guardrails.router,
    prefix="/api/v1/guardrails",
    tags=["guardrails"],
)

app.include_router(
    unified.router,
    prefix="/api/v1",
    tags=["system"],
)


# Mount Prometheus metrics endpoint
if settings.metrics_enabled:
    metrics_app = make_asgi_app()
    app.mount("/metrics", metrics_app)


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