"""
Aethyme Python SDK

Official Python SDK for Aethyme API.

Example usage:
    >>> from aethyme_sdk import AethymeClient
    >>> client = AethymeClient(api_key="your-api-key", org_id="your-org")
    >>> results = client.query.search("UserService")
    >>> scorecard = client.scorecard.scan(repo_id="abc123")
"""

from .client import AethymeClient
from .models import (
    SearchResult,
    EgoGraphResult,
    ImpactAnalysisResult,
    ScorecardResult,
    AutofixResult,
)
from .exceptions import (
    AethymeError,
    AuthenticationError,
    APIError,
    NotFoundError,
    ValidationError,
)

__version__ = "2.0.0"
__all__ = [
    "AethymeClient",
    "SearchResult",
    "EgoGraphResult",
    "ImpactAnalysisResult",
    "ScorecardResult",
    "AutofixResult",
    "AethymeError",
    "AuthenticationError",
    "APIError",
    "NotFoundError",
    "ValidationError",
]
