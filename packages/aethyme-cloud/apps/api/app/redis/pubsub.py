"""
Redis Pub/Sub Manager

Handles pub/sub messaging between servers for distributed collaboration.
"""

import asyncio
import json
import logging
from typing import Any, Callable, Dict, Optional, Set

from app.redis.connection import RedisConnectionManager

logger = logging.getLogger(__name__)

# Type alias for message handler
MessageHandler = Callable[[str, Dict[str, Any]], None]


class RedisPubSubManager:
    """
    Manages Redis pub/sub subscriptions and message publishing.
    """

    def __init__(self, redis_manager: RedisConnectionManager):
        self.redis_manager = redis_manager
        self._pubsub = None
        self._subscriptions: Dict[str, Set[MessageHandler]] = {}
        self._listener_task: Optional[asyncio.Task] = None
        self._running = False

    async def start(self):
        """Start the pub/sub listener."""
        if not self.redis_manager.is_healthy:
            logger.warning("Cannot start pub/sub: Redis not healthy")
            return False

        if self._running:
            logger.warning("Pub/sub listener already running")
            return True

        try:
            # Create pub/sub instance
            self._pubsub = self.redis_manager.client.pubsub()
            self._running = True

            # Start listener task
            self._listener_task = asyncio.create_task(self._listen_loop())

            logger.info("✅ Redis pub/sub started")
            return True

        except Exception as e:
            logger.error(f"❌ Failed to start pub/sub: {e}")
            return False

    async def stop(self):
        """Stop the pub/sub listener."""
        self._running = False

        # Cancel listener task
        if self._listener_task:
            self._listener_task.cancel()
            try:
                await self._listener_task
            except asyncio.CancelledError:
                pass
            self._listener_task = None

        # Close pub/sub
        if self._pubsub:
            await self._pubsub.close()
            self._pubsub = None

        # Clear subscriptions
        self._subscriptions.clear()

        logger.info("Redis pub/sub stopped")

    async def subscribe(self, channel: str, handler: MessageHandler):
        """
        Subscribe to a channel with a message handler.

        Args:
            channel: Channel name to subscribe to
            handler: Callback function for messages
        """
        if not self._running:
            logger.warning(f"Cannot subscribe to {channel}: Pub/sub not running")
            return

        # Add handler to subscriptions
        if channel not in self._subscriptions:
            self._subscriptions[channel] = set()
            # Subscribe to channel in Redis
            if self._pubsub:
                await self._pubsub.subscribe(channel)
                logger.info(f"📡 Subscribed to channel: {channel}")

        self._subscriptions[channel].add(handler)

    async def unsubscribe(self, channel: str, handler: Optional[MessageHandler] = None):
        """
        Unsubscribe from a channel.

        Args:
            channel: Channel name to unsubscribe from
            handler: Specific handler to remove (if None, removes all handlers)
        """
        if channel not in self._subscriptions:
            return

        # Remove specific handler or all handlers
        if handler:
            self._subscriptions[channel].discard(handler)
        else:
            self._subscriptions[channel].clear()

        # Unsubscribe from Redis if no more handlers
        if not self._subscriptions[channel]:
            del self._subscriptions[channel]
            if self._pubsub:
                await self._pubsub.unsubscribe(channel)
                logger.info(f"📡 Unsubscribed from channel: {channel}")

    async def publish(self, channel: str, message: Dict[str, Any]) -> bool:
        """
        Publish a message to a channel.

        Args:
            channel: Channel name
            message: Message data (will be JSON serialized)

        Returns:
            True if successful, False otherwise
        """
        if not self.redis_manager.is_healthy:
            return False

        try:
            message_json = json.dumps(message)
            await self.redis_manager.client.publish(channel, message_json)
            return True
        except Exception as e:
            logger.error(f"Failed to publish to {channel}: {e}")
            return False

    async def _listen_loop(self):
        """Background task to listen for pub/sub messages."""
        logger.info("Starting pub/sub listener loop")

        try:
            while self._running and self._pubsub:
                try:
                    # Get message with timeout
                    message = await asyncio.wait_for(
                        self._pubsub.get_message(ignore_subscribe_messages=True, timeout=1.0),
                        timeout=2.0
                    )

                    if message and message["type"] == "message":
                        channel = message["channel"]
                        data = message["data"]

                        # Decode if bytes
                        if isinstance(data, bytes):
                            data = data.decode("utf-8")

                        # Parse JSON
                        try:
                            message_dict = json.loads(data)
                        except json.JSONDecodeError:
                            logger.error(f"Invalid JSON in message from {channel}: {data}")
                            continue

                        # Call handlers
                        if channel in self._subscriptions:
                            for handler in list(self._subscriptions[channel]):
                                try:
                                    # Run handler in background to avoid blocking
                                    asyncio.create_task(
                                        self._run_handler(handler, channel, message_dict)
                                    )
                                except Exception as e:
                                    logger.error(f"Error calling handler for {channel}: {e}")

                except asyncio.TimeoutError:
                    # Normal timeout, continue loop
                    continue
                except Exception as e:
                    if self._running:
                        logger.error(f"Error in pub/sub listener: {e}")
                    await asyncio.sleep(1)  # Brief pause before retrying

        except asyncio.CancelledError:
            logger.info("Pub/sub listener cancelled")
        except Exception as e:
            logger.error(f"Fatal error in pub/sub listener: {e}")
        finally:
            logger.info("Pub/sub listener loop ended")

    async def _run_handler(self, handler: MessageHandler, channel: str, message: Dict[str, Any]):
        """
        Run a message handler (async wrapper for sync handlers).

        Args:
            handler: Message handler function
            channel: Channel name
            message: Message data
        """
        try:
            if asyncio.iscoroutinefunction(handler):
                await handler(channel, message)
            else:
                handler(channel, message)
        except Exception as e:
            logger.error(f"Error in message handler for {channel}: {e}")

    def is_running(self) -> bool:
        """Check if pub/sub listener is running."""
        return self._running

    def get_subscribed_channels(self) -> Set[str]:
        """Get list of subscribed channels."""
        return set(self._subscriptions.keys())
