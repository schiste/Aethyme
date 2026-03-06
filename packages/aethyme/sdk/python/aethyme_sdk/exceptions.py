"""Aethyme SDK exceptions."""

from typing import Any


class AethymeError(Exception):
    """Base exception for Aethyme SDK."""
    pass


class AuthenticationError(AethymeError):
    """Authentication failed."""
    pass


class APIError(AethymeError):
    """API request failed."""

    def __init__(
        self,
        message: str,
        status_code: int | None = None,
        response: dict[str, Any] | None = None,
    ):
        super().__init__(message)
        self.status_code = status_code
        self.response = response


class NotFoundError(APIError):
    """Resource not found (404)."""
    pass


class ValidationError(APIError):
    """Request validation failed (400)."""
    pass


class RateLimitError(APIError):
    """Rate limit exceeded (429)."""
    pass


class ServerError(APIError):
    """Server error (5xx)."""
    pass
