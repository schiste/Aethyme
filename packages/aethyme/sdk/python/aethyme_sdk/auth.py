"""Authentication management for Aethyme SDK."""

from .exceptions import AuthenticationError


class AuthManager:
    """
    Manages authentication for Aethyme API.

    Args:
        token: Bearer credential issued by the trusted identity layer
    """

    def __init__(self, token: str):
        if not token:
            raise AuthenticationError("Bearer token or API key is required")

        self.token = token

    def get_headers(self) -> dict[str, str]:
        """Get authentication headers."""
        return {
            "Authorization": f"Bearer {self.token}",
        }

    def validate(self) -> bool:
        """Validate credentials locally."""
        return bool(self.token)
