"""
Input validation and sanitization middleware.

Provides:
- Request body validation
- SQL injection prevention
- XSS prevention
- Path traversal prevention
- Content-Type validation
- Request size limits
"""

from fastapi import Request, HTTPException
from starlette.middleware.base import BaseHTTPMiddleware
from typing import Callable, Any
import re
import json
import logging

logger = logging.getLogger(__name__)

# Maximum request body size (10 MB)
MAX_BODY_SIZE = 10 * 1024 * 1024

# Dangerous patterns to detect
SQL_INJECTION_PATTERNS = [
    r"(\bunion\b.*\bselect\b)",
    r"(\bselect\b.*\bfrom\b)",
    r"(\binsert\b.*\binto\b)",
    r"(\bupdate\b.*\bset\b)",
    r"(\bdelete\b.*\bfrom\b)",
    r"(\bdrop\b.*\btable\b)",
    r"(\bexec\b.*\()",
    r"(\bexecute\b.*\()",
    r"(--\s*$)",  # SQL comments
    r"(/\*.*\*/)",  # SQL block comments
    r"(\bor\b.*=.*)",
    r"(\band\b.*=.*)",
]

XSS_PATTERNS = [
    r"<script[^>]*>.*?</script>",
    r"javascript:",
    r"on\w+\s*=",  # Event handlers like onclick=
    r"<iframe",
    r"<object",
    r"<embed",
]

PATH_TRAVERSAL_PATTERNS = [
    r"\.\./",
    r"\.\.",
    r"%2e%2e",
    r"%252e%252e",
]


