"""
Server Registry

Tracks active collaboration servers and their health status using Redis.
"""

import asyncio
import json
import logging
import os
import socket
import time
from datetime import datetime
from typing import Dict, List, Optional
from uuid import uuid4

from app.redis.connection import RedisConnectionManager
from app.redis.pubsub import RedisPubSubManager

logger = logging.getLogger(__name__)


class ServerInfo:
    """Information about a collaboration server."""

    def __init__(
        self,
        server_id: str,
        host: str,
        port: int,
        connections: int = 0,
        started_at: Optional[str] = None,
        last_heartbeat: Optional[str] = None,
    ):
        self.server_id = server_id
        self.host = host
        self.port = port
        self.connections = connections
        self.started_at = started_at or datetime.utcnow().isoformat()
        self.last_heartbeat = last_heartbeat or datetime.utcnow().isoformat()

    def to_dict(self) -> Dict:
        """Convert to dictionary."""
        return {
            "server_id": self.server_id,
            "host": self.host,
            "port": self.port,
            "connections": self.connections,
            "started_at": self.started_at,
            "last_heartbeat": self.last_heartbeat,
        }

    @classmethod
    def from_dict(cls, data: Dict) -> "ServerInfo":
        """Create from dictionary."""
        return cls(
            server_id=data["server_id"],
            host=data["host"],
            port=data["port"],
            connections=int(data.get("connections", 0)),
            started_at=data.get("started_at"),
            last_heartbeat=data.get("last_heartbeat"),
        )


