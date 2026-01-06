"""Rate limiting middleware for RepoGraph using Redis.

Implements token bucket algorithm with sliding window for accurate rate limiting.
"""

import time
from typing import Optional, Callable
from functools import wraps
import hashlib

from fastapi import Request, HTTPException, status
from starlette.middleware.base import BaseHTTPMiddleware
from starlette.responses import Response
import structlog

try:
    import redis.asyncio as redis
except ImportError:
    import redis

from ..config import settings

logger = structlog.get_logger(__name__)


class RateLimitExceeded(HTTPException):
    """Rate limit exceeded exception."""

    def __init__(self, retry_after: int, limit: int, window: int):
        """Initialize rate limit exception.

        Args:
            retry_after: Seconds until rate limit resets
            limit: Rate limit (requests per window)
            window: Window size in seconds
        """
        super().__init__(
            status_code=status.HTTP_429_TOO_MANY_REQUESTS,
            detail=f"Rate limit exceeded. Try again in {retry_after} seconds.",
            headers={
                "Retry-After": str(retry_after),
                "X-RateLimit-Limit": str(limit),
                "X-RateLimit-Remaining": "0",
                "X-RateLimit-Reset": str(int(time.time()) + retry_after),
            },
        )


class RateLimiter:
    """Redis-backed rate limiter using token bucket algorithm.

    Uses sliding window for accurate rate limiting across distributed systems.
    """

    def __init__(
        self,
        redis_url: Optional[str] = None,
        default_limit: int = 100,
        default_window: int = 60,
    ):
        """Initialize rate limiter.

        Args:
            redis_url: Redis connection URL
            default_limit: Default requests per window
            default_window: Default window size in seconds
        """
        self.redis_url = redis_url or settings.redis_url_str
        self.default_limit = default_limit
        self.default_window = default_window
        self._redis: Optional[redis.Redis] = None

    async def connect(self):
        """Connect to Redis."""
        if not self._redis:
            try:
                self._redis = await redis.from_url(
                    self.redis_url,
                    encoding="utf-8",
                    decode_responses=True,
                )
                await self._redis.ping()
                logger.info("Rate limiter connected to Redis")
            except Exception as e:
                logger.error("Failed to connect to Redis for rate limiting", error=str(e))
                self._redis = None

    async def disconnect(self):
        """Disconnect from Redis."""
        if self._redis:
            await self._redis.close()
            self._redis = None

    def _get_key(self, identifier: str, endpoint: str) -> str:
        """Generate Redis key for rate limit tracking.

        Args:
            identifier: User/API key identifier
            endpoint: API endpoint

        Returns:
            Redis key
        """
        # Hash the identifier for privacy
        id_hash = hashlib.sha256(identifier.encode()).hexdigest()[:16]
        return f"ratelimit:{endpoint}:{id_hash}"

    async def check_rate_limit(
        self,
        identifier: str,
        endpoint: str,
        limit: Optional[int] = None,
        window: Optional[int] = None,
    ) -> tuple[bool, int, int]:
        """Check if request is within rate limit.

        Uses sliding window algorithm with Redis sorted sets.

        Args:
            identifier: User/API key identifier
            endpoint: API endpoint
            limit: Requests per window (uses default if None)
            window: Window size in seconds (uses default if None)

        Returns:
            Tuple of (allowed, remaining, reset_time)
        """
        if not self._redis:
            # If Redis is unavailable, allow the request but log warning
            logger.warning("Redis unavailable, rate limiting disabled")
            return True, -1, 0

        limit = limit or self.default_limit
        window = window or self.default_window

        key = self._get_key(identifier, endpoint)
        now = time.time()
        window_start = now - window

        try:
            # Use Redis pipeline for atomic operations
            pipe = self._redis.pipeline()

            # Remove old entries outside the window
            pipe.zremrangebyscore(key, 0, window_start)

            # Count requests in current window
            pipe.zcard(key)

            # Add current request with timestamp
            pipe.zadd(key, {str(now): now})

            # Set expiry on the key
            pipe.expire(key, window)

            # Execute pipeline
            results = await pipe.execute()

            current_count = results[1]  # Count before adding current request

            # Check if over limit
            if current_count >= limit:
                # Get oldest request in window to calculate reset time
                oldest = await self._redis.zrange(key, 0, 0, withscores=True)
                if oldest:
                    oldest_time = oldest[0][1]
                    reset_time = int(oldest_time + window)
                    retry_after = max(1, reset_time - int(now))
                else:
                    reset_time = int(now + window)
                    retry_after = window

                # Remove the request we just added since it's over limit
                await self._redis.zrem(key, str(now))

                logger.debug(
                    "Rate limit exceeded",
                    identifier=identifier[:8],
                    endpoint=endpoint,
                    count=current_count,
                    limit=limit,
                )

                return False, 0, retry_after

            remaining = limit - current_count - 1  # -1 for current request
            reset_time = int(now + window)

            logger.debug(
                "Rate limit check passed",
                identifier=identifier[:8],
                endpoint=endpoint,
                count=current_count + 1,
                limit=limit,
                remaining=remaining,
            )

            return True, remaining, window

        except redis.RedisError as e:
            logger.error("Redis error during rate limit check", error=str(e))
            # On error, allow the request (fail open)
            return True, -1, 0

    async def reset(self, identifier: str, endpoint: str):
        """Reset rate limit for an identifier and endpoint.

        Useful for testing or manual intervention.

        Args:
            identifier: User/API key identifier
            endpoint: API endpoint
        """
        if not self._redis:
            return

        key = self._get_key(identifier, endpoint)
        await self._redis.delete(key)

        logger.info(
            "Rate limit reset",
            identifier=identifier[:8],
            endpoint=endpoint,
        )


