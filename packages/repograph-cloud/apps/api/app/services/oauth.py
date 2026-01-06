"""
OAuth Service for GitHub, GitLab, and Bitbucket integration.

Handles OAuth flows, token exchange, and API client creation.
"""

from typing import Optional, Dict, Any
from urllib.parse import urlencode
import httpx
from cryptography.fernet import Fernet

from app.core.config import settings
from app.models.user import User
from app.models.repository import Repository


class OAuthProvider:
    """Base OAuth provider."""

    def __init__(self, client_id: str, client_secret: str, redirect_uri: str):
        self.client_id = client_id
        self.client_secret = client_secret
        self.redirect_uri = redirect_uri
        self.fernet = Fernet(settings.ENCRYPTION_KEY.encode())

    def get_authorization_url(self, state: str) -> str:
        """Generate OAuth authorization URL."""
        raise NotImplementedError

    async def exchange_code_for_token(self, code: str) -> Dict[str, Any]:
        """Exchange authorization code for access token."""
        raise NotImplementedError

    async def get_user_info(self, access_token: str) -> Dict[str, Any]:
        """Get user information from provider."""
        raise NotImplementedError

    async def list_repositories(self, access_token: str) -> list[Dict[str, Any]]:
        """List user's repositories."""
        raise NotImplementedError

    def encrypt_token(self, token: str) -> str:
        """Encrypt access token for storage."""
        return self.fernet.encrypt(token.encode()).decode()

    def decrypt_token(self, encrypted_token: str) -> str:
        """Decrypt access token."""
        return self.fernet.decrypt(encrypted_token.encode()).decode()


class GitHubOAuthProvider(OAuthProvider):
    """GitHub OAuth provider."""

    AUTHORIZE_URL = "https://github.com/login/oauth/authorize"
    TOKEN_URL = "https://github.com/login/oauth/access_token"
    API_BASE_URL = "https://api.github.com"

    def get_authorization_url(self, state: str, scope: str = "repo,read:user,user:email") -> str:
        """Generate GitHub authorization URL."""
        params = {
            "client_id": self.client_id,
            "redirect_uri": self.redirect_uri,
            "scope": scope,
            "state": state,
        }
        return f"{self.AUTHORIZE_URL}?{urlencode(params)}"

    async def exchange_code_for_token(self, code: str) -> Dict[str, Any]:
        """Exchange authorization code for GitHub access token."""
        async with httpx.AsyncClient() as client:
            response = await client.post(
                self.TOKEN_URL,
                headers={"Accept": "application/json"},
                data={
                    "client_id": self.client_id,
                    "client_secret": self.client_secret,
                    "code": code,
                    "redirect_uri": self.redirect_uri,
                },
            )
            response.raise_for_status()
            return response.json()

    async def get_user_info(self, access_token: str) -> Dict[str, Any]:
        """Get GitHub user information."""
        async with httpx.AsyncClient() as client:
            response = await client.get(
                f"{self.API_BASE_URL}/user",
                headers={
                    "Authorization": f"Bearer {access_token}",
                    "Accept": "application/vnd.github.v3+json",
                },
            )
            response.raise_for_status()
            return response.json()

    async def list_repositories(
        self,
        access_token: str,
        visibility: str = "all",  # all, public, private
        per_page: int = 100
    ) -> list[Dict[str, Any]]:
        """List user's GitHub repositories."""
        repos = []
        page = 1

        async with httpx.AsyncClient() as client:
            while True:
                response = await client.get(
                    f"{self.API_BASE_URL}/user/repos",
                    headers={
                        "Authorization": f"Bearer {access_token}",
                        "Accept": "application/vnd.github.v3+json",
                    },
                    params={
                        "visibility": visibility,
                        "per_page": per_page,
                        "page": page,
                        "sort": "updated",
                    },
                )
                response.raise_for_status()

                batch = response.json()
                if not batch:
                    break

                repos.extend(batch)
                page += 1

                # Stop if we've fetched all repos
                if len(batch) < per_page:
                    break

        return repos

    async def create_webhook(
        self,
        access_token: str,
        repo_full_name: str,
        webhook_url: str,
        secret: str
    ) -> Dict[str, Any]:
        """Create webhook for repository."""
        async with httpx.AsyncClient() as client:
            response = await client.post(
                f"{self.API_BASE_URL}/repos/{repo_full_name}/hooks",
                headers={
                    "Authorization": f"Bearer {access_token}",
                    "Accept": "application/vnd.github.v3+json",
                },
                json={
                    "name": "web",
                    "active": True,
                    "events": ["push", "pull_request"],
                    "config": {
                        "url": webhook_url,
                        "content_type": "json",
                        "secret": secret,
                        "insecure_ssl": "0",
                    },
                },
            )
            response.raise_for_status()
            return response.json()


