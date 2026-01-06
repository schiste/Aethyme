"""
Pytest configuration and shared fixtures for RepoGraph tests.

This module provides common test fixtures, utilities, and configuration
for the entire test suite.
"""

import asyncio
import os
from typing import AsyncGenerator, Generator
from unittest.mock import MagicMock

import pytest
from fastapi.testclient import TestClient
from sqlalchemy import create_engine, event
from sqlalchemy.ext.asyncio import AsyncSession, create_async_engine
from sqlalchemy.orm import Session, sessionmaker

# Fixtures will be imported here as modules are developed
# from src.database import Base, get_db
# from src.main import app


# ============================================================================
# Session Management
# ============================================================================

@pytest.fixture(scope="session")
def event_loop():
    """Create an instance of the default event loop for the test session."""
    loop = asyncio.get_event_loop_policy().new_event_loop()
    yield loop
    loop.close()


# ============================================================================
# Database Fixtures
# ============================================================================

@pytest.fixture(scope="session")
def database_url() -> str:
    """Get test database URL from environment or use default."""
    return os.getenv(
        "TEST_DATABASE_URL",
        "postgresql://repograph:dev_password_change_me@localhost:5432/repograph_test"
    )


@pytest.fixture(scope="session")
def sync_engine(database_url: str):
    """Create a synchronous SQLAlchemy engine for tests."""
    engine = create_engine(database_url, echo=False, pool_pre_ping=True)
    yield engine
    engine.dispose()


@pytest.fixture(scope="session")
def async_engine(database_url: str):
    """Create an async SQLAlchemy engine for tests."""
    # Convert postgresql:// to postgresql+asyncpg://
    async_url = database_url.replace("postgresql://", "postgresql+asyncpg://")
    engine = create_async_engine(async_url, echo=False, pool_pre_ping=True)
    yield engine
    asyncio.get_event_loop().run_until_complete(engine.dispose())


@pytest.fixture(scope="function")
async def db_session(async_engine) -> AsyncGenerator[AsyncSession, None]:
    """
    Create a new database session for a test with automatic rollback.

    This fixture provides transaction isolation - all database changes
    are rolled back after the test completes.
    """
    async with async_engine.connect() as connection:
        async with connection.begin() as transaction:
            session = AsyncSession(bind=connection, expire_on_commit=False)

            yield session

            await session.close()
            await transaction.rollback()


@pytest.fixture(scope="function")
def sync_db_session(sync_engine) -> Generator[Session, None, None]:
    """
    Create a synchronous database session for a test with automatic rollback.
    """
    connection = sync_engine.connect()
    transaction = connection.begin()
    session = Session(bind=connection)

    yield session

    session.close()
    transaction.rollback()
    connection.close()


@pytest.fixture(scope="function", autouse=True)
async def setup_database(async_engine):
    """
    Set up and tear down database schema for each test.

    This is autouse so it runs for every test automatically.
    """
    # Uncomment when Base is available:
    # async with async_engine.begin() as conn:
    #     await conn.run_sync(Base.metadata.create_all)

    yield

    # Uncomment when Base is available:
    # async with async_engine.begin() as conn:
    #     await conn.run_sync(Base.metadata.drop_all)


# ============================================================================
# Redis Fixtures
# ============================================================================

@pytest.fixture(scope="session")
def redis_url() -> str:
    """Get test Redis URL from environment or use default."""
    return os.getenv("TEST_REDIS_URL", "redis://localhost:6379/1")


@pytest.fixture(scope="function")
async def redis_client(redis_url: str):
    """
    Create a Redis client for testing with automatic cleanup.
    """
    # Uncomment when Redis client is available:
    # from src.cache import get_redis_client
    # client = await get_redis_client(redis_url)
    # yield client
    # await client.flushdb()
    # await client.close()

    # Mock for now
    yield MagicMock()


# ============================================================================
# API Client Fixtures
# ============================================================================

