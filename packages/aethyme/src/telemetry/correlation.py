"""
Request Correlation and Context Propagation.

Provides:
- Request ID generation and propagation
- Correlation between logs, metrics, and traces
- User session tracking
- Context propagation across service boundaries
"""

import uuid
import threading
from typing import Optional, Dict, Any
from contextvars import ContextVar
import structlog

logger = structlog.get_logger(__name__)

# Context variables for storing correlation IDs
_request_id: ContextVar[Optional[str]] = ContextVar('request_id', default=None)
_session_id: ContextVar[Optional[str]] = ContextVar('session_id', default=None)
_user_id: ContextVar[Optional[str]] = ContextVar('user_id', default=None)
_tenant_id: ContextVar[Optional[str]] = ContextVar('tenant_id', default=None)


class CorrelationManager:
    """
    Manages correlation IDs and context propagation.

    Provides methods for:
    - Generating and storing request IDs
    - Propagating context across async boundaries
    - Linking logs, metrics, and traces
    - Tracking user sessions
    """

    def __init__(self):
        self.logger = logger.bind(component="CorrelationManager")

    def generate_request_id(self) -> str:
        """
        Generate a new request ID.

        Returns:
            UUID-based request ID
        """
        return str(uuid.uuid4())

    def get_request_id(self) -> Optional[str]:
        """
        Get the current request ID.

        Returns:
            Current request ID or None
        """
        return _request_id.get()

    def set_request_id(self, request_id: str):
        """
        Set the current request ID.

        Args:
            request_id: Request ID to set
        """
        _request_id.set(request_id)
        self.logger.debug("Request ID set", request_id=request_id)

    def get_or_create_request_id(self) -> str:
        """
        Get the current request ID or create a new one.

        Returns:
            Request ID
        """
        request_id = self.get_request_id()
        if not request_id:
            request_id = self.generate_request_id()
            self.set_request_id(request_id)
        return request_id

    def get_session_id(self) -> Optional[str]:
        """
        Get the current session ID.

        Returns:
            Current session ID or None
        """
        return _session_id.get()

    def set_session_id(self, session_id: str):
        """
        Set the current session ID.

        Args:
            session_id: Session ID to set
        """
        _session_id.set(session_id)
        self.logger.debug("Session ID set", session_id=session_id)

    def get_user_id(self) -> Optional[str]:
        """
        Get the current user ID.

        Returns:
            Current user ID or None
        """
        return _user_id.get()

    def set_user_id(self, user_id: str):
        """
        Set the current user ID.

        Args:
            user_id: User ID to set
        """
        _user_id.set(user_id)
        self.logger.debug("User ID set", user_id=user_id)

    def get_tenant_id(self) -> Optional[str]:
        """
        Get the current tenant ID.

        Returns:
            Current tenant ID or None
        """
        return _tenant_id.get()

    def set_tenant_id(self, tenant_id: str):
        """
        Set the current tenant ID.

        Args:
            tenant_id: Tenant ID to set
        """
        _tenant_id.set(tenant_id)
        self.logger.debug("Tenant ID set", tenant_id=tenant_id)

    def get_correlation_context(self) -> Dict[str, Optional[str]]:
        """
        Get all correlation IDs.

        Returns:
            Dictionary with all correlation IDs
        """
        return {
            "request_id": self.get_request_id(),
            "session_id": self.get_session_id(),
            "user_id": self.get_user_id(),
            "tenant_id": self.get_tenant_id(),
        }

    def set_correlation_context(self, context: Dict[str, str]):
        """
        Set multiple correlation IDs from a context dictionary.

        Args:
            context: Dictionary with correlation IDs
        """
        if "request_id" in context:
            self.set_request_id(context["request_id"])
        if "session_id" in context:
            self.set_session_id(context["session_id"])
        if "user_id" in context:
            self.set_user_id(context["user_id"])
        if "tenant_id" in context:
            self.set_tenant_id(context["tenant_id"])

    def clear_correlation_context(self):
        """
        Clear all correlation IDs.
        """
        _request_id.set(None)
        _session_id.set(None)
        _user_id.set(None)
        _tenant_id.set(None)

    def get_structured_log_context(self) -> Dict[str, Any]:
        """
        Get correlation context formatted for structured logging.

        Returns:
            Dictionary with correlation IDs for logging
        """
        context = self.get_correlation_context()
        # Filter out None values
        return {k: v for k, v in context.items() if v is not None}

    def bind_logger(self, base_logger: Any) -> Any:
        """
        Bind correlation context to a structured logger.

        Args:
            base_logger: Base structlog logger

        Returns:
            Logger with correlation context bound
        """
        context = self.get_structured_log_context()
        return base_logger.bind(**context)

    def inject_http_headers(self) -> Dict[str, str]:
        """
        Inject correlation IDs into HTTP headers.

        Returns:
            Dictionary of HTTP headers with correlation IDs
        """
        headers = {}
        context = self.get_correlation_context()

        if context["request_id"]:
            headers["X-Request-ID"] = context["request_id"]
        if context["session_id"]:
            headers["X-Session-ID"] = context["session_id"]
        if context["user_id"]:
            headers["X-User-ID"] = context["user_id"]
        if context["tenant_id"]:
            headers["X-Tenant-ID"] = context["tenant_id"]

        return headers

    def extract_http_headers(self, headers: Dict[str, str]):
        """
        Extract correlation IDs from HTTP headers.

        Args:
            headers: Dictionary of HTTP headers
        """
        # Handle case-insensitive headers
        headers_lower = {k.lower(): v for k, v in headers.items()}

        if "x-request-id" in headers_lower:
            self.set_request_id(headers_lower["x-request-id"])
        if "x-session-id" in headers_lower:
            self.set_session_id(headers_lower["x-session-id"])
        if "x-user-id" in headers_lower:
            self.set_user_id(headers_lower["x-user-id"])
        if "x-tenant-id" in headers_lower:
            self.set_tenant_id(headers_lower["x-tenant-id"])


# Global correlation manager instance
correlation_manager = CorrelationManager()


# Convenience functions
def get_request_id() -> Optional[str]:
    """Get the current request ID."""
    return correlation_manager.get_request_id()


def set_request_id(request_id: str):
    """Set the current request ID."""
    correlation_manager.set_request_id(request_id)


def get_or_create_request_id() -> str:
    """Get or create a request ID."""
    return correlation_manager.get_or_create_request_id()


def get_correlation_context() -> Dict[str, Optional[str]]:
    """Get all correlation IDs."""
    return correlation_manager.get_correlation_context()


def set_correlation_context(context: Dict[str, str]):
    """Set multiple correlation IDs."""
    correlation_manager.set_correlation_context(context)


def clear_correlation_context():
    """Clear all correlation IDs."""
    correlation_manager.clear_correlation_context()
