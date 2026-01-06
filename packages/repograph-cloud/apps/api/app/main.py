"""
RepoGraph Cloud API - Main Application Entry Point

Enterprise-grade FastAPI application with:
- GraphQL and REST endpoints
- WebSocket support
- Distributed tracing (OpenTelemetry)
- Rate limiting with Redis
- Security headers
- Input validation
- Request logging
- Error tracking (Sentry)
"""

from contextlib import asynccontextmanager
from fastapi import FastAPI
from fastapi.middleware.gzip import GZipMiddleware
from fastapi.responses import JSONResponse
import sentry_sdk
import logging
import signal
import sys

from app.core.config import settings
from app.core.database import init_db, close_db
from app.core.rate_limit import limiter
from app.core.middleware import (
    RateLimitHeadersMiddleware,
    CorrelationIDMiddleware,
    SecurityHeadersMiddleware,
    RequestLoggingMiddleware,
    get_cors_middleware,
)
from app.core.validation import InputValidationMiddleware
from app.core.tracing import init_tracing, instrument_app
from app.api.v1 import router as api_v1_router
from app.api.graphql import graphql_router
from app.websocket.router import websocket_router

# Import to register presence message handlers
import app.services.presence.message_handlers

# Configure logging
logging.basicConfig(
    level=getattr(logging, settings.LOG_LEVEL.upper()),
    format="%(asctime)s - %(name)s - %(levelname)s - %(message)s"
)
logger = logging.getLogger(__name__)


# Initialize Sentry for error tracking
if settings.SENTRY_DSN:
    try:
        from sentry_sdk.integrations.fastapi import FastAPIIntegration
        from sentry_sdk.integrations.sqlalchemy import SqlalchemyIntegration

        sentry_sdk.init(
            dsn=settings.SENTRY_DSN,
            environment=settings.SENTRY_ENVIRONMENT,
            traces_sample_rate=settings.SENTRY_TRACES_SAMPLE_RATE,
            integrations=[
                FastAPIIntegration(),
                SqlalchemyIntegration(),
            ],
            # Send errors to Sentry
            send_default_pii=False,  # Don't send PII by default
            attach_stacktrace=True,  # Attach stack traces to events
            request_bodies="medium",  # Include request bodies (up to medium size)
        )
        logger.info("Sentry initialized successfully")
    except Exception as e:
        logger.warning(f"Failed to initialize Sentry: {e}")


# Application lifecycle
@asynccontextmanager
async def lifespan(app: FastAPI):
    """
    Application lifespan events.

    Startup:
    - Initialize database connection pool
    - Initialize distributed tracing
    - Warm up connections

    Shutdown:
    - Close database connections gracefully
    - Flush remaining traces
    """
    logger.info("Starting RepoGraph Cloud API...")

    # Startup
    try:
        # Initialize database
        await init_db()
        logger.info("Database initialized")

        # Initialize tracing
        init_tracing()
        logger.info("Tracing initialized")

        logger.info("RepoGraph Cloud API started successfully")

    except Exception as e:
        logger.error(f"Failed to start application: {e}")
        raise

    yield

    # Shutdown
    logger.info("Shutting down RepoGraph Cloud API...")
    try:
        await close_db()
        logger.info("Database connections closed")
    except Exception as e:
        logger.error(f"Error during shutdown: {e}")

    logger.info("RepoGraph Cloud API stopped")


# Create FastAPI application
app = FastAPI(
    title="RepoGraph Cloud API",
    description="Enterprise Code Intelligence Platform",
    version="1.0.0",
    docs_url="/docs" if settings.ENVIRONMENT != "production" else None,
    redoc_url="/redoc" if settings.ENVIRONMENT != "production" else None,
    openapi_url="/openapi.json" if settings.ENVIRONMENT != "production" else None,
    lifespan=lifespan,
)

# Instrument app with OpenTelemetry
try:
    instrument_app(app)
    logger.info("OpenTelemetry instrumentation applied")
