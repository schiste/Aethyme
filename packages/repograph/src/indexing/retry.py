"""
Retry Logic with Exponential Backoff and Circuit Breaker

Provides resilient retry mechanisms for transient failures during indexing operations.
"""

import time
import structlog
from typing import Callable, Optional, Any, Dict
from dataclasses import dataclass, field
from enum import Enum
from datetime import datetime, timedelta
from functools import wraps

logger = structlog.get_logger(__name__)


class CircuitState(Enum):
    """State of the circuit breaker."""
    CLOSED = "closed"  # Normal operation
    OPEN = "open"  # Failures exceeded threshold, reject requests
    HALF_OPEN = "half_open"  # Testing if service recovered


@dataclass
class RetryConfig:
    """Configuration for retry behavior."""
    max_attempts: int = 3
    initial_delay_seconds: float = 1.0
    max_delay_seconds: float = 60.0
    exponential_base: float = 2.0
    jitter: bool = True  # Add randomness to prevent thundering herd


@dataclass
class CircuitBreakerConfig:
    """Configuration for circuit breaker."""
    failure_threshold: int = 5  # Failures before opening circuit
    success_threshold: int = 2  # Successes in half-open before closing
    timeout_seconds: float = 60.0  # Time to wait before half-open


class RetryableError(Exception):
    """Error that should trigger a retry."""
    pass


class NonRetryableError(Exception):
    """Error that should not be retried."""
    pass


class CircuitOpenError(Exception):
    """Error raised when circuit breaker is open."""
    pass


class CircuitBreaker:
    """
    Circuit breaker to prevent cascading failures.

    Tracks failure rate and temporarily blocks requests when threshold exceeded.
    """

    def __init__(self, config: CircuitBreakerConfig, name: str = "default"):
        self.config = config
        self.name = name
        self.state = CircuitState.CLOSED
        self.failure_count = 0
        self.success_count = 0
        self.last_failure_time: Optional[datetime] = None
        self.logger = logger.bind(component="CircuitBreaker", circuit=name)

    def call(self, func: Callable, *args, **kwargs) -> Any:
        """
        Execute function through circuit breaker.

        Args:
            func: Function to call
            *args, **kwargs: Arguments to pass to function

        Returns:
            Result of function call

        Raises:
            CircuitOpenError: If circuit is open
        """
        # Check circuit state
        if self.state == CircuitState.OPEN:
            if self._should_attempt_reset():
                self.logger.info("Circuit entering half-open state")
                self.state = CircuitState.HALF_OPEN
                self.success_count = 0
            else:
                raise CircuitOpenError(
                    f"Circuit breaker {self.name} is open. "
                    f"Last failure: {self.last_failure_time}"
                )

        try:
            result = func(*args, **kwargs)
            self._on_success()
            return result
        except Exception as e:
            self._on_failure()
            raise

    def _should_attempt_reset(self) -> bool:
        """Check if enough time has passed to attempt reset."""
        if not self.last_failure_time:
            return True

        elapsed = (datetime.now() - self.last_failure_time).total_seconds()
        return elapsed >= self.config.timeout_seconds

    def _on_success(self):
        """Handle successful call."""
        if self.state == CircuitState.HALF_OPEN:
            self.success_count += 1
            self.logger.info(
                "Success in half-open state",
                success_count=self.success_count,
                threshold=self.config.success_threshold,
            )

            if self.success_count >= self.config.success_threshold:
                self.logger.info("Circuit closing after successful recovery")
                self.state = CircuitState.CLOSED
                self.failure_count = 0
                self.success_count = 0
        elif self.state == CircuitState.CLOSED:
            # Reset failure count on success
            if self.failure_count > 0:
                self.logger.debug("Resetting failure count after success")
                self.failure_count = 0

    def _on_failure(self):
        """Handle failed call."""
        self.last_failure_time = datetime.now()
        self.failure_count += 1

        self.logger.warning(
            "Circuit breaker failure recorded",
            failure_count=self.failure_count,
            threshold=self.config.failure_threshold,
            state=self.state.value,
        )

        if self.state == CircuitState.HALF_OPEN:
            self.logger.warning("Circuit reopening after failure in half-open state")
            self.state = CircuitState.OPEN
            self.failure_count = 0
        elif self.failure_count >= self.config.failure_threshold:
            self.logger.error(
                "Circuit breaker opening due to failures",
                failure_count=self.failure_count,
            )
            self.state = CircuitState.OPEN

    def reset(self):
        """Manually reset circuit breaker."""
        self.logger.info("Manually resetting circuit breaker")
        self.state = CircuitState.CLOSED
        self.failure_count = 0
        self.success_count = 0
        self.last_failure_time = None

    def get_status(self) -> Dict[str, Any]:
        """Get current circuit breaker status."""
        return {
            "name": self.name,
            "state": self.state.value,
            "failure_count": self.failure_count,
            "success_count": self.success_count,
            "last_failure_time": self.last_failure_time.isoformat() if self.last_failure_time else None,
        }


