"""
Tests for correlation and context propagation.
"""

import pytest
from src.telemetry.correlation import (
    CorrelationManager,
    correlation_manager,
    get_request_id,
    set_request_id,
    get_or_create_request_id,
    get_correlation_context,
    set_correlation_context,
    clear_correlation_context,
)


class TestCorrelationManager:
    """Test correlation manager."""

    def setup_method(self):
        """Set up test fixtures."""
        self.manager = CorrelationManager()
        # Clear any existing context
        self.manager.clear_correlation_context()

    def teardown_method(self):
        """Clean up after tests."""
        self.manager.clear_correlation_context()

    def test_generate_request_id(self):
        """Test request ID generation."""
        request_id = self.manager.generate_request_id()
        assert isinstance(request_id, str)
        assert len(request_id) > 0

        # Should be unique
        request_id2 = self.manager.generate_request_id()
        assert request_id != request_id2

    def test_set_and_get_request_id(self):
        """Test setting and getting request ID."""
        test_id = "test-request-123"
        self.manager.set_request_id(test_id)

        retrieved_id = self.manager.get_request_id()
        assert retrieved_id == test_id

    def test_get_request_id_none_when_not_set(self):
        """Test getting request ID when not set."""
        request_id = self.manager.get_request_id()
        assert request_id is None

    def test_get_or_create_request_id(self):
        """Test get or create request ID."""
        # First call should create
        request_id1 = self.manager.get_or_create_request_id()
        assert isinstance(request_id1, str)

        # Second call should return same
        request_id2 = self.manager.get_or_create_request_id()
        assert request_id1 == request_id2

    def test_set_and_get_session_id(self):
        """Test session ID management."""
        test_session = "session-abc-123"
        self.manager.set_session_id(test_session)

        retrieved = self.manager.get_session_id()
        assert retrieved == test_session

    def test_set_and_get_user_id(self):
        """Test user ID management."""
        test_user = "user-456"
        self.manager.set_user_id(test_user)

        retrieved = self.manager.get_user_id()
        assert retrieved == test_user

    def test_set_and_get_tenant_id(self):
        """Test tenant ID management."""
        test_tenant = "tenant-789"
        self.manager.set_tenant_id(test_tenant)

        retrieved = self.manager.get_tenant_id()
        assert retrieved == test_tenant

    def test_get_correlation_context(self):
        """Test getting full correlation context."""
        self.manager.set_request_id("req-123")
        self.manager.set_session_id("sess-456")
        self.manager.set_user_id("user-789")
        self.manager.set_tenant_id("tenant-abc")

        context = self.manager.get_correlation_context()

        assert context["request_id"] == "req-123"
        assert context["session_id"] == "sess-456"
        assert context["user_id"] == "user-789"
        assert context["tenant_id"] == "tenant-abc"

    def test_set_correlation_context(self):
        """Test setting full correlation context."""
        context = {
            "request_id": "req-new-123",
            "session_id": "sess-new-456",
            "user_id": "user-new-789",
            "tenant_id": "tenant-new-abc",
        }

        self.manager.set_correlation_context(context)

        assert self.manager.get_request_id() == "req-new-123"
        assert self.manager.get_session_id() == "sess-new-456"
        assert self.manager.get_user_id() == "user-new-789"
        assert self.manager.get_tenant_id() == "tenant-new-abc"

    def test_set_correlation_context_partial(self):
        """Test setting partial correlation context."""
        context = {
            "request_id": "req-partial",
            "user_id": "user-partial",
        }

        self.manager.set_correlation_context(context)

        assert self.manager.get_request_id() == "req-partial"
        assert self.manager.get_user_id() == "user-partial"
        assert self.manager.get_session_id() is None
        assert self.manager.get_tenant_id() is None

    def test_clear_correlation_context(self):
        """Test clearing correlation context."""
        self.manager.set_request_id("req-123")
        self.manager.set_session_id("sess-456")

        self.manager.clear_correlation_context()

        assert self.manager.get_request_id() is None
        assert self.manager.get_session_id() is None
        assert self.manager.get_user_id() is None
        assert self.manager.get_tenant_id() is None

    def test_get_structured_log_context(self):
        """Test getting structured log context."""
        self.manager.set_request_id("req-123")
        self.manager.set_user_id("user-456")

        log_context = self.manager.get_structured_log_context()

        assert "request_id" in log_context
        assert log_context["request_id"] == "req-123"
        assert "user_id" in log_context
        assert log_context["user_id"] == "user-456"

        # Should not include None values
        assert "session_id" not in log_context
        assert "tenant_id" not in log_context

    def test_inject_http_headers(self):
        """Test injecting correlation IDs into HTTP headers."""
        self.manager.set_request_id("req-http-123")
        self.manager.set_session_id("sess-http-456")

        headers = self.manager.inject_http_headers()

        assert "X-Request-ID" in headers
        assert headers["X-Request-ID"] == "req-http-123"
        assert "X-Session-ID" in headers
        assert headers["X-Session-ID"] == "sess-http-456"

    def test_inject_http_headers_empty(self):
        """Test injecting headers with no context set."""
        headers = self.manager.inject_http_headers()
        assert len(headers) == 0

    def test_extract_http_headers(self):
        """Test extracting correlation IDs from HTTP headers."""
        headers = {
            "X-Request-ID": "req-extracted-123",
            "X-Session-ID": "sess-extracted-456",
            "X-User-ID": "user-extracted-789",
            "X-Tenant-ID": "tenant-extracted-abc",
        }

        self.manager.extract_http_headers(headers)

        assert self.manager.get_request_id() == "req-extracted-123"
        assert self.manager.get_session_id() == "sess-extracted-456"
        assert self.manager.get_user_id() == "user-extracted-789"
        assert self.manager.get_tenant_id() == "tenant-extracted-abc"

    def test_extract_http_headers_case_insensitive(self):
        """Test header extraction is case insensitive."""
        headers = {
            "x-request-id": "req-lower-123",
            "X-SESSION-ID": "sess-upper-456",
        }

        self.manager.extract_http_headers(headers)

        assert self.manager.get_request_id() == "req-lower-123"
        assert self.manager.get_session_id() == "sess-upper-456"


