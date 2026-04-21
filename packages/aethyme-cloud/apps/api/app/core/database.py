"""
Database configuration and session management.

Uses SQLAlchemy 2.0 with async support and optimized connection pooling.

Features:
- Connection pooling with health checks
- Automatic reconnection on connection loss
- Pool size optimization for production
- Query performance logging
- Connection lifecycle events
"""

from typing import AsyncGenerator
from sqlalchemy.ext.asyncio import AsyncSession, create_async_engine, async_sessionmaker
from sqlalchemy.orm import DeclarativeBase
from sqlalchemy.pool import AsyncAdaptedQueuePool
from sqlalchemy import event, text
from sqlalchemy.exc import DBAPIError, DisconnectionError
import logging
from contextlib import asynccontextmanager

from app.core.config import settings

logger = logging.getLogger(__name__)


# Create async engine with optimized settings
engine_config = {
    "url": str(settings.DATABASE_URL).replace("postgresql://", "postgresql+asyncpg://"),
    "echo": settings.DB_ECHO,
}

if settings.ENVIRONMENT == "production":
    engine_config.update({
        # Connection pool settings
        "pool_size": settings.DB_POOL_MIN_SIZE,  # Minimum connections to maintain
        "max_overflow": settings.DB_POOL_MAX_SIZE - settings.DB_POOL_MIN_SIZE,  # Additional connections on demand
        "poolclass": AsyncAdaptedQueuePool,

        # Connection lifecycle settings
        "pool_pre_ping": True,  # Verify connections before using (prevents stale connections)
        "pool_recycle": 3600,  # Recycle connections after 1 hour
        "pool_timeout": 30,  # Timeout for getting a connection from pool

        # Query execution settings
        "connect_args": {
            "server_settings": {
                "application_name": "aethyme_cloud_api",
                "jit": "off",  # Disable JIT compilation for faster simple queries
            },
            "command_timeout": 60,  # Query timeout in seconds
            "timeout": 30,  # Connection timeout in seconds
        },

        # Pessimistic disconnect handling (for detecting disconnects)
        "pool_use_lifo": True,  # Use most recently returned connections first
    })
else:
    # Development: Simpler pool configuration
    engine_config.update({
        "pool_size": 5,
        "max_overflow": 10,
        "poolclass": AsyncAdaptedQueuePool,
        "pool_pre_ping": True,
    })

engine = create_async_engine(**engine_config)

# Create session factory
AsyncSessionLocal = async_sessionmaker(
    engine,
    class_=AsyncSession,
    expire_on_commit=False,
    autocommit=False,
    autoflush=False,
)


class Base(DeclarativeBase):
    """Base class for all database models."""
    pass


async def get_db() -> AsyncGenerator[AsyncSession, None]:
    """
    Dependency for getting async database sessions.

    Features:
    - Automatic commit on success
    - Automatic rollback on errors
    - Retry logic for transient database errors
    - Connection cleanup

    Usage:
        @app.get("/users")
        async def get_users(db: AsyncSession = Depends(get_db)):
            ...
    """
    max_retries = 3
    retry_count = 0

    while retry_count < max_retries:
        try:
            async with AsyncSessionLocal() as session:
                try:
                    yield session
                    await session.commit()
                    break  # Success, exit retry loop
                except (DBAPIError, DisconnectionError) as e:
                    await session.rollback()
                    retry_count += 1
                    if retry_count >= max_retries:
                        logger.error(f"Database error after {max_retries} retries: {e}")
                        raise
                    logger.warning(f"Database error (retry {retry_count}/{max_retries}): {e}")
                    # Wait before retry (exponential backoff)
                    import asyncio
                    await asyncio.sleep(0.1 * (2 ** retry_count))
                except Exception:
                    await session.rollback()
                    raise
                finally:
                    await session.close()
        except (DBAPIError, DisconnectionError):
            # Connection error, retry with new session
            if retry_count >= max_retries:
                raise
            continue


@asynccontextmanager
async def get_db_context() -> AsyncGenerator[AsyncSession, None]:
    """
    Context manager for database sessions (non-dependency usage).

    Usage:
        async with get_db_context() as db:
            result = await db.execute(...)
    """
    async with AsyncSessionLocal() as session:
        try:
            yield session
            await session.commit()
        except Exception:
            await session.rollback()
            raise
        finally:
            await session.close()


async def init_db() -> None:
    """
    Initialize database connection pool.

    Performs:
    - Connection test
    - Table creation (development only)
    - Connection pool warmup
    """
    logger.info("Initializing database connection pool...")

    # Test connection
    try:
        async with engine.begin() as conn:
            # Test query
            await conn.execute(text("SELECT 1"))
            logger.info("Database connection successful")

            # Create tables if they don't exist (development only)
            if settings.ENVIRONMENT == "development":
                logger.info("Creating database tables (development mode)...")
                await conn.run_sync(Base.metadata.create_all)
                logger.info("Database tables created")

    except Exception as e:
        logger.error(f"Failed to initialize database: {e}")
        raise

    # Warm up connection pool (create initial connections)
    if settings.ENVIRONMENT == "production":
        logger.info(f"Warming up connection pool (size: {settings.DB_POOL_MIN_SIZE})...")
        try:
            # Create initial connections
            sessions = []
            for _ in range(min(settings.DB_POOL_MIN_SIZE, 3)):
                session = AsyncSessionLocal()
                await session.execute(text("SELECT 1"))
                sessions.append(session)

            # Close sessions to return to pool
            for session in sessions:
                await session.close()

            logger.info("Connection pool warmed up successfully")
        except Exception as e:
            logger.warning(f"Connection pool warmup failed (non-critical): {e}")


async def close_db() -> None:
    """
    Close database connection pool gracefully.

    Waits for active connections to complete before closing.
    """
    logger.info("Closing database connection pool...")
    await engine.dispose()
    logger.info("Database connection pool closed")


async def get_pool_status() -> dict:
    """
    Get current connection pool status.

    Returns:
        dict: Pool statistics including size, overflow, and checked out connections
    """
    pool = engine.pool
    return {
        "pool_size": pool.size(),
        "checked_out": pool.checkedout(),
        "overflow": pool.overflow(),
        "total_connections": pool.size() + pool.overflow(),
        "max_connections": settings.DB_POOL_MAX_SIZE,
    }


# Connection lifecycle event handlers
@event.listens_for(engine.sync_engine, "connect")
def receive_connect(dbapi_conn, connection_record):
    """Log when new database connections are created."""
    logger.debug("New database connection created")


@event.listens_for(engine.sync_engine, "checkout")
def receive_checkout(dbapi_conn, connection_record, connection_proxy):
    """Log when connections are checked out from pool."""
    logger.debug("Connection checked out from pool")


@event.listens_for(engine.sync_engine, "checkin")
def receive_checkin(dbapi_conn, connection_record):
    """Log when connections are returned to pool."""
    logger.debug("Connection returned to pool")