class RetryManager:
    """
    Manages retry logic with exponential backoff.

    Handles transient failures with configurable retry attempts and delays.
    """

    def __init__(self, config: Optional[RetryConfig] = None):
        self.config = config or RetryConfig()
        self.logger = logger.bind(component="RetryManager")

    def execute_with_retry(
        self,
        func: Callable,
        *args,
        operation_name: str = "operation",
        **kwargs,
    ) -> Any:
        """
        Execute function with retry logic.

        Args:
            func: Function to execute
            *args, **kwargs: Arguments to pass to function
            operation_name: Name for logging

        Returns:
            Result of function call

        Raises:
            Last exception if all retries exhausted
        """
        last_exception = None

        for attempt in range(1, self.config.max_attempts + 1):
            try:
                self.logger.info(
                    "Attempting operation",
                    operation=operation_name,
                    attempt=attempt,
                    max_attempts=self.config.max_attempts,
                )

                result = func(*args, **kwargs)

                if attempt > 1:
                    self.logger.info(
                        "Operation succeeded after retry",
                        operation=operation_name,
                        attempt=attempt,
                    )

                return result

            except NonRetryableError as e:
                self.logger.error(
                    "Non-retryable error encountered",
                    operation=operation_name,
                    error=str(e),
                )
                raise

            except Exception as e:
                last_exception = e
                self.logger.warning(
                    "Operation failed, will retry",
                    operation=operation_name,
                    attempt=attempt,
                    max_attempts=self.config.max_attempts,
                    error=str(e),
                )

                # Don't sleep after last attempt
                if attempt < self.config.max_attempts:
                    delay = self._calculate_delay(attempt)
                    self.logger.debug(
                        "Waiting before retry",
                        operation=operation_name,
                        delay_seconds=delay,
                    )
                    time.sleep(delay)

        # All retries exhausted
        self.logger.error(
            "All retry attempts exhausted",
            operation=operation_name,
            attempts=self.config.max_attempts,
        )
        raise last_exception

    def _calculate_delay(self, attempt: int) -> float:
        """
        Calculate delay for retry attempt using exponential backoff.

        Args:
            attempt: Current attempt number (1-indexed)

        Returns:
            Delay in seconds
        """
        # Exponential backoff: initial_delay * (base ^ (attempt - 1))
        delay = self.config.initial_delay_seconds * (
            self.config.exponential_base ** (attempt - 1)
        )

        # Cap at max delay
        delay = min(delay, self.config.max_delay_seconds)

        # Add jitter to prevent thundering herd
        if self.config.jitter:
            import random
            jitter = random.uniform(0, delay * 0.1)  # ±10% jitter
            delay += jitter

        return delay


# Global circuit breakers for common operations
_circuit_breakers: Dict[str, CircuitBreaker] = {}


def get_circuit_breaker(
    name: str,
    config: Optional[CircuitBreakerConfig] = None,
) -> CircuitBreaker:
    """
    Get or create a circuit breaker by name.

    Args:
        name: Name of circuit breaker
        config: Configuration (only used when creating new circuit)

    Returns:
        CircuitBreaker instance
    """
    if name not in _circuit_breakers:
        config = config or CircuitBreakerConfig()
        _circuit_breakers[name] = CircuitBreaker(config, name)

    return _circuit_breakers[name]


def with_retry(
    max_attempts: int = 3,
    initial_delay: float = 1.0,
    operation_name: Optional[str] = None,
):
    """
    Decorator to add retry logic to a function.

    Args:
        max_attempts: Maximum retry attempts
        initial_delay: Initial delay in seconds
        operation_name: Name for logging (defaults to function name)

    Example:
        @with_retry(max_attempts=3, initial_delay=2.0)
        def flaky_operation():
            ...
    """
    def decorator(func: Callable) -> Callable:
        @wraps(func)
        def wrapper(*args, **kwargs):
            config = RetryConfig(
                max_attempts=max_attempts,
                initial_delay_seconds=initial_delay,
            )
            manager = RetryManager(config)
            op_name = operation_name or func.__name__

            return manager.execute_with_retry(func, *args, operation_name=op_name, **kwargs)

        return wrapper
    return decorator


def with_circuit_breaker(circuit_name: str):
    """
    Decorator to add circuit breaker protection to a function.

    Args:
        circuit_name: Name of circuit breaker to use

    Example:
        @with_circuit_breaker("scip_indexer")
        def index_with_scip():
            ...
    """
    def decorator(func: Callable) -> Callable:
        @wraps(func)
        def wrapper(*args, **kwargs):
            circuit = get_circuit_breaker(circuit_name)
            return circuit.call(func, *args, **kwargs)

        return wrapper
    return decorator
