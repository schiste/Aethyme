"""
Distributed tracing with OpenTelemetry.

Features:
- Automatic request tracing
- Database query tracing
- External API call tracing
- Redis operation tracing
- Custom span creation
- Trace context propagation
"""

from opentelemetry import trace
from opentelemetry.sdk.trace import TracerProvider
from opentelemetry.sdk.trace.export import BatchSpanProcessor
from opentelemetry.sdk.resources import Resource, SERVICE_NAME, SERVICE_VERSION
from opentelemetry.exporter.otlp.proto.grpc.trace_exporter import OTLPSpanExporter
from opentelemetry.instrumentation.fastapi import FastAPIInstrumentor
from opentelemetry.instrumentation.sqlalchemy import SQLAlchemyInstrumentor
from opentelemetry.instrumentation.redis import RedisInstrumentor
from opentelemetry.instrumentation.httpx import HTTPXClientInstrumentor
from opentelemetry.trace import Status, StatusCode
from typing import Optional, Dict, Any
from contextlib import contextmanager
import logging

from app.core.config import settings

logger = logging.getLogger(__name__)

# Global tracer instance
_tracer: Optional[trace.Tracer] = None


def init_tracing() -> None:
    """
    Initialize OpenTelemetry tracing.

    Sets up:
    - Tracer provider with resource attributes
    - OTLP exporter (for sending traces to collector)
    - Auto-instrumentation for FastAPI, SQLAlchemy, Redis, HTTPX
    """
    global _tracer

    # Only enable tracing in production or if explicitly enabled
    if settings.ENVIRONMENT != "production" and not settings.DEBUG:
        logger.info("Tracing disabled (not in production)")
        return

    try:
        # Create resource with service information
        resource = Resource(
            attributes={
                SERVICE_NAME: "repograph-cloud-api",
                SERVICE_VERSION: "1.0.0",
                "environment": settings.ENVIRONMENT,
            }
        )

        # Create tracer provider
        provider = TracerProvider(resource=resource)

        # Add OTLP exporter (sends traces to OpenTelemetry collector)
        # The collector can then send to Jaeger, Zipkin, or other backends
        otlp_exporter = OTLPSpanExporter(
            endpoint="http://localhost:4317",  # OTLP gRPC endpoint
            insecure=True,  # Use TLS in production
        )

        # Use batch processor for better performance
        processor = BatchSpanProcessor(otlp_exporter)
        provider.add_span_processor(processor)

        # Set global tracer provider
        trace.set_tracer_provider(provider)

        # Get tracer instance
        _tracer = trace.get_tracer(__name__)

        logger.info("OpenTelemetry tracing initialized successfully")

    except Exception as e:
        logger.error(f"Failed to initialize tracing: {e}")
        # Don't fail startup if tracing fails
        return


def instrument_app(app) -> None:
    """
    Instrument FastAPI application with OpenTelemetry.

    Args:
        app: FastAPI application instance
    """
    try:
        # Instrument FastAPI
        FastAPIInstrumentor.instrument_app(app)

        # Instrument database (SQLAlchemy)
        SQLAlchemyInstrumentor().instrument()

        # Instrument Redis
        RedisInstrumentor().instrument()

        # Instrument HTTPX (for external API calls)
        HTTPXClientInstrumentor().instrument()

        logger.info("Application instrumentation completed")

    except Exception as e:
        logger.error(f"Failed to instrument application: {e}")


def get_tracer() -> trace.Tracer:
    """
    Get the global tracer instance.

    Returns:
        OpenTelemetry tracer
    """
    global _tracer
    if _tracer is None:
        _tracer = trace.get_tracer(__name__)
    return _tracer


@contextmanager
def trace_operation(
    operation_name: str,
    attributes: Optional[Dict[str, Any]] = None,
    record_exception: bool = True
):
    """
    Context manager for tracing custom operations.

    Usage:
        with trace_operation("process_repository", {"repo_id": 123}):
            # Your code here
            process_repository(123)

    Args:
        operation_name: Name of the operation to trace
        attributes: Additional attributes to add to the span
        record_exception: Whether to record exceptions in the span
    """
    tracer = get_tracer()
    with tracer.start_as_current_span(operation_name) as span:
        try:
            # Add custom attributes
            if attributes:
                for key, value in attributes.items():
                    span.set_attribute(key, str(value))

            yield span

            # Mark as successful
            span.set_status(Status(StatusCode.OK))

        except Exception as e:
            # Record exception
            if record_exception:
                span.record_exception(e)
                span.set_status(Status(StatusCode.ERROR, str(e)))
            raise


def add_span_attributes(**attributes: Any) -> None:
    """
    Add attributes to the current span.

    Usage:
        add_span_attributes(user_id=123, action="create_repository")

    Args:
        **attributes: Key-value pairs to add as span attributes
    """
    span = trace.get_current_span()
    if span.is_recording():
        for key, value in attributes.items():
            span.set_attribute(key, str(value))


def add_span_event(name: str, attributes: Optional[Dict[str, Any]] = None) -> None:
    """
    Add an event to the current span.

    Usage:
        add_span_event("repository_indexed", {"symbols_count": 1500})

    Args:
        name: Event name
        attributes: Event attributes
    """
    span = trace.get_current_span()
    if span.is_recording():
        span.add_event(name, attributes=attributes or {})


def get_trace_id() -> str:
    """
    Get the current trace ID.

    Returns:
        Trace ID as hex string, or empty string if not in a trace
    """
    span = trace.get_current_span()
    if span.get_span_context().is_valid:
        return format(span.get_span_context().trace_id, '032x')
    return ""


def get_span_id() -> str:
    """
    Get the current span ID.

    Returns:
        Span ID as hex string, or empty string if not in a span
    """
    span = trace.get_current_span()
    if span.get_span_context().is_valid:
        return format(span.get_span_context().span_id, '016x')
    return ""


# Decorators for common operations
def trace_db_operation(operation_name: str):
    """
    Decorator for tracing database operations.

    Usage:
        @trace_db_operation("fetch_repositories")
        async def get_repositories(db: AsyncSession):
            ...
    """
    def decorator(func):
        async def wrapper(*args, **kwargs):
            with trace_operation(
                f"db.{operation_name}",
                {"db.operation": operation_name}
            ):
                return await func(*args, **kwargs)
        return wrapper
    return decorator


def trace_api_call(service_name: str, endpoint: str):
    """
    Decorator for tracing external API calls.

    Usage:
        @trace_api_call("github", "/repos/{owner}/{repo}")
        async def fetch_github_repo(owner: str, repo: str):
            ...
    """
    def decorator(func):
        async def wrapper(*args, **kwargs):
            with trace_operation(
                f"api.{service_name}",
                {
                    "service.name": service_name,
                    "http.endpoint": endpoint,
                }
            ):
                return await func(*args, **kwargs)
        return wrapper
    return decorator


def trace_background_task(task_name: str):
    """
    Decorator for tracing Celery background tasks.

    Usage:
        @trace_background_task("index_repository")
        def index_repository_task(repo_id: int):
            ...
    """
    def decorator(func):
        def wrapper(*args, **kwargs):
            with trace_operation(
                f"task.{task_name}",
                {
                    "task.name": task_name,
                    "task.args": str(args),
                }
            ):
                return func(*args, **kwargs)
        return wrapper
    return decorator
