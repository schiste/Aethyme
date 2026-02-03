"""
Cache invalidation strategies.

Features:
- Invalidate cache when repo re-indexed
- Tenant-scoped invalidation
- Selective invalidation (only affected queries)
- Staleness tracking
"""

from typing import Optional, Dict, Any
from datetime import datetime, timedelta
import structlog

from .query_cache import get_query_cache

logger = structlog.get_logger(__name__)


class CacheInvalidationService:
    """Service for intelligent cache invalidation."""

    def __init__(self):
        self.cache = get_query_cache()

    def on_repository_indexed(
        self,
        tenant_id: str,
        repository_id: str
    ) -> Dict[str, int]:
        """
        Invalidate cache when repository is re-indexed.

        Args:
            tenant_id: Tenant UUID
            repository_id: Repository UUID

        Returns:
            Dict with invalidation stats
        """
        logger.info(
            "Invalidating cache for re-indexed repository",
            tenant_id=tenant_id,
            repository_id=repository_id
        )

        # Invalidate all queries for this repository
        deleted = self.cache.invalidate_repository(tenant_id, repository_id)

        # Track last indexed time
        self._update_last_indexed(tenant_id, repository_id)

        return {
            "keys_deleted": deleted,
            "repository_id": repository_id,
            "timestamp": datetime.utcnow().isoformat(),
        }

    def on_repository_deleted(
        self,
        tenant_id: str,
        repository_id: str
    ) -> Dict[str, int]:
        """
        Invalidate cache when repository is deleted.

        Args:
            tenant_id: Tenant UUID
            repository_id: Repository UUID

        Returns:
            Dict with invalidation stats
        """
        logger.info(
            "Invalidating cache for deleted repository",
            tenant_id=tenant_id,
            repository_id=repository_id
        )

        deleted = self.cache.invalidate_repository(tenant_id, repository_id)

        return {
            "keys_deleted": deleted,
            "repository_id": repository_id,
        }

    def selective_invalidate(
        self,
        tenant_id: str,
        affected_symbols: list[str]
    ) -> Dict[str, int]:
        """
        Selectively invalidate only queries affected by changes.

        Args:
            tenant_id: Tenant UUID
            affected_symbols: List of changed symbol names

        Returns:
            Dict with invalidation stats
        """
        # For now, invalidate all tenant cache
        # TODO: Implement precise invalidation based on affected symbols
        deleted = self.cache.invalidate_tenant(tenant_id)

        return {
            "keys_deleted": deleted,
            "affected_symbols_count": len(affected_symbols),
        }

    def _update_last_indexed(
        self,
        tenant_id: str,
        repository_id: str
    ):
        """Track last indexed timestamp."""
        key = f"last_indexed:{tenant_id}:{repository_id}"
        timestamp = datetime.utcnow().isoformat()

        try:
            self.cache.redis.set(key, timestamp)
        except Exception as e:
            logger.error("Error tracking last indexed", error=str(e))

    def get_last_indexed(
        self,
        tenant_id: str,
        repository_id: str
    ) -> Optional[datetime]:
        """Get last indexed timestamp for repository."""
        key = f"last_indexed:{tenant_id}:{repository_id}"

        try:
            timestamp_str = self.cache.redis.get(key)
            if timestamp_str:
                return datetime.fromisoformat(timestamp_str)
        except Exception as e:
            logger.error("Error getting last indexed", error=str(e))

        return None

    def is_stale(
        self,
        tenant_id: str,
        repository_id: str,
        max_age_hours: int = 24
    ) -> bool:
        """
        Check if repository data is stale.

        Args:
            tenant_id: Tenant UUID
            repository_id: Repository UUID
            max_age_hours: Maximum age before considering stale

        Returns:
            True if stale
        """
        last_indexed = self.get_last_indexed(tenant_id, repository_id)

        if last_indexed is None:
            return True  # Never indexed

        age = datetime.utcnow() - last_indexed
        return age > timedelta(hours=max_age_hours)


# Global service instance
_service: Optional[CacheInvalidationService] = None


def get_invalidation_service() -> CacheInvalidationService:
    """Get cache invalidation service instance."""
    global _service

    if _service is None:
        _service = CacheInvalidationService()

    return _service