class GitLabOAuthProvider(OAuthProvider):
    """GitLab OAuth provider."""

    AUTHORIZE_URL = "https://gitlab.com/oauth/authorize"
    TOKEN_URL = "https://gitlab.com/oauth/token"
    API_BASE_URL = "https://gitlab.com/api/v4"

    def get_authorization_url(self, state: str, scope: str = "read_user,read_api,read_repository") -> str:
        """Generate GitLab authorization URL."""
        params = {
            "client_id": self.client_id,
            "redirect_uri": self.redirect_uri,
            "response_type": "code",
            "scope": scope,
            "state": state,
        }
        return f"{self.AUTHORIZE_URL}?{urlencode(params)}"

    async def exchange_code_for_token(self, code: str) -> Dict[str, Any]:
        """Exchange authorization code for GitLab access token."""
        async with httpx.AsyncClient() as client:
            response = await client.post(
                self.TOKEN_URL,
                data={
                    "client_id": self.client_id,
                    "client_secret": self.client_secret,
                    "code": code,
                    "grant_type": "authorization_code",
                    "redirect_uri": self.redirect_uri,
                },
            )
            response.raise_for_status()
            return response.json()

    async def get_user_info(self, access_token: str) -> Dict[str, Any]:
        """Get GitLab user information."""
        async with httpx.AsyncClient() as client:
            response = await client.get(
                f"{self.API_BASE_URL}/user",
                headers={"Authorization": f"Bearer {access_token}"},
            )
            response.raise_for_status()
            return response.json()

    async def list_repositories(self, access_token: str, per_page: int = 100) -> list[Dict[str, Any]]:
        """List user's GitLab projects."""
        projects = []
        page = 1

        async with httpx.AsyncClient() as client:
            while True:
                response = await client.get(
                    f"{self.API_BASE_URL}/projects",
                    headers={"Authorization": f"Bearer {access_token}"},
                    params={
                        "membership": True,
                        "per_page": per_page,
                        "page": page,
                        "order_by": "updated_at",
                    },
                )
                response.raise_for_status()

                batch = response.json()
                if not batch:
                    break

                projects.extend(batch)
                page += 1

                if len(batch) < per_page:
                    break

        return projects


class BitbucketOAuthProvider(OAuthProvider):
    """Bitbucket OAuth provider."""

    AUTHORIZE_URL = "https://bitbucket.org/site/oauth2/authorize"
    TOKEN_URL = "https://bitbucket.org/site/oauth2/access_token"
    API_BASE_URL = "https://api.bitbucket.org/2.0"

    def get_authorization_url(self, state: str, scope: str = "repository,account") -> str:
        """Generate Bitbucket authorization URL."""
        params = {
            "client_id": self.client_id,
            "response_type": "code",
            "state": state,
            "scope": scope,
        }
        return f"{self.AUTHORIZE_URL}?{urlencode(params)}"

    async def exchange_code_for_token(self, code: str) -> Dict[str, Any]:
        """Exchange authorization code for Bitbucket access token."""
        async with httpx.AsyncClient() as client:
            response = await client.post(
                self.TOKEN_URL,
                auth=(self.client_id, self.client_secret),
                data={
                    "grant_type": "authorization_code",
                    "code": code,
                },
            )
            response.raise_for_status()
            return response.json()

    async def get_user_info(self, access_token: str) -> Dict[str, Any]:
        """Get Bitbucket user information."""
        async with httpx.AsyncClient() as client:
            response = await client.get(
                f"{self.API_BASE_URL}/user",
                headers={"Authorization": f"Bearer {access_token}"},
            )
            response.raise_for_status()
            return response.json()

    async def list_repositories(self, access_token: str) -> list[Dict[str, Any]]:
        """List user's Bitbucket repositories."""
        repos = []
        url = f"{self.API_BASE_URL}/repositories"

        async with httpx.AsyncClient() as client:
            while url:
                response = await client.get(
                    url,
                    headers={"Authorization": f"Bearer {access_token}"},
                    params={"role": "member", "pagelen": 100},
                )
                response.raise_for_status()

                data = response.json()
                repos.extend(data.get("values", []))
                url = data.get("next")  # Bitbucket uses cursor pagination

        return repos


class OAuthService:
    """OAuth service for managing providers."""

    def __init__(self):
        self.providers: Dict[str, OAuthProvider] = {}

        # Initialize GitHub
        if settings.GITHUB_CLIENT_ID and settings.GITHUB_CLIENT_SECRET:
            self.providers["github"] = GitHubOAuthProvider(
                client_id=settings.GITHUB_CLIENT_ID,
                client_secret=settings.GITHUB_CLIENT_SECRET,
                redirect_uri=settings.GITHUB_REDIRECT_URI or "http://localhost:3000/auth/callback/github",
            )

        # Initialize GitLab
        if settings.GITLAB_CLIENT_ID and settings.GITLAB_CLIENT_SECRET:
            self.providers["gitlab"] = GitLabOAuthProvider(
                client_id=settings.GITLAB_CLIENT_ID,
                client_secret=settings.GITLAB_CLIENT_SECRET,
                redirect_uri=settings.GITLAB_REDIRECT_URI or "http://localhost:3000/auth/callback/gitlab",
            )

        # Initialize Bitbucket
        if settings.BITBUCKET_CLIENT_ID and settings.BITBUCKET_CLIENT_SECRET:
            self.providers["bitbucket"] = BitbucketOAuthProvider(
                client_id=settings.BITBUCKET_CLIENT_ID,
                client_secret=settings.BITBUCKET_CLIENT_SECRET,
                redirect_uri=settings.BITBUCKET_REDIRECT_URI or "http://localhost:3000/auth/callback/bitbucket",
            )

    def get_provider(self, provider_name: str) -> OAuthProvider:
        """Get OAuth provider by name."""
        if provider_name not in self.providers:
            raise ValueError(f"OAuth provider '{provider_name}' not configured")
        return self.providers[provider_name]

    def get_available_providers(self) -> list[str]:
        """Get list of available OAuth providers."""
        return list(self.providers.keys())


# Global OAuth service instance
oauth_service = OAuthService()
