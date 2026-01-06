"""
Enterprise-grade rate limiting with token bucket algorithm and Redis backend.

Features:
- Redis-backed storage for distributed rate limiting
- Token bucket algorithm for burst handling
- Per-endpoint rate limit configuration
- Admin bypass capability
- Rate limit headers in responses
- Configurable limits per user tier
"""

from slowapi import Limiter
from slowapi.util import get_remote_address
from fastapi import Request, HTTPException
from typing import Optional, Dict, Any
import redis.asyncio as aioredis
import time
from app.core.config import settings
import logging

logger = logging.getLogger(__name__)


class TokenBucket:
    """
    Token bucket algorithm implementation for rate limiting.

    Allows for burst traffic while maintaining average rate limits.
    """

    def __init__(
        self,
        redis_client: aioredis.Redis,
        capacity: int,
        refill_rate: int,  # tokens per second
        key_prefix: str = "rate_limit"
    ):
        self.redis = redis_client
        self.capacity = capacity
        self.refill_rate = refill_rate
        self.key_prefix = key_prefix

    async def consume(self, key: str, tokens: int = 1) -> tuple[bool, Dict[str, Any]]:
        """
        Attempt to consume tokens from the bucket.

        Returns:
            (allowed, info) where info contains:
            - remaining: tokens remaining
            - reset_at: timestamp when bucket will be full
            - retry_after: seconds to wait before retry (if not allowed)
        """
        bucket_key = f"{self.key_prefix}:{key}"
        current_time = time.time()

        # Lua script for atomic token bucket operation
        lua_script = """
        local bucket_key = KEYS[1]
        local capacity = tonumber(ARGV[1])
        local refill_rate = tonumber(ARGV[2])
        local tokens_requested = tonumber(ARGV[3])
        local current_time = tonumber(ARGV[4])

        -- Get current bucket state
        local bucket = redis.call('HMGET', bucket_key, 'tokens', 'last_refill')
        local tokens = tonumber(bucket[1]) or capacity
        local last_refill = tonumber(bucket[2]) or current_time

        -- Calculate tokens to add based on time elapsed
        local time_elapsed = current_time - last_refill
        local tokens_to_add = time_elapsed * refill_rate
        tokens = math.min(capacity, tokens + tokens_to_add)

        -- Try to consume tokens
        local allowed = 0
        local remaining = tokens

        if tokens >= tokens_requested then
            tokens = tokens - tokens_requested
            remaining = tokens
            allowed = 1
        end

        -- Update bucket state
        redis.call('HMSET', bucket_key, 'tokens', tokens, 'last_refill', current_time)
        redis.call('EXPIRE', bucket_key, 3600)  -- 1 hour expiry

        return {allowed, remaining, capacity}
        """

        try:
            result = await self.redis.eval(
                lua_script,
                1,
                bucket_key,
                self.capacity,
                self.refill_rate,
                tokens,
                current_time
            )

            allowed = bool(result[0])
            remaining = int(result[1])
            capacity = int(result[2])

            # Calculate reset time (when bucket will be full)
            tokens_needed = capacity - remaining
            seconds_to_full = tokens_needed / self.refill_rate if self.refill_rate > 0 else 0
            reset_at = int(current_time + seconds_to_full)

            # Calculate retry_after if not allowed
            retry_after = int((tokens - remaining + tokens) / self.refill_rate) if not allowed else 0

            info = {
                "remaining": remaining,
                "capacity": capacity,
                "reset_at": reset_at,
                "retry_after": retry_after
            }

            return allowed, info

        except Exception as e:
            logger.error(f"Rate limit check failed: {e}")
            # Fail open - allow request if rate limiting fails
            return True, {
                "remaining": self.capacity,
                "capacity": self.capacity,
                "reset_at": int(current_time + 60),
                "retry_after": 0
            }


# Global Redis connection for rate limiting
_redis_client: Optional[aioredis.Redis] = None


async def get_redis_client() -> aioredis.Redis:
    """Get or create Redis client for rate limiting."""
    global _redis_client
    if _redis_client is None:
        _redis_client = aioredis.from_url(
            str(settings.REDIS_URL),
            encoding="utf-8",
            decode_responses=False,  # We need bytes for Lua scripts
        )
    return _redis_client


