"""
Prometheus Metrics Collection for Indexing

Provides metrics collection and emission for indexing operations.
Tracks duration, failures, symbol counts, and staleness.
"""

import time
from contextlib import contextmanager

import structlog
from prometheus_client import (
    CollectorRegistry,
    Counter,
    Gauge,
    Histogram,
    generate_latest,
)

logger = structlog.get_logger(__name__)


# Create a custom registry (or use default)
registry = CollectorRegistry()


# Metrics definitions
index_duration_seconds = Histogram(
    "aethyme_index_duration_seconds",
    "Time spent indexing repositories",
    labelnames=["repository", "language", "indexer_type", "status"],
    buckets=[10, 30, 60, 120, 300, 600, 1200, 1800, 3600],  # 10s to 1h
    registry=registry,
)

index_failures_total = Counter(
    "aethyme_index_failures_total",
    "Total number of indexing failures",
    labelnames=["repository", "language", "indexer_type", "error_type"],
    registry=registry,
)

index_symbols_total = Gauge(
    "aethyme_index_symbols_total",
    "Total number of symbols indexed",
    labelnames=["repository", "language"],
    registry=registry,
)

index_files_total = Gauge(
    "aethyme_index_files_total",
    "Total number of files indexed",
    labelnames=["repository", "language"],
    registry=registry,
)

index_nodes_total = Gauge(
    "aethyme_index_nodes_total",
    "Total number of graph nodes created",
    labelnames=["repository"],
    registry=registry,
)

index_edges_total = Gauge(
    "aethyme_index_edges_total",
    "Total number of graph edges created",
    labelnames=["repository"],
    registry=registry,
)

index_staleness_seconds = Gauge(
    "aethyme_index_staleness_seconds",
    "Time since last successful index",
    labelnames=["repository", "tenant"],
    registry=registry,
)

index_operations_total = Counter(
    "aethyme_index_operations_total",
    "Total number of index operations",
    labelnames=["repository", "operation_type", "status"],
    registry=registry,
)

indexer_fallback_total = Counter(
    "aethyme_indexer_fallback_total",
    "Number of times fallback indexer was used",
    labelnames=["repository", "language", "reason"],
    registry=registry,
)

index_retry_attempts_total = Counter(
    "aethyme_index_retry_attempts_total",
    "Total number of retry attempts during indexing",
    labelnames=["repository", "language", "attempt_number"],
    registry=registry,
)

circuit_breaker_state = Gauge(
    "aethyme_circuit_breaker_state",
    "Circuit breaker state (0=closed, 1=half-open, 2=open)",
    labelnames=["circuit_name"],
    registry=registry,
)

circuit_breaker_failures_total = Counter(
    "aethyme_circuit_breaker_failures_total",
    "Total circuit breaker failures",
    labelnames=["circuit_name"],
    registry=registry,
)


