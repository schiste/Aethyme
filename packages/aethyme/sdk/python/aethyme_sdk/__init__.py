"""Official Python SDK for the live Aethyme core API."""

from .client import AethymeClient
from .exceptions import AethymeError, APIError, AuthenticationError, NotFoundError, ValidationError
from .models import EgoGraphResult, ImpactAnalysisResult, ScorecardResult, SearchResult

__version__ = "2.0.0"
__all__ = [
    "AethymeClient",
    "SearchResult",
    "EgoGraphResult",
    "ImpactAnalysisResult",
    "ScorecardResult",
    "AethymeError",
    "AuthenticationError",
    "APIError",
    "NotFoundError",
    "ValidationError",
]
