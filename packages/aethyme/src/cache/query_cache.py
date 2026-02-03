"""
Redis-backed query caching with TTL and invalidation.

Features:
- Cache key generation (query hash + tenant)
- TTL: 5 minutes for search, 10 minutes for ego/impact
- Cache invalidation on new index
- Cache hit/miss metrics
"""

import hashlib
import json
from typing import Optional, Dict, Any
import redis
import structlog
from datetime import datetime, timedelta

logger = structlog.get_logger(__name__)


class QueryCache:
    """Redis-backed cache for query results."""

    def __init__(self, redis_client: redis.Redis):
        """
        Initialize query cache.

        Args:
            redis_client: Redis client instance
        """
        self.redis = redis_client
        self.ttl_search = 300  # 5 minutes
        self.ttl_ego = 600  # 10 minutes
        self.ttl_impact = 600  # 10 minutes

    def _generate_cache_key(
        self,
        tenant_id: str,
        query_type: str,
        params: Dict[str, Any]
    ) -> str:
        """
        Generate cache key from query parameters.

        Format: query:{tenant_id}:{query_type}:{hash(params)}

        Args:
            tenant_id: Tenant UUID
            query_type: search, ego, or impact
            params: Query parameters dict

        Returns:
            Cache key string
        """
        # Sort params for consistent hashing
        param_str = json.dumps(params, sort_keys=True)
        param_hash = hashlib.sha256(param_str.encode()).hexdigest()[:16]

        return f"query:{tenant_id}:{query_type}:{param_hash}"

    def get(
        self,
        tenant_id: str,
        query_type: str,
        params: Dict[str, Any]
    ) -> Optional[Dict[str, Any]]:
        """
        Get cached query result.

        Args:
            tenant_id: Tenant UUID
            query_type: search, ego, or impact
            params: Query parameters

        Returns:
            Cached result dict or None if miss
        """
        cache_key = self._generate_cache_key(tenant_id, query_type, params)

        try:
            cached_data = self.redis.get(cache_key)

            if cached_data:
                logger.info("Cache hit", key=cache_key, query_type=query_type)
                return json.loads(cached_data)

            logger.debug("Cache miss", key=cache_key, query_type=query_type)
            return None

        except Exception as e:
            logger.error("Cache get error", error=str(e), key=cache_key)
            return None  # Cache errors are non-fatal

    def set(
        self,
        tenant_id: str,
        query_type: str,
        params: Dict[str, Any],
        result: Dict[str, Any]
    ) -> bool:
        """
        Cache query result with TTL.

        Args:
            tenant_id: Tenant UUID
            query_type: search, ego, or impact
            params: Query parameters
            result: Result to cache

        Returns:
            True if cached successfully
        """
        cache_key = self._generate_cache_key(tenant_id, query_type, params)

        # Determine TTL based on query type
        ttl_map = {
            "search": self.ttl_search,
            "ego": self.ttl_ego,
            "impact": self.ttl_impact,
        }
        ttl = ttl_map.get(query_type, self.ttl_search)

        try:
            # Serialize result
            cached_data = json.dumps(result, default=str)

            # Set with TTL
            self.redis.setex(cache_key, ttl, cached_data)

            logger.info("Cache set", key=cache_key, ttl=ttl, query_type=query_type)
            return True

        except Exception as e:
            logger.error("Cache set error", error=str(e), key=cache_key)
            return False

    def invalidate_pattern(self, pattern: str) -> int:
        """
        Invalidate all keys matching pattern.

        Args:
            pattern: Redis key pattern (e.g., "query:tenant-id:*")

        Returns:
            Number of keys deleted
        """
        try:
            cursor = 0
            deleted = 0

            while True:
                cursor, keys = self.redis.scan(cursor, match=pattern, count=100)

                if keys:
                    deleted += self.redis.delete(*keys)

                if cursor == 0:
                    break

            logger.info("Cache invalidated", pattern=pattern, deleted=deleted)
            return deleted

        except Exception as e:
            logger.error("Cache invalidation error", error=str(e), pattern=pattern)
            return 0

    def invalidate_tenant(self, tenant_id: str) -> int:
        """
        Invalidate all cache for a tenant.

        Args:
            tenant_id: Tenant UUID

        Returns:
            Number of keys deleted
        """
        pattern = f"query:{tenant_id}:*"
        return self.invalidate_pattern(pattern)

    def invalidate_repository(self, tenant_id: str, repository_id: str) -> int:
        """
        Invalidate cache for a specific repository.

        Args:
            tenant_id: Tenant UUID
            repository_id: Repository UUID

        Returns:
            Number of keys deleted
        """
        # For now, invalidate all tenant cache
        # TODO: Add repository-specific tracking
        return self.invalidate_tenant(tenant_id)

    def get_stats(self) -> Dict[str, Any]:
        """
        Get cache statistics.

        Returns:
            Dict with cache stats (hits, misses, size, etc.)
        """
        try:
            info = self.redis.info("stats")

            return {
                "hits": info.get("keyspace_hits", 0),
                "misses": info.get("keyspace_misses", 0),
                "hit_rate": self._calculate_hit_rate(
                    info.get("keyspace_hits", 0),
                    info.get("keyspace_misses", 0)
                ),
                "total_keys": self.redis.dbsize(),
                "memory_used": info.get("used_memory_human", "unknown"),
            }

        except Exception as e:
            logger.error("Cache stats error", error=str(e))
            return {}

    def _calculate_hit_rate(self, hits: int, misses: int) -> float:
        """Calculate cache hit rate as percentage."""
        total = hits + misses
        if total == 0:
            return 0.0
        return (hits / total) * 100


# Global cache instance
_cache: Optional[QueryCache] = None


def get_query_cache() -> QueryCache:
    """Get or create query cache instance."""
    global _cache

    if _cache is None:
        import os

        redis_client = redis.Redis(
            host=os.getenv("REDIS_HOST", "localhost"),
            port=int(os.getenv("REDIS_PORT", "6379")),
            db=int(os.getenv("REDIS_DB", "0")),
            decode_responses=True,
            socket_connect_timeout=1.0,
            socket_timeout=1.0,
        )

        _cache = QueryCache(redis_client)

    return _cache
