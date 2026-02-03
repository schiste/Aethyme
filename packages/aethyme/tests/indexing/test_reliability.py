"""
Tests for indexing reliability features.

Tests validation, fallback, retry logic, and freshness detection.
"""

import pytest
import time
from pathlib import Path
from datetime import datetime, timedelta
from unittest.mock import Mock, patch, MagicMock

import sys
sys.path.insert(0, str(Path(__file__).parent.parent.parent))

from src.indexing.validator import (
    IndexerValidator,
    IndexerType,
    IndexerResult,
)
from src.indexing.retry import (
    RetryManager,
    RetryConfig,
    CircuitBreaker,
    CircuitBreakerConfig,
    CircuitState,
    RetryableError,
    NonRetryableError,
    CircuitOpenError,
)
from src.indexing.freshness import (
    FreshnessMonitor,
    FreshnessStatus,
    ReindexTrigger,
    format_staleness,
)


class TestIndexerValidator:
    """Tests for IndexerValidator."""

    def test_validator_initialization(self):
        """Test validator can be initialized."""
        validator = IndexerValidator()
        assert validator is not None
        assert len(validator.metrics) == 0

    def test_scip_success(self, tmp_path):
        """Test successful SCIP indexing."""
        validator = IndexerValidator()

        # Mock SCIP indexer
        with patch("src.indexing.validator.SCIPIndexer") as mock_scip:
            mock_instance = mock_scip.return_value
            mock_instance.is_available.return_value = True
            mock_instance.index.return_value = {
                "symbols": [{"file_path": "test.py"} for _ in range(100)],
                "relationships": [{"type": "calls"} for _ in range(50)],
            }

            metrics = validator.validate_repository(
                repo_path=tmp_path,
                repo_name="test-repo",
                language="python",
                try_scip=True,
            )

            assert metrics.indexer_type == IndexerType.SCIP
            assert metrics.result == IndexerResult.SUCCESS
            assert metrics.symbol_count == 100
            assert metrics.edge_count == 50

    def test_fallback_when_scip_unavailable(self, tmp_path):
        """Test fallback is used when SCIP is unavailable."""
        validator = IndexerValidator()

        with patch("src.indexing.validator.SCIPIndexer") as mock_scip, \
             patch("src.indexing.validator.FallbackIndexer") as mock_fallback:

            # SCIP not available
            mock_scip_instance = mock_scip.return_value
            mock_scip_instance.is_available.return_value = False

            # Fallback succeeds
            mock_fallback_instance = mock_fallback.return_value
            nodes = [{"kind": "function"} for _ in range(50)]
            edges = [{"type": "calls"} for _ in range(25)]
            mock_fallback_instance.index.return_value = (nodes, edges)

            metrics = validator.validate_repository(
                repo_path=tmp_path,
                repo_name="test-repo",
                language="python",
                try_scip=True,
            )

            assert metrics.indexer_type == IndexerType.FALLBACK
            assert metrics.result == IndexerResult.FALLBACK_USED
            assert metrics.symbol_count == 50

    def test_force_fallback(self, tmp_path):
        """Test forcing fallback indexer."""
        validator = IndexerValidator()

        with patch("src.indexing.validator.FallbackIndexer") as mock_fallback:
            mock_fallback_instance = mock_fallback.return_value
            nodes = [{"kind": "function"} for _ in range(30)]
            edges = []
            mock_fallback_instance.index.return_value = (nodes, edges)

            metrics = validator.validate_repository(
                repo_path=tmp_path,
                repo_name="test-repo",
                language="python",
                try_scip=False,
                force_fallback=True,
            )

            assert metrics.indexer_type == IndexerType.FALLBACK
            assert metrics.result == IndexerResult.SUCCESS

    def test_success_rate_calculation(self, tmp_path):
        """Test success rate calculation."""
        validator = IndexerValidator()

        # Add mock metrics
        from src.indexing.validator import ValidationMetrics

        validator.metrics = [
            ValidationMetrics(
                repo_name="repo1",
                repo_path=str(tmp_path),
                language="python",
                indexer_type=IndexerType.SCIP,
                result=IndexerResult.SUCCESS,
                duration_seconds=10.0,
                symbol_count=100,
                node_count=100,
                edge_count=50,
                file_count=10,
            ),
            ValidationMetrics(
                repo_name="repo2",
                repo_path=str(tmp_path),
                language="python",
                indexer_type=IndexerType.FALLBACK,
                result=IndexerResult.FALLBACK_USED,
                duration_seconds=15.0,
                symbol_count=80,
                node_count=80,
                edge_count=40,
                file_count=8,
            ),
        ]

        rates = validator.get_success_rate()
        assert rates["scip"] == 0.5  # 1 of 2
        assert rates["fallback"] == 1.0  # 2 of 2
        assert rates["overall"] == 1.0

    def test_generate_report(self, tmp_path):
        """Test report generation."""
        validator = IndexerValidator()

        from src.indexing.validator import ValidationMetrics

        validator.metrics = [
            ValidationMetrics(
                repo_name="test-repo",
                repo_path=str(tmp_path),
                language="python",
                indexer_type=IndexerType.SCIP,
                result=IndexerResult.SUCCESS,
                duration_seconds=25.0,
                symbol_count=500,
                node_count=500,
                edge_count=250,
                file_count=50,
            ),
        ]

        report = validator.generate_report()
        assert "Indexer Validation Report" in report
        assert "test-repo" in report
        assert "python" in report


