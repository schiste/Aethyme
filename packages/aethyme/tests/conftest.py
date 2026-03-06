"""Shared test environment for Aethyme."""

from __future__ import annotations

import asyncio
import os
from collections.abc import Generator
from typing import TYPE_CHECKING, Any, cast

import psycopg2
import pytest
from _pytest.config import Config
from _pytest.nodes import Item
from psycopg2.extensions import connection as PsycopgConnection

if TYPE_CHECKING:
    from tests.support import GraphSeedContext

DEFAULT_TEST_DATABASE_URL = "postgresql://aethyme:dev_password_change_me@localhost:5432/aethyme_test"
INTEGRATION_TEST_DIRS = {"api", "auth", "queries"}
os.environ["DATABASE_URL"] = os.getenv("TEST_DATABASE_URL", DEFAULT_TEST_DATABASE_URL)


@pytest.fixture(scope="session")
def event_loop() -> Generator[asyncio.AbstractEventLoop, None, None]:
    """Create a session event loop for async tests."""
    loop = asyncio.new_event_loop()
    yield loop
    loop.close()


@pytest.fixture(scope="session")
def database_url() -> str:
    """Return the database URL shared by tests and the runtime connection pool."""
    return os.environ["DATABASE_URL"]


@pytest.fixture(scope="session")
def db_connection(database_url: str) -> Generator[PsycopgConnection, None, None]:
    """Provide a session-scoped database connection for schema rebuilds and seed data."""
    connection = psycopg2.connect(database_url)
    yield connection
    connection.close()


@pytest.fixture(scope="session")
def seeded_database(db_connection: PsycopgConnection) -> GraphSeedContext:
    """Rebuild the schema and load the canonical graph seed dataset once per test session."""
    from tests.support import rebuild_test_database, seed_graph_dataset

    rebuild_test_database(db_connection)
    return seed_graph_dataset(db_connection)


@pytest.fixture(scope="session")
def graph_seed(seeded_database: GraphSeedContext) -> GraphSeedContext:
    """Expose seeded IDs to tests that need them."""
    return seeded_database


def pytest_configure(config: Config) -> None:
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

def pytest_collection_modifyitems(config: Config, items: list[Item]) -> None:
    """Mark integration tests by directory."""
    del config
    for item in items:
        path_parts = set(item.path.parts)
        if "tests" not in path_parts:
            continue
        if path_parts & INTEGRATION_TEST_DIRS:
            item.add_marker(pytest.mark.integration)


@pytest.fixture(autouse=True)
def ensure_integration_database(
    request: pytest.FixtureRequest,
) -> GraphSeedContext | None:
    """Initialize the seeded database only for integration tests."""
    node = cast(Item, cast(Any, request).node)
    if node.get_closest_marker("integration") is None:
        return None
    return request.getfixturevalue("seeded_database")
