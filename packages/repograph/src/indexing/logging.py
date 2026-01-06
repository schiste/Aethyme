"""
Structured Logging for Indexing Operations

Provides JSON-formatted structured logging with correlation IDs for tracing
and comprehensive context for debugging.
"""

import uuid
import structlog
import logging
import sys
from typing import Dict, Optional, Any
from datetime import datetime
from contextlib import contextmanager


def setup_indexing_logging(
    log_level: str = "INFO",
    json_format: bool = True,
):
    """
    Configure structured logging for indexing operations.

    Args:
        log_level: Logging level (DEBUG, INFO, WARNING, ERROR)
        json_format: Use JSON format (True) or console format (False)
    """
    # Configure standard library logging
    logging.basicConfig(
        format="%(message)s",
        stream=sys.stdout,
        level=getattr(logging, log_level.upper()),
    )

    # Configure structlog
    processors = [
        structlog.stdlib.filter_by_level,
        structlog.stdlib.add_logger_name,
        structlog.stdlib.add_log_level,
        structlog.processors.TimeStamper(fmt="iso"),
        structlog.processors.StackInfoRenderer(),
        structlog.processors.format_exc_info,
    ]

    if json_format:
        # JSON output for production
        processors.append(structlog.processors.JSONRenderer())
    else:
        # Console output for development
        processors.extend([
            structlog.stdlib.PositionalArgumentsFormatter(),
            structlog.dev.ConsoleRenderer(colors=True),
        ])

    structlog.configure(
        processors=processors,
        context_class=dict,
        logger_factory=structlog.stdlib.LoggerFactory(),
        cache_logger_on_first_use=True,
    )