class TestRetryLogic:
    """Tests for retry logic and circuit breaker."""

    def test_retry_success_on_first_attempt(self):
        """Test successful operation on first attempt."""
        manager = RetryManager(RetryConfig(max_attempts=3))

        call_count = 0

        def operation():
            nonlocal call_count
            call_count += 1
            return "success"

        result = manager.execute_with_retry(operation, operation_name="test")
        assert result == "success"
        assert call_count == 1

    def test_retry_success_after_failures(self):
        """Test retry succeeds after transient failures."""
        manager = RetryManager(RetryConfig(max_attempts=3, initial_delay_seconds=0.1))

        call_count = 0

        def operation():
            nonlocal call_count
            call_count += 1
            if call_count < 3:
                raise RetryableError("Transient failure")
            return "success"

        result = manager.execute_with_retry(operation, operation_name="test")
        assert result == "success"
        assert call_count == 3

    def test_retry_exhausted(self):
        """Test all retries exhausted."""
        manager = RetryManager(RetryConfig(max_attempts=3, initial_delay_seconds=0.1))

        def operation():
            raise RetryableError("Always fails")

        with pytest.raises(RetryableError):
            manager.execute_with_retry(operation, operation_name="test")

    def test_non_retryable_error(self):
        """Test non-retryable errors fail immediately."""
        manager = RetryManager(RetryConfig(max_attempts=3))

        call_count = 0

        def operation():
            nonlocal call_count
            call_count += 1
            raise NonRetryableError("Fatal error")

        with pytest.raises(NonRetryableError):
            manager.execute_with_retry(operation, operation_name="test")

        assert call_count == 1

    def test_circuit_breaker_opens_on_failures(self):
        """Test circuit breaker opens after threshold failures."""
        config = CircuitBreakerConfig(
            failure_threshold=3,
            success_threshold=2,
            timeout_seconds=1.0,
        )
        circuit = CircuitBreaker(config, "test")

        def failing_operation():
            raise Exception("Failure")

        # Cause failures to open circuit
        for _ in range(3):
            with pytest.raises(Exception):
                circuit.call(failing_operation)

        assert circuit.state == CircuitState.OPEN

        # Next call should fail immediately
        with pytest.raises(CircuitOpenError):
            circuit.call(failing_operation)

    def test_circuit_breaker_half_open_recovery(self):
        """Test circuit breaker recovers via half-open state."""
        config = CircuitBreakerConfig(
            failure_threshold=2,
            success_threshold=2,
            timeout_seconds=0.5,
        )
        circuit = CircuitBreaker(config, "test")

        def failing_operation():
            raise Exception("Failure")

        def success_operation():
            return "success"

        # Open circuit
        for _ in range(2):
            with pytest.raises(Exception):
                circuit.call(failing_operation)

        assert circuit.state == CircuitState.OPEN

        # Wait for timeout
        time.sleep(0.6)

        # Should enter half-open and allow attempts
        result = circuit.call(success_operation)
        assert result == "success"
        assert circuit.state == CircuitState.HALF_OPEN

        # Second success should close circuit
        result = circuit.call(success_operation)
        assert result == "success"
        assert circuit.state == CircuitState.CLOSED

    def test_circuit_breaker_manual_reset(self):
        """Test manual circuit breaker reset."""
        config = CircuitBreakerConfig(failure_threshold=1)
        circuit = CircuitBreaker(config, "test")

        def failing_operation():
            raise Exception("Failure")

        # Open circuit
        with pytest.raises(Exception):
            circuit.call(failing_operation)

        assert circuit.state == CircuitState.OPEN

        # Manual reset
        circuit.reset()
        assert circuit.state == CircuitState.CLOSED
        assert circuit.failure_count == 0