def get_rate_limit_key(request: Request) -> str:
    """
    Get rate limit key from request.

    Priority:
    1. API key (if present in Authorization header starting with "rgph_")
    2. User ID (if authenticated with JWT)
    3. IP address (fallback)
    """
    auth_header = request.headers.get("Authorization", "")

    # Check for API key
    if auth_header.startswith("Bearer rgph_"):
        api_key_prefix = auth_header[7:20]  # Get "rgph_liv_xxx" part
        return f"api_key:{api_key_prefix}"

    # Check for admin bypass header (for system operations)
    if request.headers.get("X-Admin-Bypass") == settings.JWT_SECRET_KEY[:16]:
        return "admin:bypass"

    # Check for JWT token (would need to decode to get user ID)
    # For now, fall back to IP

    # Fallback to IP address
    return f"ip:{get_remote_address(request)}"


def is_admin_bypass(request: Request) -> bool:
    """Check if request has admin bypass for rate limiting."""
    return request.headers.get("X-Admin-Bypass") == settings.JWT_SECRET_KEY[:16]


# Rate limit configurations per endpoint type
RATE_LIMITS = {
    "default": {"capacity": 100, "refill_rate": 100 / 60},  # 100 per minute
    "search": {"capacity": 50, "refill_rate": 50 / 60},  # 50 per minute
    "indexing": {"capacity": 20, "refill_rate": 20 / 60},  # 20 per minute
    "ai": {"capacity": 30, "refill_rate": 30 / 60},  # 30 per minute
    "auth": {"capacity": 10, "refill_rate": 10 / 60},  # 10 per minute (stricter for auth)
}


async def check_rate_limit(
    request: Request,
    endpoint_type: str = "default"
) -> Dict[str, Any]:
    """
    Check rate limit for the request.

    Args:
        request: FastAPI request object
        endpoint_type: Type of endpoint (default, search, indexing, ai, auth)

    Returns:
        Rate limit info dict

    Raises:
        HTTPException: If rate limit exceeded
    """
    # Admin bypass
    if is_admin_bypass(request):
        return {
            "allowed": True,
            "remaining": 999999,
            "capacity": 999999,
            "reset_at": int(time.time() + 3600)
        }

    # Get rate limit config
    config = RATE_LIMITS.get(endpoint_type, RATE_LIMITS["default"])

    # Get Redis client and create token bucket
    redis_client = await get_redis_client()
    bucket = TokenBucket(
        redis_client=redis_client,
        capacity=config["capacity"],
        refill_rate=config["refill_rate"],
        key_prefix=f"rate_limit:{endpoint_type}"
    )

    # Get rate limit key
    key = get_rate_limit_key(request)

    # Check rate limit
    allowed, info = await bucket.consume(key)

    if not allowed:
        # Add rate limit headers
        logger.warning(f"Rate limit exceeded for {key} on {endpoint_type}")
        raise HTTPException(
            status_code=429,
            detail={
                "error": "Rate limit exceeded",
                "retry_after": info["retry_after"],
                "reset_at": info["reset_at"]
            },
            headers={
                "X-RateLimit-Limit": str(config["capacity"]),
                "X-RateLimit-Remaining": "0",
                "X-RateLimit-Reset": str(info["reset_at"]),
                "Retry-After": str(info["retry_after"])
            }
        )

    # Return rate limit info (can be added to response headers)
    return {
        "allowed": True,
        "limit": config["capacity"],
        "remaining": info["remaining"],
        "reset_at": info["reset_at"]
    }


# Create limiter instance (for slowapi compatibility)
limiter = Limiter(
    key_func=get_rate_limit_key,
    default_limits=[
        f"{settings.RATE_LIMIT_PER_MINUTE}/minute",
        f"{settings.RATE_LIMIT_PER_HOUR}/hour",
        f"{settings.RATE_LIMIT_PER_DAY}/day"
    ],
    storage_uri=str(settings.REDIS_URL),
    strategy="fixed-window-elastic-expiry",
    headers_enabled=True,
)
