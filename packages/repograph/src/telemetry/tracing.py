"""
OpenTelemetry Distributed Tracing for RepoGraph.

Provides:
- Span creation and management
- Trace context propagation
- Integration with Jaeger
- Correlation with metrics and logs
"""

import os
import uuid
from contextlib import contextmanager
from typing import Optional, Dict, Any
import structlog

try:
    from opentelemetry import trace
    from opentelemetry.sdk.trace import TracerProvider
    from opentelemetry.sdk.trace.export import BatchSpanProcessor
    from opentelemetry.exporter.jaeger.thrift import JaegerExporter
    from opentelemetry.sdk.resources import SERVICE_NAME, Resource
    from opentelemetry.trace import Status, StatusCode, SpanKind
    OTEL_AVAILABLE = True
except ImportError:
    OTEL_AVAILABLE = False

logger = structlog.get_logger(__name__)


class TracingManager:
    """
    Manages OpenTelemetry tracing for RepoGraph.

    Provides methods for creating spans, propagating context,
    and correlating traces with metrics and logs.
    """

    def __init__(
        self,
        service_name: str = "repograph",
        jaeger_host: Optional[str] = None,
        jaeger_port: Optional[int] = None,
        enabled: bool = True,
    ):
        """
        Initialize the tracing manager.

        Args:
            service_name: Name of the service
            jaeger_host: Jaeger collector host
            jaeger_port: Jaeger collector port
            enabled: Whether tracing is enabled
        """
        self.service_name = service_name
        self.enabled = enabled and OTEL_AVAILABLE
        self.logger = logger.bind(component="TracingManager")

        if not OTEL_AVAILABLE:
            self.logger.warning(
                "OpenTelemetry not available - tracing disabled",
                reason="missing_dependencies"
            )
            self.enabled = False
            return

        if not self.enabled:
            self.logger.info("Tracing disabled by configuration")
            return

        # Set up tracer provider
        resource = Resource(attributes={
            SERVICE_NAME: service_name
        })

        provider = TracerProvider(resource=resource)

        # Configure Jaeger exporter if available
        jaeger_host = jaeger_host or os.getenv("JAEGER_HOST", "localhost")
        jaeger_port = jaeger_port or int(os.getenv("JAEGER_PORT", "6831"))

        try:
            jaeger_exporter = JaegerExporter(
                agent_host_name=jaeger_host,
                agent_port=jaeger_port,
            )
            processor = BatchSpanProcessor(jaeger_exporter)
            provider.add_span_processor(processor)

            self.logger.info(
                "Jaeger exporter configured",
                host=jaeger_host,
                port=jaeger_port,
            )
        except Exception as e:
            self.logger.warning(
                "Failed to configure Jaeger exporter",
                error=str(e),
            )

        # Set the global tracer provider
        trace.set_tracer_provider(provider)
        self.tracer = trace.get_tracer(__name__)

        self.logger.info(
            "Tracing initialized",
            service_name=service_name,
        )

    @contextmanager
    def create_span(
        self,
        name: str,
        attributes: Optional[Dict[str, Any]] = None,
        kind: Optional[SpanKind] = None,
    ):
        """
        Create a new span.

        Args:
            name: Span name
            attributes: Optional attributes
            kind: Optional span kind

        Yields:
            Span object (or None if tracing disabled)

        Example:
            with tracing_manager.create_span("index_repository", {"repo": "myrepo"}):
                # indexing code
                pass
        """
        if not self.enabled:
            yield None
            return

        kind = kind or SpanKind.INTERNAL
        span = self.tracer.start_span(name, kind=kind)

        if attributes:
            for key, value in attributes.items():
                span.set_attribute(key, str(value))

        try:
            with trace.use_span(span, end_on_exit=True):
                yield span
        except Exception as e:
            if span:
                span.set_status(Status(StatusCode.ERROR, str(e)))
                span.record_exception(e)
            raise

    def get_current_trace_id(self) -> Optional[str]:
        """
        Get the current trace ID.

        Returns:
            Trace ID as hex string, or None if no active span
        """
        if not self.enabled:
            return None

        span = trace.get_current_span()
        if span and span.get_span_context().is_valid:
            return format(span.get_span_context().trace_id, '032x')
        return None

    def get_current_span_id(self) -> Optional[str]:
        """
        Get the current span ID.

        Returns:
            Span ID as hex string, or None if no active span
        """
        if not self.enabled:
            return None

        span = trace.get_current_span()
        if span and span.get_span_context().is_valid:
            return format(span.get_span_context().span_id, '016x')
        return None

    def add_span_attribute(self, key: str, value: Any):
        """
        Add an attribute to the current span.

        Args:
            key: Attribute key
            value: Attribute value
        """
        if not self.enabled:
            return

        span = trace.get_current_span()
        if span:
            span.set_attribute(key, str(value))

    def add_span_event(self, name: str, attributes: Optional[Dict[str, Any]] = None):
        """
        Add an event to the current span.

        Args:
            name: Event name
            attributes: Optional event attributes
        """
        if not self.enabled:
            return

        span = trace.get_current_span()
        if span:
            span.add_event(name, attributes=attributes or {})

    def set_span_error(self, error: Exception):
        """
        Mark the current span as having an error.

        Args:
            error: The exception that occurred
        """
        if not self.enabled:
            return

        span = trace.get_current_span()
        if span:
            span.set_status(Status(StatusCode.ERROR, str(error)))
            span.record_exception(error)


# Global tracing manager instance
_tracing_manager: Optional[TracingManager] = None


def get_tracing_manager() -> TracingManager:
    """
    Get or create the global tracing manager.

    Returns:
        Global TracingManager instance
    """
    global _tracing_manager
    if _tracing_manager is None:
        enabled = os.getenv("TELEM_EVAL_V1", "false").lower() == "true"
        _tracing_manager = TracingManager(enabled=enabled)
    return _tracing_manager


# Convenience accessor
tracing_manager = get_tracing_manager()


def trace_operation(
    name: str,
    attributes: Optional[Dict[str, Any]] = None,
    kind: Optional[SpanKind] = None,
):
    """
    Decorator to trace a function.

    Args:
        name: Span name
        attributes: Optional attributes
        kind: Optional span kind

    Example:
        @trace_operation("index_repository", {"repo": "myrepo"})
        def index_repo(repo_path):
            # indexing code
            pass
    """
    def decorator(func):
        def wrapper(*args, **kwargs):
            manager = get_tracing_manager()
            with manager.create_span(name, attributes, kind):
                return func(*args, **kwargs)
        return wrapper
    return decorator


def get_current_trace_id() -> Optional[str]:
    """
    Get the current trace ID.

    Returns:
        Trace ID as hex string, or None if no active span
    """
    manager = get_tracing_manager()
    return manager.get_current_trace_id()


def add_trace_attribute(key: str, value: Any):
    """
    Add an attribute to the current span.

    Args:
        key: Attribute key
        value: Attribute value
    """
    manager = get_tracing_manager()
    manager.add_span_attribute(key, value)


def add_trace_event(name: str, attributes: Optional[Dict[str, Any]] = None):
    """
    Add an event to the current span.

    Args:
        name: Event name
        attributes: Optional event attributes
    """
    manager = get_tracing_manager()
    manager.add_span_event(name, attributes)
