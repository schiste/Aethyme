"""Pytest configuration and shared fixtures."""

import asyncio
import os
from typing import AsyncGenerator, Generator
from datetime import datetime, timedelta

import pytest
from fastapi.testclient import TestClient
from httpx import AsyncClient
from sqlalchemy import create_engine, event
from sqlalchemy.ext.asyncio import AsyncSession, create_async_engine, async_sessionmaker
from sqlalchemy.pool import StaticPool

# Import Base from where models actually use it
from app.models.base import Base
from app.core.database import get_db

# Import ALL models FIRST before anything else to register them with Base.metadata
from app.models import User, Organization, Repository, APIKey
from app.models.github_account import GitHubAccount

from app.main import app
from app.core.security import build_user_access_claims, create_access_token, get_password_hash


# Test database URL (use PostgreSQL test database)
# Database created via: docker exec aethyme-postgres psql -U aethyme -c "CREATE DATABASE aethyme_test;"
TEST_DATABASE_URL = "postgresql+asyncpg://aethyme:aethyme_dev_password@localhost:5433/aethyme_test"


@pytest.fixture(scope="session")
def event_loop() -> Generator:
    """Create event loop for async tests."""
    loop = asyncio.get_event_loop_policy().new_event_loop()
    yield loop
    loop.close()


@pytest.fixture(scope="session", autouse=True)
def setup_test_database():
    """Set up test database once for entire test session."""
    from sqlalchemy import create_engine, text
    import time

    print(f"\n[SETUP] Base has {len(Base.metadata.tables)} tables to create")

    print("[SETUP] Resetting test database...")
    # First, reset the database schema
    import subprocess
    subprocess.run([
        'docker', 'exec', 'aethyme-postgres',
        'psql', '-U', 'aethyme', '-d', 'aethyme_test', '-c',
        'DROP SCHEMA IF EXISTS public CASCADE; CREATE SCHEMA public;'
    ], capture_output=True, text=True)

    print("[SETUP] Creating tables with sync engine...")
    # Create tables using synchronous engine with begin() for auto-commit
    engine = create_engine("postgresql+psycopg2://aethyme:aethyme_dev_password@localhost:5433/aethyme_test")

    with engine.begin() as conn:
        Base.metadata.create_all(bind=conn)

    print("[SETUP] Tables created and committed!")

    # Verify tables exist with a fresh connection
    with engine.connect() as conn:
        result = conn.execute(text("SELECT tablename FROM pg_tables WHERE schemaname='public'"))
        tables = [row[0] for row in result]
        print(f"[SETUP] Verified {len(tables)} tables: {tables}")

    engine.dispose()

    # Small delay to ensure all connections see the schema
    time.sleep(0.2)

    yield

    print("[TEARDOWN] Test session complete!")


@pytest.fixture(scope="session")
def test_db_engine():
    """Create test database engine (session-scoped)."""
    engine = create_async_engine(
        TEST_DATABASE_URL,
        echo=False,
        pool_pre_ping=True,
    )
    return engine


@pytest.fixture(scope="function")
async def db_session(test_db_engine) -> AsyncGenerator[AsyncSession, None]:
    """Create test database session with cleanup."""
    async_session = async_sessionmaker(
        test_db_engine,
        class_=AsyncSession,
        expire_on_commit=False,
    )

    async with async_session() as session:
        yield session

    # Clean up test data after each test
    async with async_session() as cleanup_session:
        from sqlalchemy import text
        # Delete data in reverse order of dependencies
        await cleanup_session.execute(text("DELETE FROM github_accounts"))
        await cleanup_session.execute(text("DELETE FROM api_keys"))
        await cleanup_session.execute(text("DELETE FROM repositories"))
        await cleanup_session.execute(text("DELETE FROM users"))
        await cleanup_session.execute(text("DELETE FROM organizations"))
        await cleanup_session.commit()


@pytest.fixture(scope="function")
def override_get_db(db_session: AsyncSession):
    """Override database dependency."""
    async def _override_get_db():
        yield db_session

    app.dependency_overrides[get_db] = _override_get_db
    yield
    app.dependency_overrides.clear()