except Exception as e:
    logger.warning(f"Failed to instrument app with OpenTelemetry: {e}")

# Apply middleware (order matters - first added is outermost)
# 1. Request logging (outermost - logs everything)
app.add_middleware(RequestLoggingMiddleware)

# 2. Security headers
app.add_middleware(SecurityHeadersMiddleware)

# 3. Correlation ID (for distributed tracing)
app.add_middleware(CorrelationIDMiddleware)

# 4. Input validation
app.add_middleware(InputValidationMiddleware)

# 5. Rate limit headers
app.add_middleware(RateLimitHeadersMiddleware)

# 6. CORS (with proper configuration)
app.add_middleware(get_cors_middleware().__class__, **get_cors_middleware().__dict__)

# 7. GZip compression (innermost - compresses response)
app.add_middleware(GZipMiddleware, minimum_size=1000)

# Rate limiting state
app.state.limiter = limiter


@app.get("/")
async def root():
    """Root endpoint with API information."""
    return {
        "message": "RepoGraph Cloud API",
        "version": "1.0.0",
        "environment": settings.ENVIRONMENT,
        "docs": "/docs" if settings.ENVIRONMENT != "production" else None,
        "graphql": "/graphql",
        "health": {
            "liveness": "/health",
            "readiness": "/health/ready",
            "detailed": "/health/detailed",
        }
    }


# Include API routers
app.include_router(api_v1_router, prefix="/api", tags=["api"])
app.include_router(graphql_router, prefix="/graphql", tags=["graphql"])
app.include_router(websocket_router, tags=["websocket"])


# Global Exception Handler
@app.exception_handler(Exception)
async def global_exception_handler(request, exc):
    """
    Global exception handler with Sentry integration.

    Logs all unhandled exceptions and returns appropriate error responses.
    """
    # Get correlation ID for tracking
    correlation_id = getattr(request.state, "correlation_id", "unknown")

    # Log exception
    logger.error(
        f"Unhandled exception",
        extra={
            "correlation_id": correlation_id,
            "path": request.url.path,
            "method": request.method,
        },
        exc_info=True,
    )

    # Send to Sentry
    if settings.SENTRY_DSN:
        sentry_sdk.capture_exception(exc)

    # Return error response
    if settings.ENVIRONMENT == "development":
        import traceback
        return JSONResponse(
            status_code=500,
            content={
                "error": str(exc),
                "type": exc.__class__.__name__,
                "correlation_id": correlation_id,
                "traceback": traceback.format_exc(),
            },
        )

    return JSONResponse(
        status_code=500,
        content={
            "error": "Internal server error",
            "correlation_id": correlation_id,
        },
    )


# Graceful shutdown handler
def handle_shutdown(signum, frame):
    """Handle graceful shutdown on SIGTERM/SIGINT."""
    logger.info(f"Received signal {signum}, initiating graceful shutdown...")
    sys.exit(0)


# Register signal handlers
signal.signal(signal.SIGTERM, handle_shutdown)
signal.signal(signal.SIGINT, handle_shutdown)


if __name__ == "__main__":
    import uvicorn

    # Production-ready uvicorn configuration
    uvicorn_config = {
        "app": "app.main:app",
        "host": settings.API_HOST,
        "port": settings.API_PORT,
        "log_level": settings.LOG_LEVEL.lower(),
        "access_log": True,
        "use_colors": True,
    }

    # Development-specific settings
    if settings.ENVIRONMENT == "development":
        uvicorn_config.update({
            "reload": True,
            "reload_dirs": ["app"],
        })
    else:
        # Production settings
        uvicorn_config.update({
            "workers": 4,  # Number of worker processes
            "loop": "uvloop",  # Use faster uvloop
            "http": "httptools",  # Use faster httptools
            "timeout_keep_alive": 5,
            "limit_concurrency": 1000,
            "limit_max_requests": 10000,  # Restart workers after N requests
        })

    logger.info(f"Starting server with config: {uvicorn_config}")
    uvicorn.run(**uvicorn_config)
