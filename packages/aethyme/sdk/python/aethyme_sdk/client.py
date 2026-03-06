"""Aethyme API client."""

from typing import Any

import httpx

from .auth import AuthManager
from .exceptions import APIError
from .query import QueryAPI
from .scorecard import ScorecardAPI


class AethymeClient:
    """Thin client for the live Aethyme core API."""

    def __init__(
        self,
        api_key: str,
        org_id: str,
        base_url: str = "https://api.aethyme.com",
        timeout: float = 30.0,
    ):
        self.base_url = base_url.rstrip("/")
        self.timeout = timeout
        self.auth = AuthManager(api_key, org_id)
        self._client = httpx.Client(
            base_url=self.base_url,
            timeout=timeout,
            headers={
                "Authorization": f"Bearer {api_key}",
                "X-Organization-ID": org_id,
                "Content-Type": "application/json",
                "User-Agent": "aethyme-python-sdk/2.0.0",
            },
        )

        self.query = QueryAPI(self)
        self.scorecard = ScorecardAPI(self)

    def request(self, method: str, path: str, **kwargs: Any) -> dict[str, Any]:
        """Make an API request and return decoded JSON."""
        try:
            response = self._client.request(method, path, **kwargs)
            response.raise_for_status()
            return response.json()
        except httpx.HTTPStatusError as exc:
            raise APIError(
                f"API request failed: {exc.response.status_code}",
                status_code=exc.response.status_code,
                response=exc.response.json() if exc.response.text else None,
            ) from exc
        except httpx.RequestError as exc:
            raise APIError(f"Request error: {exc}") from exc

    def get(self, path: str, **kwargs: Any) -> dict[str, Any]:
        return self.request("GET", path, **kwargs)

    def post(self, path: str, **kwargs: Any) -> dict[str, Any]:
        return self.request("POST", path, **kwargs)

    def get_health(self) -> dict[str, Any]:
        return self.get("/health")

    def get_info(self) -> dict[str, Any]:
        return self.get("/api/v1/info")

    def close(self) -> None:
        self._client.close()

    def __enter__(self) -> "AethymeClient":
        return self

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        traceback: Any,
    ) -> None:
        del exc_type, exc, traceback
        self.close()
