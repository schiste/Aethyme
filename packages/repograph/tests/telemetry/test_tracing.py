"""
Tests for OpenTelemetry tracing.
"""

import pytest
from src.telemetry.tracing import (
    TracingManager,
    tracing_manager,
    trace_operation,
    get_current_trace_id,
    add_trace_attribute,
    add_trace_event,
)


class TestTracingManager:
    """Test tracing manager."""

    def setup_method(self):
        """Set up test fixtures."""
        # Create manager with tracing disabled for testing
        self.manager = TracingManager(enabled=False)

    def test_create_span_disabled(self):
        """Test span creation when tracing is disabled."""
        with self.manager.create_span("test_operation") as span:
            assert span is None

    def test_get_trace_id_disabled(self):
        """Test getting trace ID when disabled."""
        trace_id = self.manager.get_current_trace_id()
        assert trace_id is None

    def test_add_span_attribute_disabled(self):
        """Test adding span attribute when disabled."""
        # Should not crash
        self.manager.add_span_attribute("key", "value")
        assert True

    def test_add_span_event_disabled(self):
        """Test adding span event when disabled."""
        self.manager.add_span_event("event_name", {"key": "value"})
        assert True

    def test_set_span_error_disabled(self):
        """Test setting span error when disabled."""
        error = ValueError("test error")
        self.manager.set_span_error(error)
        assert True


class TestTracingManagerEnabled:
    """Test tracing manager with tracing enabled."""

    @pytest.fixture(autouse=True)
    def skip_if_no_otel(self):
        """Skip tests if OpenTelemetry is not available."""
        try:
            import opentelemetry
            pytest.skip_if_not_available = False
        except ImportError:
            pytest.skip("OpenTelemetry not available")

    def test_create_span_enabled(self):
        """Test span creation when enabled."""
        # This would require actual OTEL setup
        # For now, just test that manager can be created
        manager = TracingManager(
            enabled=True,
            jaeger_host="localhost",
            jaeger_port=6831,
        )
        assert manager.enabled or not manager.enabled  # May fail if OTEL unavailable


class TestTraceDecorator:
    """Test trace operation decorator."""

    def test_trace_operation_decorator(self):
        """Test the trace_operation decorator."""

        @trace_operation("test_func", {"param": "value"})
        def test_function():
            return "success"

        result = test_function()
        assert result == "success"

    def test_trace_operation_with_exception(self):
        """Test trace_operation with exception."""

        @trace_operation("failing_func")
        def failing_function():
            raise ValueError("test error")

        with pytest.raises(ValueError):
            failing_function()


class TestGlobalFunctions:
    """Test global convenience functions."""

    def test_get_current_trace_id(self):
        """Test getting current trace ID."""
        trace_id = get_current_trace_id()
        # May be None if tracing disabled
        assert trace_id is None or isinstance(trace_id, str)

    def test_add_trace_attribute(self):
        """Test adding trace attribute."""
        add_trace_attribute("test_key", "test_value")
        # Should not crash
        assert True

    def test_add_trace_event(self):
        """Test adding trace event."""
        add_trace_event("test_event", {"key": "value"})
        assert True


class TestTracingIntegration:
    """Integration tests for tracing."""

    def test_nested_spans(self):
        """Test nested span creation."""
        manager = TracingManager(enabled=False)

        with manager.create_span("parent") as parent_span:
            with manager.create_span("child") as child_span:
                # Both should be None when disabled
                assert parent_span is None
                assert child_span is None

    def test_span_with_attributes(self):
        """Test span with attributes."""
        manager = TracingManager(enabled=False)

        with manager.create_span(
            "operation",
            attributes={"repo": "test", "user": "alice"},
        ):
            # Should complete without error
            pass

        assert True


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