@pytest.fixture(scope="function")
def client(override_get_db) -> TestClient:
    """Create test client."""
    return TestClient(app)


@pytest.fixture(scope="function")
async def async_client(override_get_db) -> AsyncGenerator[AsyncClient, None]:
    """Create async test client."""
    async with AsyncClient(app=app, base_url="http://test") as ac:
        yield ac


@pytest.fixture
async def test_organization(db_session: AsyncSession) -> Organization:
    """Create test organization."""
    org = Organization(
        id="test-org-123",
        name="Test Organization",
        slug="test-org",
        plan="pro",
        is_active=True,
        max_repositories=100,
        max_queries_per_month=10000,
        current_queries_this_month=0,
    )
    db_session.add(org)
    await db_session.commit()
    await db_session.refresh(org)
    return org


@pytest.fixture
async def test_user(db_session: AsyncSession, test_organization: Organization) -> User:
    """Create test user."""
    user = User(
        id="test-user-123",
        email="test@example.com",
        hashed_password=get_password_hash("testpassword123"),
        full_name="Test User",
        is_active=True,
        is_verified=True,
        organization_id=test_organization.id,
    )
    db_session.add(user)
    await db_session.commit()
    await db_session.refresh(user)
    return user


@pytest.fixture
async def test_user_token(test_user: User) -> str:
    """Create test user access token."""
    return create_access_token(
        data=build_user_access_claims(test_user)
    )


@pytest.fixture
async def authenticated_client(client: TestClient, test_user_token: str) -> TestClient:
    """Create authenticated test client."""
    client.headers = {"Authorization": f"Bearer {test_user_token}"}
    return client


@pytest.fixture
async def authenticated_async_client(
    async_client: AsyncClient, test_user_token: str
) -> AsyncClient:
    """Create authenticated async test client."""
    async_client.headers = {"Authorization": f"Bearer {test_user_token}"}
    return async_client


@pytest.fixture
async def test_repository(
    db_session: AsyncSession, test_organization: Organization
) -> Repository:
    """Create test repository."""
    repo = Repository(
        id="test-repo-123",
        organization_id=test_organization.id,
        name="test-repo",
        full_name="testorg/test-repo",
        url="https://github.com/testorg/test-repo",
        description="Test repository",
        provider="github",
        provider_id="12345",
        default_branch="main",
        is_private=False,
        indexing_status="completed",
        is_indexed=True,
        file_count=100,
        line_count=5000,
        total_files=100,
        total_lines=5000,
        languages={"Python": 0.7, "JavaScript": 0.3},
        primary_language="Python",
        indexed_at=datetime.utcnow(),
    )
    db_session.add(repo)
    await db_session.commit()
    await db_session.refresh(repo)
    return repo


@pytest.fixture
async def multiple_test_repositories(
    db_session: AsyncSession, test_organization: Organization
) -> list[Repository]:
    """Create multiple test repositories."""
    repos = []
    for i in range(5):
        repo = Repository(
            id=f"test-repo-{i}",
            organization_id=test_organization.id,
            name=f"test-repo-{i}",
            full_name=f"testorg/test-repo-{i}",
            url=f"https://github.com/testorg/test-repo-{i}",
            description=f"Test repository {i}",
            provider="github",
            provider_id=f"1234{i}",
            default_branch="main",
            is_private=i % 2 == 0,
            indexing_status="completed" if i < 3 else "pending",
            is_indexed=i < 3,
            file_count=100 * (i + 1),
            line_count=5000 * (i + 1),
            total_files=100 * (i + 1),
            total_lines=5000 * (i + 1),
            languages={"Python": 0.5 + (i * 0.1), "JavaScript": 0.5 - (i * 0.1)},
            primary_language="Python",
            indexed_at=datetime.utcnow() if i < 3 else None,
        )
        repos.append(repo)
        db_session.add(repo)

    await db_session.commit()
    for repo in repos:
        await db_session.refresh(repo)

    return repos