class TestGlobalFunctions:
    """Test global convenience functions."""

    def setup_method(self):
        """Set up test fixtures."""
        clear_correlation_context()

    def teardown_method(self):
        """Clean up after tests."""
        clear_correlation_context()

    def test_global_get_request_id(self):
        """Test global get_request_id."""
        assert get_request_id() is None

    def test_global_set_request_id(self):
        """Test global set_request_id."""
        set_request_id("global-req-123")
        assert get_request_id() == "global-req-123"

    def test_global_get_or_create_request_id(self):
        """Test global get_or_create_request_id."""
        request_id = get_or_create_request_id()
        assert isinstance(request_id, str)

    def test_global_get_correlation_context(self):
        """Test global get_correlation_context."""
        set_request_id("ctx-req-123")
        context = get_correlation_context()
        assert context["request_id"] == "ctx-req-123"

    def test_global_set_correlation_context(self):
        """Test global set_correlation_context."""
        set_correlation_context({
            "request_id": "ctx-new-123",
            "user_id": "ctx-user-456",
        })
        assert get_request_id() == "ctx-new-123"

    def test_global_clear_correlation_context(self):
        """Test global clear_correlation_context."""
        set_request_id("to-be-cleared")
        clear_correlation_context()
        assert get_request_id() is None


class TestCorrelationIntegration:
    """Integration tests for correlation."""

    def setup_method(self):
        """Set up test fixtures."""
        self.manager = CorrelationManager()
        self.manager.clear_correlation_context()

    def teardown_method(self):
        """Clean up after tests."""
        self.manager.clear_correlation_context()

    def test_full_request_lifecycle(self):
        """Test full request correlation lifecycle."""
        # Simulate incoming request
        incoming_headers = {
            "X-Request-ID": "incoming-req-123",
            "X-Session-ID": "incoming-sess-456",
        }

        self.manager.extract_http_headers(incoming_headers)

        # Verify context is set
        assert self.manager.get_request_id() == "incoming-req-123"
        assert self.manager.get_session_id() == "incoming-sess-456"

        # Simulate outgoing request
        outgoing_headers = self.manager.inject_http_headers()

        assert outgoing_headers["X-Request-ID"] == "incoming-req-123"
        assert outgoing_headers["X-Session-ID"] == "incoming-sess-456"

    def test_context_isolation(self):
        """Test that context is properly isolated."""
        # This test verifies that ContextVar provides isolation
        # In a real scenario, this would be tested with threads/async
        self.manager.set_request_id("isolated-123")
        assert self.manager.get_request_id() == "isolated-123"


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
