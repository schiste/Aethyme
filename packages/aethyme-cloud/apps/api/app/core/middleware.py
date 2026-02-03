"""
Enterprise-grade middleware for FastAPI application.

Includes:
- Rate limiting with headers
- Request correlation IDs
- Security headers
- Request/response logging
- Performance monitoring
"""

from fastapi import Request, Response
from starlette.middleware.base import BaseHTTPMiddleware
from starlette.middleware.cors import CORSMiddleware
from typing import Callable
import time
import uuid
import logging
from app.core.config import settings

logger = logging.getLogger(__name__)


class RateLimitHeadersMiddleware(BaseHTTPMiddleware):
    """
    Add rate limit headers to all responses.

    Headers added:
    - X-RateLimit-Limit: Maximum requests allowed
    - X-RateLimit-Remaining: Requests remaining
    - X-RateLimit-Reset: Timestamp when limit resets
    """

    async def dispatch(self, request: Request, call_next: Callable) -> Response:
        # Get rate limit info from request state (set by rate limit check)
        response = await call_next(request)

        # Add rate limit headers if available
        if hasattr(request.state, "rate_limit_info"):
            info = request.state.rate_limit_info
            response.headers["X-RateLimit-Limit"] = str(info.get("limit", 0))
            response.headers["X-RateLimit-Remaining"] = str(info.get("remaining", 0))
            response.headers["X-RateLimit-Reset"] = str(info.get("reset_at", 0))

        return response


class CorrelationIDMiddleware(BaseHTTPMiddleware):
    """
    Add correlation ID to all requests for distributed tracing.

    The correlation ID is:
    1. Taken from X-Correlation-ID header if present
    2. Generated as UUID if not present
    3. Added to response headers
    4. Available in request.state.correlation_id
    """

    async def dispatch(self, request: Request, call_next: Callable) -> Response:
        # Get or generate correlation ID
        correlation_id = request.headers.get("X-Correlation-ID")
        if not correlation_id:
            correlation_id = str(uuid.uuid4())

        # Store in request state
        request.state.correlation_id = correlation_id

        # Process request
        response = await call_next(request)

        # Add to response headers
        response.headers["X-Correlation-ID"] = correlation_id

        return response


class SecurityHeadersMiddleware(BaseHTTPMiddleware):
    """
    Add security headers to all responses.

    Headers added:
    - X-Content-Type-Options: nosniff
    - X-Frame-Options: DENY
    - X-XSS-Protection: 1; mode=block
    - Strict-Transport-Security: max-age=31536000; includeSubDomains
    - Content-Security-Policy: default-src 'self'
    - Referrer-Policy: strict-origin-when-cross-origin
    """

    async def dispatch(self, request: Request, call_next: Callable) -> Response:
        response = await call_next(request)

        # Security headers
        response.headers["X-Content-Type-Options"] = "nosniff"
        response.headers["X-Frame-Options"] = "DENY"
        response.headers["X-XSS-Protection"] = "1; mode=block"

        # HSTS (only in production)
        if settings.ENVIRONMENT == "production":
            response.headers["Strict-Transport-Security"] = (
                "max-age=31536000; includeSubDomains"
            )

        # CSP
        response.headers["Content-Security-Policy"] = (
            "default-src 'self'; "
            "script-src 'self' 'unsafe-inline' 'unsafe-eval'; "
            "style-src 'self' 'unsafe-inline'; "
            "img-src 'self' data: https:; "
            "font-src 'self' data:; "
            "connect-src 'self' https:; "
            "frame-ancestors 'none';"
        )

        # Referrer policy
        response.headers["Referrer-Policy"] = "strict-origin-when-cross-origin"

        # Remove server header
        response.headers.pop("server", None)

        return response


class RequestLoggingMiddleware(BaseHTTPMiddleware):
    """
    Log all requests and responses with timing information.

    Logs include:
    - Correlation ID
    - Method and path
    - Status code
    - Duration
    - User agent
    - IP address
    """

    async def dispatch(self, request: Request, call_next: Callable) -> Response:
        # Start timer
        start_time = time.time()

        # Get correlation ID
        correlation_id = getattr(request.state, "correlation_id", "unknown")

        # Get client IP
        client_ip = request.client.host if request.client else "unknown"

        # Log request
        logger.info(
            f"Request started",
            extra={
                "correlation_id": correlation_id,
                "method": request.method,
                "path": request.url.path,
                "client_ip": client_ip,
                "user_agent": request.headers.get("user-agent", "unknown"),
            }
        )

        # Process request
        try:
            response = await call_next(request)
            duration_ms = (time.time() - start_time) * 1000

            # Log response
            logger.info(
                f"Request completed",
                extra={
                    "correlation_id": correlation_id,
                    "method": request.method,
                    "path": request.url.path,
                    "status_code": response.status_code,
                    "duration_ms": round(duration_ms, 2),
                }
            )

            # Add timing header
            response.headers["X-Response-Time"] = f"{round(duration_ms, 2)}ms"

            return response

        except Exception as e:
            duration_ms = (time.time() - start_time) * 1000

            # Log error
            logger.error(
                f"Request failed",
                extra={
                    "correlation_id": correlation_id,
                    "method": request.method,
                    "path": request.url.path,
                    "duration_ms": round(duration_ms, 2),
                    "error": str(e),
                },
                exc_info=True,
            )

            raise


def get_cors_middleware() -> CORSMiddleware:
    """
    Configure CORS middleware with security best practices.

    Returns configured CORS middleware instance.
    """
    return CORSMiddleware(
        allow_origins=settings.CORS_ORIGINS,
        allow_credentials=True,
        allow_methods=["GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"],
        allow_headers=[
            "Authorization",
            "Content-Type",
            "X-Correlation-ID",
            "X-Admin-Bypass",
        ],
        expose_headers=[
            "X-Correlation-ID",
            "X-RateLimit-Limit",
            "X-RateLimit-Remaining",
            "X-RateLimit-Reset",
            "X-Response-Time",
        ],
        max_age=3600,  # Cache preflight requests for 1 hour
    )