class TestFreshnessMonitoring:
    """Tests for freshness monitoring."""

    def test_format_staleness(self):
        """Test staleness formatting."""
        assert format_staleness(None) == "never indexed"
        assert "minutes" in format_staleness(0.5)
        assert "hours" in format_staleness(5.0)
        assert "days" in format_staleness(48.0)

    def test_freshness_metrics_never_indexed(self):
        """Test metrics for never-indexed repository."""
        mock_pool = Mock()
        mock_pool.execute.return_value = [
            {"id": "repo-1", "name": "test-repo", "last_indexed_at": None}
        ]

        monitor = FreshnessMonitor(mock_pool)
        metrics = monitor.get_repository_freshness("repo-1", "tenant-1")

        assert metrics.status == FreshnessStatus.NEVER_INDEXED
        assert metrics.last_indexed_at is None
        assert metrics.staleness_hours is None

    def test_freshness_metrics_fresh(self):
        """Test metrics for fresh repository."""
        mock_pool = Mock()
        recent_time = datetime.now() - timedelta(hours=1)
        mock_pool.execute.return_value = [
            {"id": "repo-1", "name": "test-repo", "last_indexed_at": recent_time}
        ]

        monitor = FreshnessMonitor(mock_pool, warning_threshold_hours=24.0)
        metrics = monitor.get_repository_freshness("repo-1", "tenant-1")

        assert metrics.status == FreshnessStatus.FRESH
        assert metrics.staleness_hours < 24.0

    def test_freshness_metrics_stale(self):
        """Test metrics for stale repository."""
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
        assert metrics.staleness_hours > 24.0

    def test_freshness_metrics_critical(self):
        """Test metrics for critically stale repository."""
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
        assert metrics.staleness_hours > 72.0

    def test_get_stale_repositories(self):
        """Test getting list of stale repositories."""
        mock_pool = Mock()

        # Mock response for stale repos query
        old_time = datetime.now() - timedelta(hours=30)
        mock_pool.execute.side_effect = [
            # First call: get stale repos
            [
                {"id": "repo-1", "name": "repo-1", "last_indexed_at": old_time},
                {"id": "repo-2", "name": "repo-2", "last_indexed_at": None},
            ],
            # Subsequent calls: get individual repo metrics
            [{"id": "repo-1", "name": "repo-1", "last_indexed_at": old_time}],
            [{"id": "repo-2", "name": "repo-2", "last_indexed_at": None}],
        ]

        monitor = FreshnessMonitor(mock_pool, warning_threshold_hours=24.0)
        stale_repos = monitor.get_stale_repositories("tenant-1", threshold_hours=24.0)

        assert len(stale_repos) == 2

    def test_reindex_trigger_dry_run(self):
        """Test reindex trigger in dry-run mode."""
        mock_pool = Mock()
        old_time = datetime.now() - timedelta(hours=30)
        mock_pool.execute.side_effect = [
            [{"id": "repo-1", "name": "repo-1", "last_indexed_at": old_time}],
            [{"id": "repo-1", "name": "repo-1", "last_indexed_at": old_time}],
        ]

        monitor = FreshnessMonitor(mock_pool, warning_threshold_hours=24.0)
        callback = Mock()
        trigger = ReindexTrigger(monitor, callback)

        # Run in dry-run mode
        import asyncio
        reindexed = asyncio.run(
            trigger.reindex_stale_repos("tenant-1", dry_run=True)
        )

        # Callback should not be called in dry-run
        callback.assert_not_called()
        assert len(reindexed) == 0


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