class IndexingLogger:
    """
    Structured logger for indexing operations.

    Automatically adds context and correlation IDs to all log entries.
    """

    def __init__(
        self,
        repository_id: Optional[str] = None,
        repository_name: Optional[str] = None,
        correlation_id: Optional[str] = None,
    ):
        """
        Initialize indexing logger.

        Args:
            repository_id: Repository ID
            repository_name: Repository name
            correlation_id: Correlation ID for tracing
        """
        self.correlation_id = correlation_id or str(uuid.uuid4())
        self.repository_id = repository_id
        self.repository_name = repository_name
        self.start_time = datetime.now()

        # Create logger with context
        self.logger = structlog.get_logger().bind(
            correlation_id=self.correlation_id,
            repository_id=repository_id,
            repository_name=repository_name,
        )

    def log_index_start(
        self,
        language: str,
        indexer_type: str,
        additional_context: Optional[Dict[str, Any]] = None,
    ):
        """
        Log start of indexing operation.

        Args:
            language: Programming language
            indexer_type: Type of indexer
            additional_context: Additional context to log
        """
        context = {
            "event": "index_start",
            "language": language,
            "indexer_type": indexer_type,
            "timestamp": datetime.now().isoformat(),
        }

        if additional_context:
            context.update(additional_context)

        self.logger.info("Indexing started", **context)

    def log_index_complete(
        self,
        language: str,
        indexer_type: str,
        duration_seconds: float,
        symbol_count: int,
        node_count: int,
        edge_count: int,
        file_count: int,
        additional_context: Optional[Dict[str, Any]] = None,
    ):
        """
        Log completion of indexing operation.

        Args:
            language: Programming language
            indexer_type: Type of indexer
            duration_seconds: Duration
            symbol_count: Number of symbols indexed
            node_count: Number of nodes created
            edge_count: Number of edges created
            file_count: Number of files processed
            additional_context: Additional context to log
        """
        context = {
            "event": "index_complete",
            "language": language,
            "indexer_type": indexer_type,
            "duration_seconds": duration_seconds,
            "symbol_count": symbol_count,
            "node_count": node_count,
            "edge_count": edge_count,
            "file_count": file_count,
            "timestamp": datetime.now().isoformat(),
        }

        if additional_context:
            context.update(additional_context)

        self.logger.info("Indexing completed", **context)

    def log_index_failure(
        self,
        language: str,
        indexer_type: str,
        error: Exception,
        duration_seconds: float,
        additional_context: Optional[Dict[str, Any]] = None,
    ):
        """
        Log indexing failure.

        Args:
            language: Programming language
            indexer_type: Type of indexer
            error: Exception that occurred
            duration_seconds: Duration before failure
            additional_context: Additional context to log
        """
        context = {
            "event": "index_failure",
            "language": language,
            "indexer_type": indexer_type,
            "error_type": type(error).__name__,
            "error_message": str(error),
            "duration_seconds": duration_seconds,
            "timestamp": datetime.now().isoformat(),
        }

        if additional_context:
            context.update(additional_context)

        self.logger.error("Indexing failed", **context, exc_info=True)

    def log_fallback_triggered(
        self,
        language: str,
        reason: str,
        additional_context: Optional[Dict[str, Any]] = None,
    ):
        """
        Log when fallback indexer is triggered.

        Args:
            language: Programming language
            reason: Reason for fallback
            additional_context: Additional context to log
        """
        context = {
            "event": "fallback_triggered",
            "language": language,
            "reason": reason,
            "timestamp": datetime.now().isoformat(),
        }

        if additional_context:
            context.update(additional_context)

        self.logger.warning("Fallback indexer triggered", **context)

    def log_retry_attempt(
        self,
        language: str,
        attempt_number: int,
        max_attempts: int,
        delay_seconds: float,
        error: Exception,
        additional_context: Optional[Dict[str, Any]] = None,
    ):
        """
        Log retry attempt.

        Args:
            language: Programming language
            attempt_number: Current attempt number
            max_attempts: Maximum attempts
            delay_seconds: Delay before retry
            error: Exception that triggered retry
            additional_context: Additional context to log
        """
        context = {
            "event": "retry_attempt",
            "language": language,
            "attempt_number": attempt_number,
            "max_attempts": max_attempts,
            "delay_seconds": delay_seconds,
            "error_type": type(error).__name__,
            "error_message": str(error),
            "timestamp": datetime.now().isoformat(),
        }

        if additional_context:
            context.update(additional_context)

        self.logger.warning("Retry attempt", **context)

    def log_language_breakdown(
        self,
        language_stats: Dict[str, int],
        additional_context: Optional[Dict[str, Any]] = None,
    ):
        """
        Log language breakdown.

        Args:
            language_stats: Dict mapping language to symbol count
            additional_context: Additional context to log
        """
        context = {
            "event": "language_breakdown",
            "language_breakdown": language_stats,
            "total_symbols": sum(language_stats.values()),
            "timestamp": datetime.now().isoformat(),
        }

        if additional_context:
            context.update(additional_context)

        self.logger.info("Language breakdown", **context)

    def log_file_processing(
        self,
        file_path: str,
        language: str,
        symbols_found: int,
        additional_context: Optional[Dict[str, Any]] = None,
    ):
        """
        Log processing of individual file.

        Args:
            file_path: Path to file
            language: Programming language
            symbols_found: Number of symbols found
            additional_context: Additional context to log
        """
        context = {
            "event": "file_processed",
            "file_path": file_path,
            "language": language,
            "symbols_found": symbols_found,
            "timestamp": datetime.now().isoformat(),
        }

        if additional_context:
            context.update(additional_context)

        self.logger.debug("File processed", **context)

    def log_circuit_breaker_event(
        self,
        circuit_name: str,
        old_state: str,
        new_state: str,
        failure_count: int,
        additional_context: Optional[Dict[str, Any]] = None,
    ):
        """
        Log circuit breaker state change.

        Args:
            circuit_name: Name of circuit
            old_state: Previous state
            new_state: New state
            failure_count: Current failure count
            additional_context: Additional context to log
        """
        context = {
            "event": "circuit_breaker_state_change",
            "circuit_name": circuit_name,
            "old_state": old_state,
            "new_state": new_state,
            "failure_count": failure_count,
            "timestamp": datetime.now().isoformat(),
        }

        if additional_context:
            context.update(additional_context)

        self.logger.warning("Circuit breaker state changed", **context)

    @contextmanager
    def operation_context(
        self,
        operation_name: str,
        **context_kwargs,
    ):
        """
        Context manager for logging operation start/end.

        Args:
            operation_name: Name of operation
            **context_kwargs: Additional context

        Example:
            with logger.operation_context("validation", language="python"):
                # operation code
                pass
        """
        start_time = datetime.now()
        context = {
            "event": f"{operation_name}_start",
            "timestamp": start_time.isoformat(),
            **context_kwargs,
        }

        self.logger.info(f"{operation_name} started", **context)

        try:
            yield
            end_time = datetime.now()
            duration = (end_time - start_time).total_seconds()

            context.update({
                "event": f"{operation_name}_complete",
                "duration_seconds": duration,
                "timestamp": end_time.isoformat(),
            })

            self.logger.info(f"{operation_name} completed", **context)

        except Exception as e:
            end_time = datetime.now()
            duration = (end_time - start_time).total_seconds()

            context.update({
                "event": f"{operation_name}_failed",
                "duration_seconds": duration,
                "error_type": type(e).__name__,
                "error_message": str(e),
                "timestamp": end_time.isoformat(),
            })

            self.logger.error(f"{operation_name} failed", **context, exc_info=True)
            raise


def create_indexing_logger(
    repository_id: Optional[str] = None,
    repository_name: Optional[str] = None,
    correlation_id: Optional[str] = None,
) -> IndexingLogger:
    """
    Factory function to create an indexing logger.

    Args:
        repository_id: Repository ID
        repository_name: Repository name
        correlation_id: Correlation ID

    Returns:
        IndexingLogger instance
    """
    return IndexingLogger(
        repository_id=repository_id,
        repository_name=repository_name,
        correlation_id=correlation_id,
    )