class InputValidationMiddleware(BaseHTTPMiddleware):
    """
    Validate and sanitize all incoming requests.

    Checks:
    - Content-Type for POST/PUT/PATCH
    - Request body size
    - SQL injection patterns
    - XSS patterns
    - Path traversal attempts
    """

    async def dispatch(self, request: Request, call_next: Callable) -> Any:
        # Skip validation for health checks and static files
        if request.url.path.startswith(("/health", "/static", "/docs", "/openapi.json")):
            return await call_next(request)

        # Validate Content-Type for methods with body
        if request.method in ("POST", "PUT", "PATCH"):
            content_type = request.headers.get("content-type", "")

            # Allow these content types
            allowed_types = [
                "application/json",
                "application/x-www-form-urlencoded",
                "multipart/form-data",
            ]

            if not any(ct in content_type.lower() for ct in allowed_types):
                logger.warning(f"Invalid content type: {content_type}")
                raise HTTPException(
                    status_code=415,
                    detail=f"Unsupported Media Type. Allowed: {', '.join(allowed_types)}"
                )

        # Validate request size
        content_length = request.headers.get("content-length")
        if content_length and int(content_length) > MAX_BODY_SIZE:
            logger.warning(f"Request too large: {content_length} bytes")
            raise HTTPException(
                status_code=413,
                detail=f"Request body too large. Maximum size: {MAX_BODY_SIZE} bytes"
            )

        # Validate path for traversal attempts
        if self._contains_path_traversal(request.url.path):
            logger.warning(f"Path traversal attempt detected: {request.url.path}")
            raise HTTPException(
                status_code=400,
                detail="Invalid path"
            )

        # Validate query parameters
        for key, value in request.query_params.items():
            if self._contains_sql_injection(value):
                logger.warning(f"SQL injection attempt in query param: {key}={value}")
                raise HTTPException(
                    status_code=400,
                    detail="Invalid query parameter"
                )

            if self._contains_xss(value):
                logger.warning(f"XSS attempt in query param: {key}={value}")
                raise HTTPException(
                    status_code=400,
                    detail="Invalid query parameter"
                )

        # For JSON requests, validate body
        if request.method in ("POST", "PUT", "PATCH"):
            content_type = request.headers.get("content-type", "")
            if "application/json" in content_type:
                try:
                    # Read and validate body
                    body = await request.body()
                    if body:
                        # Parse JSON
                        try:
                            data = json.loads(body)
                            self._validate_json_data(data)
                        except json.JSONDecodeError:
                            logger.warning("Invalid JSON in request body")
                            raise HTTPException(
                                status_code=400,
                                detail="Invalid JSON format"
                            )

                        # Create new request with validated body
                        async def receive():
                            return {"type": "http.request", "body": body}

                        request._receive = receive

                except Exception as e:
                    if isinstance(e, HTTPException):
                        raise
                    logger.error(f"Error validating request body: {e}")
                    raise HTTPException(status_code=400, detail="Invalid request body")

        # Process request
        response = await call_next(request)
        return response

    def _validate_json_data(self, data: Any, path: str = "root") -> None:
        """
        Recursively validate JSON data for dangerous patterns.

        Args:
            data: JSON data to validate
            path: Current path in JSON structure (for error messages)
        """
        if isinstance(data, dict):
            for key, value in data.items():
                # Validate key
                if self._contains_sql_injection(str(key)):
                    logger.warning(f"SQL injection in JSON key at {path}.{key}")
                    raise HTTPException(
                        status_code=400,
                        detail=f"Invalid data at {path}.{key}"
                    )

                # Recursively validate value
                self._validate_json_data(value, f"{path}.{key}")

        elif isinstance(data, list):
            for i, item in enumerate(data):
                self._validate_json_data(item, f"{path}[{i}]")

        elif isinstance(data, str):
            # Validate string values
            if self._contains_sql_injection(data):
                logger.warning(f"SQL injection in JSON value at {path}")
                raise HTTPException(
                    status_code=400,
                    detail=f"Invalid data at {path}"
                )

            if self._contains_xss(data):
                logger.warning(f"XSS attempt in JSON value at {path}")
                raise HTTPException(
                    status_code=400,
                    detail=f"Invalid data at {path}"
                )

    def _contains_sql_injection(self, value: str) -> bool:
        """Check if value contains SQL injection patterns."""
        value_lower = value.lower()
        for pattern in SQL_INJECTION_PATTERNS:
            if re.search(pattern, value_lower, re.IGNORECASE):
                return True
        return False

    def _contains_xss(self, value: str) -> bool:
        """Check if value contains XSS patterns."""
        value_lower = value.lower()
        for pattern in XSS_PATTERNS:
            if re.search(pattern, value_lower, re.IGNORECASE):
                return True
        return False

    def _contains_path_traversal(self, path: str) -> bool:
        """Check if path contains traversal patterns."""
        path_lower = path.lower()
        for pattern in PATH_TRAVERSAL_PATTERNS:
            if re.search(pattern, path_lower, re.IGNORECASE):
                return True
        return False


def sanitize_string(value: str) -> str:
    """
    Sanitize string input by escaping dangerous characters.

    Args:
        value: String to sanitize

    Returns:
        Sanitized string
    """
    # Remove null bytes
    value = value.replace("\x00", "")

    # Escape HTML entities
    value = (
        value.replace("&", "&amp;")
        .replace("<", "&lt;")
        .replace(">", "&gt;")
        .replace('"', "&quot;")
        .replace("'", "&#x27;")
    )

    return value


def validate_email(email: str) -> bool:
    """
    Validate email format.

    Args:
        email: Email address to validate

    Returns:
        True if valid, False otherwise
    """
    pattern = r'^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$'
    return bool(re.match(pattern, email))


def validate_url(url: str) -> bool:
    """
    Validate URL format.

    Args:
        url: URL to validate

    Returns:
        True if valid, False otherwise
    """
    pattern = r'^https?://[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}(/.*)?$'
    return bool(re.match(pattern, url))


def validate_repository_slug(slug: str) -> bool:
    """
    Validate repository slug format (e.g., "owner/repo").

    Args:
        slug: Repository slug to validate

    Returns:
        True if valid, False otherwise
    """
    pattern = r'^[a-zA-Z0-9._-]+/[a-zA-Z0-9._-]+$'
    return bool(re.match(pattern, slug))