class IndexingMetricsCollector:
    """
    Collects and emits metrics for indexing operations.

    Provides context managers and helper methods for tracking
    indexing performance and reliability.
    """

    def __init__(self):
        self.logger = logger.bind(component="IndexingMetricsCollector")

    @contextmanager
    def track_indexing_duration(
        self,
        repository: str,
        language: str,
        indexer_type: str,
    ):
        """
        Context manager to track indexing duration.

        Args:
            repository: Repository name
            language: Programming language
            indexer_type: Type of indexer (scip, fallback)

        Example:
            with collector.track_indexing_duration("myrepo", "python", "scip"):
                # indexing code
                pass
        """
        start_time = time.time()
        status = "success"

        try:
            yield
        except Exception:
            status = "failure"
            raise
        finally:
            duration = time.time() - start_time
            index_duration_seconds.labels(
                repository=repository,
                language=language,
                indexer_type=indexer_type,
                status=status,
            ).observe(duration)

            self.logger.info(
                "Indexing duration tracked",
                repository=repository,
                language=language,
                indexer_type=indexer_type,
                duration_seconds=duration,
                status=status,
            )

    def record_indexing_failure(
        self,
        repository: str,
        language: str,
        indexer_type: str,
        error_type: str,
    ):
        """
        Record an indexing failure.

        Args:
            repository: Repository name
            language: Programming language
            indexer_type: Type of indexer
            error_type: Type of error
        """
        index_failures_total.labels(
            repository=repository,
            language=language,
            indexer_type=indexer_type,
            error_type=error_type,
        ).inc()

        self.logger.warning(
            "Indexing failure recorded",
            repository=repository,
            language=language,
            indexer_type=indexer_type,
            error_type=error_type,
        )

    def record_symbol_count(
        self,
        repository: str,
        language: str,
        count: int,
    ):
        """
        Record symbol count for a repository/language.

        Args:
            repository: Repository name
            language: Programming language
            count: Number of symbols
        """
        index_symbols_total.labels(
            repository=repository,
            language=language,
        ).set(count)

    def record_file_count(
        self,
        repository: str,
        language: str,
        count: int,
    ):
        """
        Record file count for a repository/language.

        Args:
            repository: Repository name
            language: Programming language
            count: Number of files
        """
        index_files_total.labels(
            repository=repository,
            language=language,
        ).set(count)

    def record_graph_stats(
        self,
        repository: str,
        node_count: int,
        edge_count: int,
    ):
        """
        Record graph statistics.

        Args:
            repository: Repository name
            node_count: Number of nodes
            edge_count: Number of edges
        """
        index_nodes_total.labels(repository=repository).set(node_count)
        index_edges_total.labels(repository=repository).set(edge_count)

    def record_staleness(
        self,
        repository: str,
        tenant: str,
        staleness_seconds: float,
    ):
        """
        Record index staleness.

        Args:
            repository: Repository name
            tenant: Tenant ID
            staleness_seconds: Seconds since last index
        """
        index_staleness_seconds.labels(
            repository=repository,
            tenant=tenant,
        ).set(staleness_seconds)

    def record_operation(
        self,
        repository: str,
        operation_type: str,
        status: str,
    ):
        """
        Record an indexing operation.

        Args:
            repository: Repository name
            operation_type: Type of operation (index, reindex, validate)
            status: Status (success, failure)
        """
        index_operations_total.labels(
            repository=repository,
            operation_type=operation_type,
            status=status,
        ).inc()

    def record_fallback_usage(
        self,
        repository: str,
        language: str,
        reason: str,
    ):
        """
        Record when fallback indexer is used.

        Args:
            repository: Repository name
            language: Programming language
            reason: Reason for fallback (scip_unavailable, scip_failed, forced)
        """
        indexer_fallback_total.labels(
            repository=repository,
            language=language,
            reason=reason,
        ).inc()

        self.logger.info(
            "Fallback indexer used",
            repository=repository,
            language=language,
            reason=reason,
        )

    def record_retry_attempt(
        self,
        repository: str,
        language: str,
        attempt_number: int,
    ):
        """
        Record a retry attempt.

        Args:
            repository: Repository name
            language: Programming language
            attempt_number: Attempt number (1-based)
        """
        index_retry_attempts_total.labels(
            repository=repository,
            language=language,
            attempt_number=str(attempt_number),
        ).inc()

    def record_circuit_breaker_state(
        self,
        circuit_name: str,
        state: str,
    ):
        """
        Record circuit breaker state.

        Args:
            circuit_name: Name of circuit
            state: State (closed, half_open, open)
        """
        state_value = {
            "closed": 0,
            "half_open": 1,
            "open": 2,
        }.get(state, -1)

        circuit_breaker_state.labels(circuit_name=circuit_name).set(state_value)

    def record_circuit_breaker_failure(self, circuit_name: str):
        """
        Record a circuit breaker failure.

        Args:
            circuit_name: Name of circuit
        """
        circuit_breaker_failures_total.labels(circuit_name=circuit_name).inc()

    def emit_full_metrics(
        self,
        repository: str,
        language: str,
        indexer_type: str,
        duration_seconds: float,
        symbol_count: int,
        node_count: int,
        edge_count: int,
        file_count: int,
        status: str,
    ):
        """
        Emit a full set of metrics for an indexing operation.

        Args:
            repository: Repository name
            language: Programming language
            indexer_type: Type of indexer
            duration_seconds: Duration
            symbol_count: Number of symbols
            node_count: Number of nodes
            edge_count: Number of edges
            file_count: Number of files
            status: Status (success, failure)
        """
        # Record duration
        index_duration_seconds.labels(
            repository=repository,
            language=language,
            indexer_type=indexer_type,
            status=status,
        ).observe(duration_seconds)

        # Record counts
        self.record_symbol_count(repository, language, symbol_count)
        self.record_file_count(repository, language, file_count)
        self.record_graph_stats(repository, node_count, edge_count)

        # Record operation
        self.record_operation(repository, "index", status)

        self.logger.info(
            "Full metrics emitted",
            repository=repository,
            language=language,
            indexer_type=indexer_type,
            duration_seconds=duration_seconds,
            symbol_count=symbol_count,
            node_count=node_count,
            edge_count=edge_count,
            status=status,
        )


def get_metrics_text() -> str:
    """
    Get Prometheus metrics in text format.

    Returns:
        Metrics in Prometheus exposition format
    """
    return generate_latest(registry).decode("utf-8")


# Global collector instance
metrics_collector = IndexingMetricsCollector()
