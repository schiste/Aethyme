"""
Redis Connection Manager

Manages Redis connection pool for pub/sub and distributed state management.
Provides graceful degradation when Redis is unavailable.
"""

import asyncio
import logging
import os
from typing import Optional

import redis.asyncio as aioredis
from redis.asyncio import ConnectionPool

logger = logging.getLogger(__name__)


class RedisConnectionManager:
    """
    Manages Redis connection pool with health checks and graceful degradation.
    """

    def __init__(
        self,
        redis_url: Optional[str] = None,
        max_connections: int = 50,
        socket_timeout: int = 5,
        socket_connect_timeout: int = 5,
        decode_responses: bool = True,
    ):
        self.redis_url = redis_url or os.getenv("REDIS_URL", "redis://localhost:6379/0")
        self.max_connections = max_connections
        self.socket_timeout = socket_timeout
        self.socket_connect_timeout = socket_connect_timeout
        self.decode_responses = decode_responses

        self._pool: Optional[ConnectionPool] = None
        self._client: Optional[aioredis.Redis] = None
        self._enabled = os.getenv("COLLABORATION_REDIS_ENABLED", "true").lower() == "true"
        self._health_check_task: Optional[asyncio.Task] = None
        self._is_healthy = False

    async def connect(self) -> bool:
        """
        Connect to Redis and create connection pool.
        Returns True if connection successful, False otherwise.
        """
        if not self._enabled:
            logger.info("Redis is disabled via configuration")
            return False

        try:
            # Create connection pool
            self._pool = ConnectionPool.from_url(
                self.redis_url,
                max_connections=self.max_connections,
                socket_timeout=self.socket_timeout,
                socket_connect_timeout=self.socket_connect_timeout,
                decode_responses=self.decode_responses,
            )

            # Create Redis client
            self._client = aioredis.Redis(connection_pool=self._pool)

            # Test connection
            await self._client.ping()
            self._is_healthy = True

            logger.info(f"✅ Redis connected: {self.redis_url}")

            # Start health check task
            self._health_check_task = asyncio.create_task(self._health_check_loop())

            return True

        except Exception as e:
            logger.error(f"❌ Failed to connect to Redis: {e}")
            logger.warning("⚠️ Collaboration system will run in single-server mode")
            self._enabled = False
            return False

    async def disconnect(self):
        """Disconnect from Redis and cleanup resources."""
        # Cancel health check
        if self._health_check_task:
            self._health_check_task.cancel()
            try:
                await self._health_check_task
            except asyncio.CancelledError:
                pass

        # Close client
        if self._client:
            await self._client.close()
            self._client = None

        # Close pool
        if self._pool:
            await self._pool.disconnect()
            self._pool = None

        self._is_healthy = False
        logger.info("Redis disconnected")

    @property
    def is_enabled(self) -> bool:
        """Check if Redis is enabled."""
        return self._enabled

    @property
    def is_healthy(self) -> bool:
        """Check if Redis connection is healthy."""
        return self._is_healthy and self._client is not None

    @property
    def client(self) -> Optional[aioredis.Redis]:
        """Get Redis client (None if not connected)."""
        return self._client if self._is_healthy else None

    async def _health_check_loop(self):
        """Background task to check Redis health every 10 seconds."""
        while True:
            try:
                await asyncio.sleep(10)

                if self._client:
                    await self._client.ping()
                    if not self._is_healthy:
                        logger.info("✅ Redis connection restored")
                    self._is_healthy = True
                else:
                    self._is_healthy = False

            except asyncio.CancelledError:
                break
            except Exception as e:
                if self._is_healthy:
                    logger.error(f"❌ Redis health check failed: {e}")
                self._is_healthy = False

    async def set(self, key: str, value: str, ex: Optional[int] = None) -> bool:
        """
        Set a key-value pair in Redis.

        Args:
            key: Redis key
            value: Value to store
            ex: Expiration time in seconds (optional)

        Returns:
            True if successful, False otherwise
        """
        if not self.client:
            return False

        try:
            await self.client.set(key, value, ex=ex)
            return True
        except Exception as e:
            logger.error(f"Failed to set Redis key {key}: {e}")
            return False

    async def get(self, key: str) -> Optional[str]:
        """
        Get value by key from Redis.

        Args:
            key: Redis key

        Returns:
            Value if found, None otherwise
        """
        if not self.client:
            return None

        try:
            return await self.client.get(key)
        except Exception as e:
            logger.error(f"Failed to get Redis key {key}: {e}")
            return None

    async def delete(self, key: str) -> bool:
        """
        Delete a key from Redis.

        Args:
            key: Redis key

        Returns:
            True if successful, False otherwise
        """
        if not self.client:
            return False

        try:
            await self.client.delete(key)
            return True
        except Exception as e:
            logger.error(f"Failed to delete Redis key {key}: {e}")
            return False

    async def hset(self, key: str, mapping: dict) -> bool:
        """
        Set hash fields.

        Args:
            key: Redis key
            mapping: Dictionary of field-value pairs

        Returns:
            True if successful, False otherwise
        """
        if not self.client:
            return False

        try:
            await self.client.hset(key, mapping=mapping)
            return True
        except Exception as e:
            logger.error(f"Failed to hset Redis key {key}: {e}")
            return False

    async def hgetall(self, key: str) -> dict:
        """
        Get all hash fields and values.

        Args:
            key: Redis key

        Returns:
            Dictionary of field-value pairs, empty dict if error
        """
        if not self.client:
            return {}

        try:
            return await self.client.hgetall(key)
        except Exception as e:
            logger.error(f"Failed to hgetall Redis key {key}: {e}")
            return {}

    async def sadd(self, key: str, *members: str) -> bool:
        """
        Add members to a set.

        Args:
            key: Redis key
            members: Members to add

        Returns:
            True if successful, False otherwise
        """
        if not self.client:
            return False

        try:
            await self.client.sadd(key, *members)
            return True
        except Exception as e:
            logger.error(f"Failed to sadd Redis key {key}: {e}")
            return False

    async def smembers(self, key: str) -> set:
        """
        Get all members of a set.

        Args:
            key: Redis key

        Returns:
            Set of members, empty set if error
        """
        if not self.client:
            return set()

        try:
            return await self.client.smembers(key)
        except Exception as e:
            logger.error(f"Failed to smembers Redis key {key}: {e}")
            return set()

    async def srem(self, key: str, *members: str) -> bool:
        """
        Remove members from a set.

        Args:
            key: Redis key
            members: Members to remove

        Returns:
            True if successful, False otherwise
        """
        if not self.client:
            return False

        try:
            await self.client.srem(key, *members)
            return True
        except Exception as e:
            logger.error(f"Failed to srem Redis key {key}: {e}")
            return False

    async def expire(self, key: str, seconds: int) -> bool:
        """
        Set expiration time for a key.

        Args:
            key: Redis key
            seconds: Expiration time in seconds

        Returns:
            True if successful, False otherwise
        """
        if not self.client:
            return False

        try:
            await self.client.expire(key, seconds)
            return True
        except Exception as e:
            logger.error(f"Failed to expire Redis key {key}: {e}")
            return False


# Global Redis connection manager instance
_redis_manager: Optional[RedisConnectionManager] = None


def get_redis() -> Optional[RedisConnectionManager]:
    """Get the global Redis connection manager instance."""
    return _redis_manager


async def init_redis() -> RedisConnectionManager:
    """Initialize the global Redis connection manager."""
    global _redis_manager

    if _redis_manager is None:
        _redis_manager = RedisConnectionManager()
        await _redis_manager.connect()

    return _redis_manager


async def close_redis():
    """Close the global Redis connection manager."""
    global _redis_manager

    if _redis_manager:
        await _redis_manager.disconnect()
        _redis_manager = None
