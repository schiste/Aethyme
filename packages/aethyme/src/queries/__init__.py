"""
Optimized query implementations for Aethyme.

High-performance search, ego graph, and impact analysis queries with:
- AsyncPG connection pooling
- Database index optimization
- Query timeout protection
- N+1 query prevention
- Prometheus metrics integration
"""

from .optimized_search import (
    OptimizedSearchService,
    get_pool,
    close_pool,
    get_search_service,
)
from .metrics import (
    track_query,
    record_result_size,
    get_cache_hit_rate,
    query_duration,
    cache_hits,
    cache_misses,
    query_errors,
    active_queries,
    result_size,
)

__all__ = [
    # Search service
    "OptimizedSearchService",
    "get_pool",
    "close_pool",
    "get_search_service",
    # Metrics
    "track_query",
    "record_result_size",
    "get_cache_hit_rate",
    "query_duration",
    "cache_hits",
    "cache_misses",
    "query_errors",
    "active_queries",
    "result_size",
]
