"""Shared pytest configuration.

The PostgreSQL fixtures (seeded database, RLS helpers) were removed on
2026-07-13 with the Gen-0 lineage — all remaining suites run against the
local filesystem and the Rust engine only.
"""

from __future__ import annotations

import asyncio
from collections.abc import Generator

import pytest
from _pytest.config import Config


@pytest.fixture(scope="session")
def event_loop() -> Generator[asyncio.AbstractEventLoop, None, None]:
    """Provide a session-scoped event loop for async tests."""
    loop = asyncio.new_event_loop()
    yield loop
    loop.close()


def pytest_configure(config: Config) -> None:
    """Register custom markers."""
    config.addinivalue_line(
        "markers", "integration: marks tests as integration tests (slower)"
    )
    config.addinivalue_line(
        "markers", "slow: marks tests as slow (deselect with '-m \"not slow\"')"
    )
