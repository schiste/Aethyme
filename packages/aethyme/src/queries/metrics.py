"""
Prometheus metrics for query performance monitoring.

Metrics:
- aethyme_query_duration_seconds{type, cache_hit} (histogram)
- aethyme_query_cache_hits_total (counter)
- aethyme_query_cache_misses_total (counter)
- aethyme_query_errors_total (counter)
"""

from prometheus_client import Counter, Histogram, Gauge
import time
from contextlib import contextmanager
from typing import Optional
import structlog

logger = structlog.get_logger(__name__)


# Query duration histogram with buckets optimized for our targets
# Target: <500ms (no cache), <50ms (with cache)
query_duration = Histogram(
    "aethyme_query_duration_seconds",
    "Query execution duration in seconds",
    labelnames=["query_type", "cache_hit"],
    buckets=[0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0],
)

# Cache metrics
cache_hits = Counter(
    "aethyme_query_cache_hits_total",
    "Total number of cache hits",
    labelnames=["query_type"],
)

cache_misses = Counter(
    "aethyme_query_cache_misses_total",
    "Total number of cache misses",
    labelnames=["query_type"],
)

# Error metrics
query_errors = Counter(
    "aethyme_query_errors_total",
    "Total number of query errors",
    labelnames=["query_type", "error_type"],
)

# Active queries gauge
active_queries = Gauge(
    "aethyme_active_queries",
    "Number of currently executing queries",
    labelnames=["query_type"],
)

# Result size histogram
result_size = Histogram(
    "aethyme_query_result_count",
    "Number of results returned by query",
    labelnames=["query_type"],
    buckets=[1, 5, 10, 20, 50, 100, 200, 500, 1000],
)


@contextmanager
def track_query(query_type: str, cache_hit: bool = False):
    """
    Context manager to track query metrics.

    Usage:
        with track_query("search", cache_hit=True):
            result = perform_search()

    Args:
        query_type: Type of query (search, ego, impact)
        cache_hit: Whether this was a cache hit
    """
    start_time = time.time()
    active_queries.labels(query_type=query_type).inc()

    try:
        yield

        # Record success
        duration = time.time() - start_time
        query_duration.labels(
            query_type=query_type,
            cache_hit=str(cache_hit).lower()
        ).observe(duration)

        # Track cache hit/miss
        if cache_hit:
            cache_hits.labels(query_type=query_type).inc()
        else:
            cache_misses.labels(query_type=query_type).inc()

        logger.info(
            "Query completed",
            query_type=query_type,
            duration_seconds=duration,
            cache_hit=cache_hit,
        )

    except Exception as e:
        # Record error
        error_type = type(e).__name__
        query_errors.labels(
            query_type=query_type,
            error_type=error_type
        ).inc()

        logger.error(
            "Query error",
            query_type=query_type,
            error=str(e),
            error_type=error_type,
        )
        raise

    finally:
        active_queries.labels(query_type=query_type).dec()


def record_result_size(query_type: str, count: int):
    """
    Record the number of results returned.

    Args:
        query_type: Type of query
        count: Number of results
    """
    result_size.labels(query_type=query_type).observe(count)


def get_cache_hit_rate(query_type: Optional[str] = None) -> float:
    """
    Calculate cache hit rate.

    Args:
        query_type: Optional query type filter

    Returns:
        Hit rate as percentage (0-100)
    """
    # This is a simplified version - in production, you'd query Prometheus
    # For now, we'll return a placeholder
    return 0.0  # TODO: Query Prometheus API
