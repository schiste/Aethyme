"""
RepoGraph Python SDK

Official Python SDK for RepoGraph API.

Example usage:
    >>> from repograph_sdk import RepoGraphClient
    >>> client = RepoGraphClient(api_key="your-api-key", org_id="your-org")
    >>> results = client.query.search("UserService")
    >>> scorecard = client.scorecard.scan(repo_id="abc123")
"""

from .client import RepoGraphClient
from .models import (
    SearchResult,
    EgoGraphResult,
    ImpactAnalysisResult,
    ScorecardResult,
    AutofixResult,
)
from .exceptions import (
    RepoGraphError,
    AuthenticationError,
    APIError,
    NotFoundError,
    ValidationError,
)

__version__ = "2.0.0"
__all__ = [
    "RepoGraphClient",
    "SearchResult",
    "EgoGraphResult",
    "ImpactAnalysisResult",
    "ScorecardResult",
    "AutofixResult",
    "RepoGraphError",
    "AuthenticationError",
    "APIError",
    "NotFoundError",
    "ValidationError",
]
