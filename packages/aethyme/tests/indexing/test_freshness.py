"""Tests for active indexing freshness behavior."""

from __future__ import annotations

import asyncio
from datetime import datetime, timedelta
from unittest.mock import Mock

from src.indexing.freshness import (
    FreshnessMonitor,
    FreshnessStatus,
    ReindexTrigger,
    format_staleness,
)


def test_format_staleness() -> None:
    """Format human-readable staleness windows."""
    assert format_staleness(None) == "never indexed"
    assert "minutes" in format_staleness(0.5)
    assert "hours" in format_staleness(5.0)
    assert "days" in format_staleness(48.0)


def test_freshness_metrics_never_indexed() -> None:
    """Repositories without an index are reported explicitly."""
    mock_pool = Mock()
    mock_pool.execute.return_value = [
        {"id": "repo-1", "name": "test-repo", "last_indexed_at": None}
    ]

    monitor = FreshnessMonitor(mock_pool)
    metrics = monitor.get_repository_freshness("repo-1", "tenant-1")

    assert metrics.status == FreshnessStatus.NEVER_INDEXED
    assert metrics.last_indexed_at is None
    assert metrics.staleness_hours is None


def test_freshness_metrics_fresh() -> None:
    """Recently indexed repositories stay fresh."""
    mock_pool = Mock()
    recent_time = datetime.now() - timedelta(hours=1)
    mock_pool.execute.return_value = [
        {"id": "repo-1", "name": "test-repo", "last_indexed_at": recent_time}
    ]

    monitor = FreshnessMonitor(mock_pool, warning_threshold_hours=24.0)
    metrics = monitor.get_repository_freshness("repo-1", "tenant-1")

    assert metrics.status == FreshnessStatus.FRESH
    assert metrics.staleness_hours is not None
    assert metrics.staleness_hours < 24.0


def test_freshness_metrics_stale() -> None:
    """Stale repositories are detected at the warning threshold."""
    mock_pool = Mock()
    old_time = datetime.now() - timedelta(hours=30)
    mock_pool.execute.return_value = [
        {"id": "repo-1", "name": "test-repo", "last_indexed_at": old_time}
    ]

    monitor = FreshnessMonitor(
        mock_pool,
        warning_threshold_hours=24.0,
        critical_threshold_hours=72.0,
    )
    metrics = monitor.get_repository_freshness("repo-1", "tenant-1")

    assert metrics.status == FreshnessStatus.STALE
    assert metrics.staleness_hours is not None
    assert metrics.staleness_hours > 24.0


def test_freshness_metrics_critical() -> None:
    """Critically stale repositories are detected at the critical threshold."""
    mock_pool = Mock()
    very_old_time = datetime.now() - timedelta(hours=100)
    mock_pool.execute.return_value = [
        {"id": "repo-1", "name": "test-repo", "last_indexed_at": very_old_time}
    ]

    monitor = FreshnessMonitor(
        mock_pool,
        warning_threshold_hours=24.0,
        critical_threshold_hours=72.0,
    )
    metrics = monitor.get_repository_freshness("repo-1", "tenant-1")

    assert metrics.status == FreshnessStatus.CRITICAL
    assert metrics.staleness_hours is not None
    assert metrics.staleness_hours > 72.0


def test_get_stale_repositories() -> None:
    """Repository scans return stale and never-indexed entries."""
    mock_pool = Mock()
    old_time = datetime.now() - timedelta(hours=30)
    mock_pool.execute.side_effect = [
        [
            {"id": "repo-1", "name": "repo-1", "last_indexed_at": old_time},
            {"id": "repo-2", "name": "repo-2", "last_indexed_at": None},
        ],
        [{"id": "repo-1", "name": "repo-1", "last_indexed_at": old_time}],
        [{"id": "repo-2", "name": "repo-2", "last_indexed_at": None}],
    ]

    monitor = FreshnessMonitor(mock_pool, warning_threshold_hours=24.0)
    stale_repos = monitor.get_stale_repositories("tenant-1", threshold_hours=24.0)

    assert len(stale_repos) == 2


def test_reindex_trigger_dry_run() -> None:
    """Dry-run reindexing reports stale repositories without invoking callbacks."""
    mock_pool = Mock()
    old_time = datetime.now() - timedelta(hours=30)
    mock_pool.execute.side_effect = [
        [{"id": "repo-1", "name": "repo-1", "last_indexed_at": old_time}],
        [{"id": "repo-1", "name": "repo-1", "last_indexed_at": old_time}],
    ]

    monitor = FreshnessMonitor(mock_pool, warning_threshold_hours=24.0)
    callback = Mock()
    trigger = ReindexTrigger(monitor, callback)

    reindexed = asyncio.run(trigger.reindex_stale_repos("tenant-1", dry_run=True))

    callback.assert_not_called()
    assert reindexed == []
