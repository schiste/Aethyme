"""
Redis-backed caching layer for Aethyme queries.

Features:
- Query result caching with TTL
- Cache key generation
- Pattern-based invalidation
- Staleness tracking
- Cache statistics
"""

from .query_cache import (
    QueryCache,
    get_query_cache,
)
from .invalidation import (
    CacheInvalidationService,
    get_invalidation_service,
)

__all__ = [
    # Cache
    "QueryCache",
    "get_query_cache",
    # Invalidation
    "CacheInvalidationService",
    "get_invalidation_service",
]