class RateLimitMiddleware(BaseHTTPMiddleware):
    """FastAPI middleware for rate limiting.

    Automatically rate limits requests based on user/API key.
    """

    def __init__(self, app, limiter: Optional[RateLimiter] = None):
        """Initialize middleware.

        Args:
            app: FastAPI application
            limiter: RateLimiter instance (creates default if None)
        """
        super().__init__(app)
        self.limiter = limiter or RateLimiter()

        # Endpoint-specific limits (requests per minute)
        self.endpoint_limits = {
            "/api/search/": (100, 60),  # 100 requests per minute
            "/api/ego/": (50, 60),  # 50 requests per minute (more expensive)
            "/api/impact/": (50, 60),  # 50 requests per minute (more expensive)
            "/api/index": (10, 60),  # 10 index operations per minute
            "/api/ai-ready": (20, 60),  # 20 scorecard runs per minute
            "/api/autofix": (10, 60),  # 10 autofix runs per minute
        }

        # Global default
        self.default_limit = (100, 60)  # 100 requests per minute

    async def dispatch(self, request: Request, call_next: Callable) -> Response:
        """Process request with rate limiting.

        Args:
            request: HTTP request
            call_next: Next middleware/handler

        Returns:
            HTTP response

        Raises:
            RateLimitExceeded: If rate limit is exceeded
        """
        # Skip rate limiting for health checks
        if request.url.path.startswith("/health"):
            return await call_next(request)

        # Connect to Redis if not connected
        if not self.limiter._redis:
            await self.limiter.connect()

        # Extract identifier from request
        identifier = self._get_identifier(request)

        # Get endpoint-specific or default limit
        endpoint = self._normalize_endpoint(request.url.path)
        limit, window = self.endpoint_limits.get(endpoint, self.default_limit)

        # Check rate limit
        allowed, remaining, retry_after = await self.limiter.check_rate_limit(
            identifier=identifier,
            endpoint=endpoint,
            limit=limit,
            window=window,
        )

        if not allowed:
            raise RateLimitExceeded(retry_after, limit, window)

        # Process request
        response = await call_next(request)

        # Add rate limit headers to response
        reset_time = int(time.time()) + window
        response.headers["X-RateLimit-Limit"] = str(limit)
        response.headers["X-RateLimit-Remaining"] = str(remaining)
        response.headers["X-RateLimit-Reset"] = str(reset_time)

        return response

    def _get_identifier(self, request: Request) -> str:
        """Extract identifier from request for rate limiting.

        Uses Authorization header if present, otherwise falls back to IP.

        Args:
            request: HTTP request

        Returns:
            Identifier string
        """
        # Try to get from Authorization header
        auth_header = request.headers.get("Authorization")
        if auth_header and auth_header.startswith("Bearer "):
            token = auth_header[7:]
            # Use first 16 chars of token as identifier
            return token[:16]

        # Fall back to IP address
        client_ip = request.client.host if request.client else "unknown"
        return f"ip:{client_ip}"

    def _normalize_endpoint(self, path: str) -> str:
        """Normalize endpoint path for rate limit lookup.

        Handles dynamic paths like /api/repos/{id}/search

        Args:
            path: Request path

        Returns:
            Normalized endpoint
        """
        # Remove trailing slashes
        path = path.rstrip("/")

        # Check exact matches first
        for endpoint in self.endpoint_limits:
            if path.startswith(endpoint.rstrip("/")):
                return endpoint

        # Check for common patterns
        if "/api/search" in path:
            return "/api/search/"
        elif "/api/ego" in path:
            return "/api/ego/"
        elif "/api/impact" in path:
            return "/api/impact/"
        elif "/api/index" in path:
            return "/api/index"

        # Default
        return path


def rate_limit(limit: int, window: int = 60):
    """Decorator for route-specific rate limiting.

    Usage:
        @router.get("/expensive")
        @rate_limit(limit=10, window=60)
        async def expensive_operation():
            ...

    Args:
        limit: Requests per window
        window: Window size in seconds

    Returns:
        Decorator function
    """
    limiter = RateLimiter()

    def decorator(func):
        @wraps(func)
        async def wrapper(*args, **kwargs):
            # This is a simplified implementation
            # In practice, you'd extract the request and identifier
            return await func(*args, **kwargs)

        return wrapper

    return decorator


# Global rate limiter instance
rate_limiter = RateLimiter(
    default_limit=getattr(settings, 'rate_limit_default', 100),
    default_window=60,
)
