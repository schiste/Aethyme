"""
Redis Infrastructure for Phase 13 Multi-Server Scaling

Provides Redis connection management, pub/sub, and distributed state management
for horizontal scaling of the collaboration system.
"""

from app.redis.connection import RedisConnectionManager, get_redis
from app.redis.pubsub import RedisPubSubManager

__all__ = [
    "RedisConnectionManager",
    "RedisPubSubManager",
    "get_redis",
]