@pytest.fixture(scope="function")
def api_client(db_session, redis_client) -> TestClient:
    """
    Create a FastAPI test client with database and Redis dependencies overridden.
    """
    # Uncomment when app is available:
    # from src.main import app
    # from src.database import get_db
    # from src.cache import get_redis

    # async def override_get_db():
    #     yield db_session

    # async def override_get_redis():
    #     yield redis_client

    # app.dependency_overrides[get_db] = override_get_db
    # app.dependency_overrides[get_redis] = override_get_redis

    # client = TestClient(app)
    # yield client

    # app.dependency_overrides.clear()

    # Mock for now
    yield MagicMock()


@pytest.fixture(scope="function")
def authenticated_client(api_client) -> TestClient:
    """
    Create an authenticated test client with a valid JWT token.
    """
    # Uncomment when auth is implemented:
    # from src.auth import create_access_token
    # token = create_access_token({"sub": "test_user", "org": "test_org"})
    # api_client.headers["Authorization"] = f"Bearer {token}"

    yield api_client


# ============================================================================
# Mock Data Fixtures
# ============================================================================

@pytest.fixture
def mock_repo_data():
    """Sample repository data for testing."""
    return {
        "id": "test-repo-123",
        "name": "test-repo",
        "org": "test-org",
        "language": "python",
        "url": "https://github.com/test-org/test-repo",
    }


@pytest.fixture
def mock_index_data():
    """Sample index data for testing."""
    return {
        "repo_id": "test-repo-123",
        "symbols": [
            {
                "name": "test_function",
                "kind": "function",
                "file": "src/test.py",
                "line": 10,
            },
            {
                "name": "TestClass",
                "kind": "class",
                "file": "src/test.py",
                "line": 20,
            },
        ],
        "indexed_at": "2025-11-22T00:00:00Z",
    }


@pytest.fixture
def mock_user_data():
    """Sample user data for testing."""
    return {
        "id": "user-123",
        "email": "test@example.com",
        "org": "test-org",
        "roles": ["developer"],
    }


# ============================================================================
# Test Markers
# ============================================================================

def pytest_configure(config):
    """Register custom markers."""
    config.addinivalue_line(
        "markers", "integration: marks tests as integration tests (slower)"
    )
    config.addinivalue_line(
        "markers", "slow: marks tests as slow (deselect with '-m \"not slow\"')"
    )
    config.addinivalue_line(
        "markers", "auth: marks tests that require authentication"
    )
    config.addinivalue_line(
        "markers", "rls: marks tests for Row Level Security (RLS)"
    )
    config.addinivalue_line(
        "markers", "performance: marks performance/benchmark tests"
    )


# ============================================================================
# Test Utilities
# ============================================================================

@pytest.fixture
def assert_timing():
    """Fixture to assert code execution time."""
    import time

    class TimingAssertion:
        def __init__(self):
            self.start = None
            self.end = None

        def __enter__(self):
            self.start = time.time()
            return self

        def __exit__(self, *args):
            self.end = time.time()

        @property
        def elapsed(self):
            return self.end - self.start

        def assert_less_than(self, max_seconds: float):
            assert self.elapsed < max_seconds, \
                f"Expected execution in <{max_seconds}s, took {self.elapsed:.3f}s"

    return TimingAssertion


@pytest.fixture
def create_test_org(db_session):
    """Helper to create test organizations."""
    async def _create_org(name: str = "test-org", **kwargs):
        # Uncomment when Org model is available:
        # from src.models import Organization
        # org = Organization(name=name, **kwargs)
        # db_session.add(org)
        # await db_session.commit()
        # await db_session.refresh(org)
        # return org
        return MagicMock(name=name, **kwargs)

    return _create_org


@pytest.fixture
def create_test_repo(db_session):
    """Helper to create test repositories."""
    async def _create_repo(org_id: str, name: str = "test-repo", **kwargs):
        # Uncomment when Repo model is available:
        # from src.models import Repository
        # repo = Repository(org_id=org_id, name=name, **kwargs)
        # db_session.add(repo)
        # await db_session.commit()
        # await db_session.refresh(repo)
        # return repo
        return MagicMock(org_id=org_id, name=name, **kwargs)

    return _create_repo


# ============================================================================
# Cleanup
# ============================================================================

@pytest.fixture(scope="function", autouse=True)
def cleanup_test_data():
    """
    Ensure test data is cleaned up after each test.

    This is autouse so it runs automatically.
    """
    yield
    # Add any additional cleanup logic here
    pass
