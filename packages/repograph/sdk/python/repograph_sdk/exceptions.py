"""RepoGraph SDK exceptions."""

from typing import Optional, Dict, Any


class RepoGraphError(Exception):
    """Base exception for RepoGraph SDK."""
    pass


class AuthenticationError(RepoGraphError):
    """Authentication failed."""
    pass


class APIError(RepoGraphError):
    """API request failed."""

    def __init__(
        self,
        message: str,
        status_code: Optional[int] = None,
        response: Optional[Dict[str, Any]] = None,
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
