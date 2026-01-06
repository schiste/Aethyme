"""GitHub API client for OAuth and repository operations."""

import httpx
import secrets
from typing import Dict, List, Optional, Any
from datetime import datetime, timedelta

from app.core.config import settings


class GitHubOAuthClient:
    """
    GitHub OAuth client for handling authorization flow.

    Handles:
    - Authorization URL generation
    - Token exchange
    - Token refresh
    - User profile fetching
    """

    BASE_URL = "https://api.github.com"
    OAUTH_AUTHORIZE_URL = "https://github.com/login/oauth/authorize"
    OAUTH_TOKEN_URL = "https://github.com/login/oauth/access_token"

    def __init__(self):
        """Initialize GitHub OAuth client with settings."""
        self.client_id = settings.GITHUB_CLIENT_ID
        self.client_secret = settings.GITHUB_CLIENT_SECRET
        self.redirect_uri = settings.GITHUB_REDIRECT_URI

    def generate_state_token(self) -> str:
        """
        Generate a cryptographically secure state token for CSRF protection.

        Returns:
            str: 32-character random hex string
        """
        return secrets.token_hex(32)

    def get_authorization_url(self, state: str, scopes: Optional[List[str]] = None) -> str:
        """
        Generate GitHub OAuth authorization URL.

        Args:
            state: CSRF protection token
            scopes: List of OAuth scopes to request (default: repo, read:user, user:email)

        Returns:
            str: Full authorization URL to redirect user to

        Example:
            >>> client = GitHubOAuthClient()
            >>> state = client.generate_state_token()
            >>> url = client.get_authorization_url(state)
            >>> print(url)
            'https://github.com/login/oauth/authorize?client_id=...&redirect_uri=...&state=...&scope=repo%20read:user'
        """
        if scopes is None:
            scopes = ["repo", "read:user", "user:email"]

        params = {
            "client_id": self.client_id,
            "redirect_uri": self.redirect_uri,
            "state": state,
            "scope": " ".join(scopes),
            "allow_signup": "true",
        }

        query_string = "&".join(f"{k}={v}" for k, v in params.items())
        return f"{self.OAUTH_AUTHORIZE_URL}?{query_string}"

    async def exchange_code_for_token(self, code: str) -> Dict[str, Any]:
        """
        Exchange authorization code for access token.

        Args:
            code: Authorization code from OAuth callback

        Returns:
            dict: Token response with access_token, token_type, scope

        Raises:
            httpx.HTTPStatusError: If token exchange fails

        Example:
            >>> client = GitHubOAuthClient()
            >>> token_data = await client.exchange_code_for_token("abc123")
            >>> print(token_data['access_token'])
            'gho_...'
        """
        async with httpx.AsyncClient() as client:
            response = await client.post(
                self.OAUTH_TOKEN_URL,
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

    async def get_user_profile(self, access_token: str) -> Dict[str, Any]:
        """
        Fetch GitHub user profile using access token.

        Args:
            access_token: GitHub OAuth access token

        Returns:
            dict: User profile data (id, login, email, avatar_url, etc.)

        Raises:
            httpx.HTTPStatusError: If API request fails

        Example:
            >>> client = GitHubOAuthClient()
            >>> profile = await client.get_user_profile("gho_...")
            >>> print(profile['login'])
            'octocat'
        """
        async with httpx.AsyncClient() as client:
            response = await client.get(
                f"{self.BASE_URL}/user",
                headers={
                    "Authorization": f"Bearer {access_token}",
                    "Accept": "application/vnd.github+json",
                    "X-GitHub-Api-Version": "2022-11-28",
                },
            )
            response.raise_for_status()
            return response.json()

    async def get_user_emails(self, access_token: str) -> List[Dict[str, Any]]:
        """
        Fetch user's email addresses from GitHub.

        Args:
            access_token: GitHub OAuth access token

        Returns:
            list: List of email objects with email, primary, verified fields

        Example:
            >>> client = GitHubOAuthClient()
            >>> emails = await client.get_user_emails("gho_...")
            >>> primary = next(e for e in emails if e['primary'])
            >>> print(primary['email'])
            'user@example.com'
        """
        async with httpx.AsyncClient() as client:
            response = await client.get(
                f"{self.BASE_URL}/user/emails",
                headers={
                    "Authorization": f"Bearer {access_token}",
                    "Accept": "application/vnd.github+json",
                    "X-GitHub-Api-Version": "2022-11-28",
                },
            )
            response.raise_for_status()
            return response.json()

    async def get_user_repositories(
        self,
        access_token: str,
        visibility: str = "all",
        affiliation: str = "owner,collaborator,organization_member",
        per_page: int = 100,
    ) -> List[Dict[str, Any]]:
        """
        Fetch user's repositories from GitHub.

        Args:
            access_token: GitHub OAuth access token
            visibility: Filter by visibility (all, public, private)
            affiliation: Filter by affiliation (owner, collaborator, organization_member)
            per_page: Number of results per page (max 100)

        Returns:
            list: List of repository objects

        Example:
            >>> client = GitHubOAuthClient()
            >>> repos = await client.get_user_repositories("gho_...")
            >>> for repo in repos:
            ...     print(f"{repo['full_name']} - {repo['html_url']}")
        """
        async with httpx.AsyncClient() as client:
            response = await client.get(
                f"{self.BASE_URL}/user/repos",
                params={
                    "visibility": visibility,
                    "affiliation": affiliation,
                    "per_page": per_page,
                    "sort": "updated",
                },
                headers={
                    "Authorization": f"Bearer {access_token}",
                    "Accept": "application/vnd.github+json",
                    "X-GitHub-Api-Version": "2022-11-28",
                },
            )
            response.raise_for_status()
            return response.json()

    async def revoke_token(self, access_token: str) -> bool:
        """
        Revoke a GitHub OAuth access token.

        Args:
            access_token: GitHub OAuth access token to revoke

        Returns:
            bool: True if revocation successful

        Example:
            >>> client = GitHubOAuthClient()
            >>> success = await client.revoke_token("gho_...")
            >>> print(success)
            True
        """
        async with httpx.AsyncClient() as client:
            response = await client.delete(
                f"{self.BASE_URL}/applications/{self.client_id}/token",
                headers={
                    "Accept": "application/vnd.github+json",
                    "X-GitHub-Api-Version": "2022-11-28",
                },
                auth=(self.client_id, self.client_secret),
                json={"access_token": access_token},
            )
            # GitHub returns 204 No Content on success
            return response.status_code == 204


class GitHubAPIClient:
    """
    GitHub API client for repository operations.

    Handles:
    - Repository content fetching
    - Commit information
    - File tree traversal
    """

    BASE_URL = "https://api.github.com"

    def __init__(self, access_token: str):
        """
        Initialize GitHub API client.

        Args:
            access_token: GitHub OAuth access token
        """
        self.access_token = access_token
        self.headers = {
            "Authorization": f"Bearer {access_token}",
            "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
        }

    async def get_repository(self, owner: str, repo: str) -> Dict[str, Any]:
        """
        Fetch repository information.

        Args:
            owner: Repository owner (username or organization)
            repo: Repository name

        Returns:
            dict: Repository data

        Example:
            >>> client = GitHubAPIClient("gho_...")
            >>> repo = await client.get_repository("octocat", "Hello-World")
            >>> print(repo['description'])
        """
        async with httpx.AsyncClient() as client:
            response = await client.get(
                f"{self.BASE_URL}/repos/{owner}/{repo}",
                headers=self.headers,
            )
            response.raise_for_status()
            return response.json()

    async def get_tree(
        self, owner: str, repo: str, tree_sha: str = "HEAD", recursive: bool = True
    ) -> Dict[str, Any]:
        """
        Fetch repository file tree.

        Args:
            owner: Repository owner
            repo: Repository name
            tree_sha: Tree SHA or branch name (default: HEAD)
            recursive: Fetch recursively (default: True)

        Returns:
            dict: Tree data with file list

        Example:
            >>> client = GitHubAPIClient("gho_...")
            >>> tree = await client.get_tree("octocat", "Hello-World", recursive=True)
            >>> for file in tree['tree']:
            ...     print(f"{file['path']} ({file['type']})")
        """
        params = {"recursive": "1" if recursive else "0"}
        async with httpx.AsyncClient() as client:
            response = await client.get(
                f"{self.BASE_URL}/repos/{owner}/{repo}/git/trees/{tree_sha}",
                headers=self.headers,
                params=params,
            )
            response.raise_for_status()
            return response.json()

    async def get_file_content(
        self, owner: str, repo: str, path: str, ref: str = "HEAD"
    ) -> Dict[str, Any]:
        """
        Fetch file content from repository.

        Args:
            owner: Repository owner
            repo: Repository name
            path: File path in repository
            ref: Branch/tag/commit reference (default: HEAD)

        Returns:
            dict: File content data (base64 encoded)

        Example:
            >>> client = GitHubAPIClient("gho_...")
            >>> file = await client.get_file_content("octocat", "Hello-World", "README.md")
            >>> import base64
            >>> content = base64.b64decode(file['content']).decode('utf-8')
            >>> print(content)
        """
        async with httpx.AsyncClient() as client:
            response = await client.get(
                f"{self.BASE_URL}/repos/{owner}/{repo}/contents/{path}",
                headers=self.headers,
                params={"ref": ref},
            )
            response.raise_for_status()
            return response.json()
