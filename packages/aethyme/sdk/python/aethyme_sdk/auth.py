"""Authentication management for Aethyme SDK."""

from typing import Optional
from .exceptions import AuthenticationError


class AuthManager:
    """
    Manages authentication for Aethyme API.

    Args:
        api_key: Aethyme API key
        org_id: Organization ID
    """

    def __init__(self, api_key: str, org_id: str):
        if not api_key:
            raise AuthenticationError("API key is required")
        if not org_id:
            raise AuthenticationError("Organization ID is required")

        self.api_key = api_key
        self.org_id = org_id

    def get_headers(self) -> dict:
        """Get authentication headers."""
        return {
            "Authorization": f"Bearer {self.api_key}",
            "X-Organization-ID": self.org_id,
        }

    def validate(self) -> bool:
        """Validate credentials (placeholder for actual validation)."""
        # In a real implementation, this would make an API call to validate
        return bool(self.api_key and self.org_id)