class ServerRegistry:
    """
    Manages server registration and health monitoring using Redis.
    """

    def __init__(
        self,
        redis_manager: RedisConnectionManager,
        pubsub_manager: RedisPubSubManager,
    ):
        self.redis_manager = redis_manager
        self.pubsub_manager = pubsub_manager

        # Server info
        self.server_id = os.getenv("SERVER_ID") or f"server-{uuid4().hex[:8]}"
        self.host = os.getenv("SERVER_HOST") or socket.gethostname()
        self.port = int(os.getenv("SERVER_PORT", "8000"))

        # Heartbeat configuration
        self.heartbeat_interval = int(
            os.getenv("COLLABORATION_SERVER_HEARTBEAT_INTERVAL", "10")
        )
        self.server_timeout = int(os.getenv("COLLABORATION_SERVER_TIMEOUT", "30"))

        # State
        self._registered = False
        self._heartbeat_task: Optional[asyncio.Task] = None
        self._cleanup_task: Optional[asyncio.Task] = None
        self._connection_count = 0
        self._started_at = datetime.utcnow().isoformat()

    async def register(self) -> bool:
        """
        Register this server with the registry.

        Returns:
            True if successful, False otherwise
        """
        if not self.redis_manager.is_healthy:
            logger.warning("Cannot register server: Redis not healthy")
            return False

        try:
            # Create server info
            server_info = ServerInfo(
                server_id=self.server_id,
                host=self.host,
                port=self.port,
                connections=self._connection_count,
                started_at=self._started_at,
            )

            # Store in Redis with TTL
            key = f"collaboration:server:{self.server_id}"
            await self.redis_manager.hset(key, server_info.to_dict())
            await self.redis_manager.expire(key, self.server_timeout)

            # Publish registration event
            await self.pubsub_manager.publish(
                "collaboration:servers",
                {
                    "server_id": self.server_id,
                    "action": "register",
                    "timestamp": datetime.utcnow().isoformat(),
                },
            )

            self._registered = True
            logger.info(f"✅ Server registered: {self.server_id} ({self.host}:{self.port})")

            # Start heartbeat and cleanup tasks
            self._heartbeat_task = asyncio.create_task(self._heartbeat_loop())
            self._cleanup_task = asyncio.create_task(self._cleanup_loop())

            return True

        except Exception as e:
            logger.error(f"❌ Failed to register server: {e}")
            return False

    async def deregister(self):
        """Deregister this server from the registry."""
        # Cancel tasks
        if self._heartbeat_task:
            self._heartbeat_task.cancel()
            try:
                await self._heartbeat_task
            except asyncio.CancelledError:
                pass

        if self._cleanup_task:
            self._cleanup_task.cancel()
            try:
                await self._cleanup_task
            except asyncio.CancelledError:
                pass

        if not self.redis_manager.is_healthy:
            return

        try:
            # Delete from Redis
            key = f"collaboration:server:{self.server_id}"
            await self.redis_manager.delete(key)

            # Publish deregistration event
            await self.pubsub_manager.publish(
                "collaboration:servers",
                {
                    "server_id": self.server_id,
                    "action": "deregister",
                    "timestamp": datetime.utcnow().isoformat(),
                },
            )

            self._registered = False
            logger.info(f"Server deregistered: {self.server_id}")

        except Exception as e:
            logger.error(f"Failed to deregister server: {e}")

    async def update_connection_count(self, count: int):
        """
        Update the connection count for this server.

        Args:
            count: Number of active connections
        """
        self._connection_count = count

    async def get_active_servers(self) -> List[ServerInfo]:
        """
        Get list of all active servers.

        Returns:
            List of ServerInfo objects
        """
        if not self.redis_manager.is_healthy:
            # In single-server mode, return only this server
            return [
                ServerInfo(
                    server_id=self.server_id,
                    host=self.host,
                    port=self.port,
                    connections=self._connection_count,
                    started_at=self._started_at,
                )
            ]

        try:
            # Scan for all server keys
            servers = []
            pattern = "collaboration:server:*"

            async for key in self.redis_manager.client.scan_iter(match=pattern):
                server_data = await self.redis_manager.hgetall(key)
                if server_data:
                    try:
                        server_info = ServerInfo.from_dict(server_data)
                        servers.append(server_info)
                    except Exception as e:
                        logger.error(f"Failed to parse server info from {key}: {e}")

            return servers

        except Exception as e:
            logger.error(f"Failed to get active servers: {e}")
            return []

    async def get_server(self, server_id: str) -> Optional[ServerInfo]:
        """
        Get info about a specific server.

        Args:
            server_id: Server ID to look up

        Returns:
            ServerInfo if found, None otherwise
        """
        if not self.redis_manager.is_healthy:
            return None

        try:
            key = f"collaboration:server:{server_id}"
            server_data = await self.redis_manager.hgetall(key)

            if server_data:
                return ServerInfo.from_dict(server_data)

            return None

        except Exception as e:
            logger.error(f"Failed to get server {server_id}: {e}")
            return None

    async def get_total_connections(self) -> int:
        """
        Get total number of connections across all servers.

        Returns:
            Total connection count
        """
        servers = await self.get_active_servers()
        return sum(server.connections for server in servers)

    async def _heartbeat_loop(self):
        """Background task to send heartbeat every interval."""
        logger.info(f"Starting heartbeat loop (interval: {self.heartbeat_interval}s)")

        try:
            while True:
                await asyncio.sleep(self.heartbeat_interval)

                if not self.redis_manager.is_healthy:
                    continue

                try:
                    # Update server info
                    server_info = ServerInfo(
                        server_id=self.server_id,
                        host=self.host,
                        port=self.port,
                        connections=self._connection_count,
                        started_at=self._started_at,
                        last_heartbeat=datetime.utcnow().isoformat(),
                    )

                    # Update Redis
                    key = f"collaboration:server:{self.server_id}"
                    await self.redis_manager.hset(key, server_info.to_dict())
                    await self.redis_manager.expire(key, self.server_timeout)

                    # Publish heartbeat event
                    await self.pubsub_manager.publish(
                        "collaboration:servers",
                        {
                            "server_id": self.server_id,
                            "action": "heartbeat",
                            "connections": self._connection_count,
                            "timestamp": datetime.utcnow().isoformat(),
                        },
                    )

                except Exception as e:
                    logger.error(f"Heartbeat failed: {e}")

        except asyncio.CancelledError:
            logger.info("Heartbeat loop cancelled")
        except Exception as e:
            logger.error(f"Fatal error in heartbeat loop: {e}")

    async def _cleanup_loop(self):
        """Background task to cleanup dead servers."""
        logger.info("Starting server cleanup loop")

        try:
            while True:
                await asyncio.sleep(self.server_timeout)

                if not self.redis_manager.is_healthy:
                    continue

                try:
                    # Get all servers
                    servers = await self.get_active_servers()

                    # Check for dead servers (last heartbeat > timeout)
                    now = time.time()
                    for server in servers:
                        if server.server_id == self.server_id:
                            continue  # Skip self

                        last_heartbeat = datetime.fromisoformat(server.last_heartbeat)
                        age = now - last_heartbeat.timestamp()

                        if age > self.server_timeout:
                            logger.warning(
                                f"⚠️ Server {server.server_id} appears dead "
                                f"(last heartbeat {age:.0f}s ago), cleaning up"
                            )

                            # Remove dead server
                            key = f"collaboration:server:{server.server_id}"
                            await self.redis_manager.delete(key)

                except Exception as e:
                    logger.error(f"Cleanup failed: {e}")

        except asyncio.CancelledError:
            logger.info("Cleanup loop cancelled")
        except Exception as e:
            logger.error(f"Fatal error in cleanup loop: {e}")

    @property
    def is_registered(self) -> bool:
        """Check if server is registered."""
        return self._registered
