"""RepoGraph API client."""

from typing import Optional, Dict, Any, List
import httpx
from .auth import AuthManager
from .query import QueryAPI
from .scorecard import ScorecardAPI
from .autofix import AutofixAPI
from .telemetry import TelemetryAPI
from .guardrails import GuardrailsAPI
from .exceptions import APIError


class RepoGraphClient:
    """
    RepoGraph API client.

    Args:
        api_key: RepoGraph API key
        org_id: Organization ID
        base_url: Base URL for API (defaults to production)
        timeout: Request timeout in seconds (default: 30)

    Example:
        >>> client = RepoGraphClient(api_key="...", org_id="...")
        >>> results = client.query.search("UserService")
        >>> scorecard = client.scorecard.scan(repo_id="abc123")
    """

    def __init__(
        self,
        api_key: str,
        org_id: str,
        base_url: str = "https://api.repograph.ai/v1",
        timeout: float = 30.0,
    ):
        self.base_url = base_url.rstrip("/")
        self.timeout = timeout

        # Initialize auth manager
        self.auth = AuthManager(api_key, org_id)

        # Initialize HTTP client
        self._client = httpx.Client(
            base_url=self.base_url,
            timeout=timeout,
            headers={
                "Authorization": f"Bearer {api_key}",
                "X-Organization-ID": org_id,
                "Content-Type": "application/json",
                "User-Agent": "repograph-python-sdk/2.0.0",
            },
        )

        # Initialize API modules
        self.query = QueryAPI(self)
        self.scorecard = ScorecardAPI(self)
        self.autofix = AutofixAPI(self)
        self.telemetry = TelemetryAPI(self)
        self.guardrails = GuardrailsAPI(self)

    def request(
        self,
        method: str,
        path: str,
        **kwargs: Any,
    ) -> Dict[str, Any]:
        """
        Make an API request.

        Args:
            method: HTTP method (GET, POST, etc.)
            path: API path (e.g., "/search")
            **kwargs: Additional arguments for httpx request

        Returns:
            Response data as dictionary

        Raises:
            APIError: If request fails
        """
        url = f"{self.base_url}{path}"

        try:
            response = self._client.request(method, url, **kwargs)
            response.raise_for_status()
            return response.json()
        except httpx.HTTPStatusError as e:
            raise APIError(
                f"API request failed: {e.response.status_code}",
                status_code=e.response.status_code,
                response=e.response.json() if e.response.text else None,
            )
        except httpx.RequestError as e:
            raise APIError(f"Request error: {str(e)}")

    def get(self, path: str, **kwargs: Any) -> Dict[str, Any]:
        """Make a GET request."""
        return self.request("GET", path, **kwargs)

    def post(self, path: str, **kwargs: Any) -> Dict[str, Any]:
        """Make a POST request."""
        return self.request("POST", path, **kwargs)

    def put(self, path: str, **kwargs: Any) -> Dict[str, Any]:
        """Make a PUT request."""
        return self.request("PUT", path, **kwargs)

    def delete(self, path: str, **kwargs: Any) -> Dict[str, Any]:
        """Make a DELETE request."""
        return self.request("DELETE", path, **kwargs)

    def get_version(self) -> Dict[str, Any]:
        """Get API version information."""
        return self.get("/version")

    def get_health(self) -> Dict[str, Any]:
        """Get API health status."""
        return self.get("/health")

    def get_status(self) -> Dict[str, Any]:
        """Get overall system status."""
        return self.get("/status")

    def close(self):
        """Close the HTTP client."""
        self._client.close()

    def __enter__(self):
        """Context manager entry."""
        return self

    def __exit__(self, *args):
        """Context manager exit."""
        self.close()
