"""
OAuth endpoints for GitHub, GitLab, and Bitbucket integration.
"""

from typing import Optional
from fastapi import APIRouter, Depends, HTTPException, Query, status
from sqlalchemy.ext.asyncio import AsyncSession
from pydantic import BaseModel
import secrets

from app.core.database import get_db
from app.core.deps import get_current_user
from app.models.user import User
from app.models.repository import Repository, RepositoryProvider
from app.services.oauth import oauth_service
from app.schemas.repository import RepositoryResponse

router = APIRouter()


class OAuthStateResponse(BaseModel):
    """OAuth state response."""
    authorization_url: str
    state: str


class OAuthCallbackRequest(BaseModel):
    """OAuth callback request."""
    code: str
    state: str
    provider: str


class OAuthUserInfo(BaseModel):
    """OAuth user information."""
    provider: str
    provider_user_id: str
    username: str
    email: Optional[str] = None
    avatar_url: Optional[str] = None
    access_token: str  # Encrypted


class RepositoryDiscoveryResponse(BaseModel):
    """Repository discovery response."""
    repositories: list[dict]
    total: int


@router.get("/providers")
async def list_oauth_providers():
    """List available OAuth providers."""
    return {
        "providers": oauth_service.get_available_providers(),
        "configured": {
            "github": "github" in oauth_service.providers,
            "gitlab": "gitlab" in oauth_service.providers,
            "bitbucket": "bitbucket" in oauth_service.providers,
        }
    }


@router.get("/{provider}/authorize", response_model=OAuthStateResponse)
async def start_oauth_flow(
    provider: str,
    current_user: User = Depends(get_current_user)
):
    """
    Start OAuth flow for a provider.

    Returns authorization URL and state for CSRF protection.
    """
    try:
        oauth_provider = oauth_service.get_provider(provider)
    except ValueError as e:
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail=str(e)
        )

    # Generate state for CSRF protection
    state = secrets.token_urlsafe(32)

    # TODO: Store state in Redis with user_id for validation
    # await redis.setex(f"oauth_state:{state}", 600, current_user.id)

    authorization_url = oauth_provider.get_authorization_url(state)

    return OAuthStateResponse(
        authorization_url=authorization_url,
        state=state
    )


@router.post("/callback", response_model=OAuthUserInfo)
async def oauth_callback(
    request: OAuthCallbackRequest,
    db: AsyncSession = Depends(get_db),
    current_user: User = Depends(get_current_user)
):
    """
    Handle OAuth callback.

    Exchanges code for access token and returns user info.
    """
    # TODO: Validate state from Redis
    # stored_user_id = await redis.get(f"oauth_state:{request.state}")
    # if not stored_user_id or stored_user_id != current_user.id:
    #     raise HTTPException(status_code=400, detail="Invalid state")

    try:
        oauth_provider = oauth_service.get_provider(request.provider)
    except ValueError as e:
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail=str(e)
        )

    # Exchange code for token
    try:
        token_data = await oauth_provider.exchange_code_for_token(request.code)
        access_token = token_data.get("access_token")

        if not access_token:
            raise HTTPException(
                status_code=status.HTTP_400_BAD_REQUEST,
                detail="Failed to obtain access token"
            )

        # Get user info from provider
        user_info = await oauth_provider.get_user_info(access_token)

        # Encrypt token for storage
        encrypted_token = oauth_provider.encrypt_token(access_token)

        # Store OAuth connection in user record
        if request.provider == "github":
            current_user.github_id = user_info.get("id")
            current_user.github_username = user_info.get("login")
            current_user.github_access_token = encrypted_token
        elif request.provider == "gitlab":
            current_user.gitlab_id = user_info.get("id")
            current_user.gitlab_username = user_info.get("username")
            current_user.gitlab_access_token = encrypted_token
        elif request.provider == "bitbucket":
            current_user.bitbucket_id = user_info.get("uuid")
            current_user.bitbucket_username = user_info.get("username")
            current_user.bitbucket_access_token = encrypted_token

        await db.commit()

        return OAuthUserInfo(
            provider=request.provider,
            provider_user_id=str(user_info.get("id") or user_info.get("uuid")),
            username=user_info.get("login") or user_info.get("username"),
            email=user_info.get("email"),
            avatar_url=user_info.get("avatar_url"),
            access_token="[ENCRYPTED]"  # Don't return actual token
        )

    except Exception as e:
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail=f"OAuth callback failed: {str(e)}"
        )


@router.get("/{provider}/repositories", response_model=RepositoryDiscoveryResponse)
async def discover_repositories(
    provider: str,
    current_user: User = Depends(get_current_user),
    db: AsyncSession = Depends(get_db)
):
    """
    Discover repositories from connected OAuth provider.

    Returns list of repositories that can be connected.
    """
    try:
        oauth_provider = oauth_service.get_provider(provider)
    except ValueError as e:
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail=str(e)
        )

    # Get encrypted token from user
    encrypted_token = None
    if provider == "github":
        encrypted_token = current_user.github_access_token
    elif provider == "gitlab":
        encrypted_token = current_user.gitlab_access_token
    elif provider == "bitbucket":
        encrypted_token = current_user.bitbucket_access_token

    if not encrypted_token:
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail=f"{provider.title()} not connected. Please authorize first."
        )

    # Decrypt token
    try:
        access_token = oauth_provider.decrypt_token(encrypted_token)
    except Exception:
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail="Invalid or expired token. Please re-authorize."
        )

    # Fetch repositories from provider
    try:
        repos_data = await oauth_provider.list_repositories(access_token)

        # Transform to common format
        repositories = []
        for repo in repos_data:
            if provider == "github":
                repositories.append({
                    "id": repo["id"],
                    "name": repo["name"],
                    "full_name": repo["full_name"],
                    "description": repo.get("description"),
                    "private": repo["private"],
                    "language": repo.get("language"),
                    "clone_url": repo["clone_url"],
                    "git_url": repo["git_url"],
                    "default_branch": repo.get("default_branch", "main"),
                    "updated_at": repo["updated_at"],
                })
            elif provider == "gitlab":
                repositories.append({
                    "id": repo["id"],
                    "name": repo["name"],
                    "full_name": repo["path_with_namespace"],
                    "description": repo.get("description"),
                    "private": repo.get("visibility") != "public",
                    "language": None,  # GitLab doesn't provide primary language easily
                    "clone_url": repo["http_url_to_repo"],
                    "git_url": repo["ssh_url_to_repo"],
                    "default_branch": repo.get("default_branch", "main"),
                    "updated_at": repo["last_activity_at"],
                })
            elif provider == "bitbucket":
                repositories.append({
                    "id": repo["uuid"],
                    "name": repo["name"],
                    "full_name": repo["full_name"],
                    "description": repo.get("description"),
                    "private": repo.get("is_private", True),
                    "language": repo.get("language"),
                    "clone_url": repo["links"]["clone"][0]["href"],  # HTTPS
                    "git_url": repo["links"]["clone"][1]["href"],  # SSH
                    "default_branch": repo.get("mainbranch", {}).get("name", "main"),
                    "updated_at": repo["updated_on"],
                })

        return RepositoryDiscoveryResponse(
            repositories=repositories,
            total=len(repositories)
        )

    except Exception as e:
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail=f"Failed to fetch repositories: {str(e)}"
        )


@router.post("/{provider}/repositories/{repo_id}/connect", response_model=RepositoryResponse)
async def connect_repository(
    provider: str,
    repo_id: str,
    current_user: User = Depends(get_current_user),
    db: AsyncSession = Depends(get_db)
):
    """
    Connect a repository from OAuth provider.

    Creates repository record and sets up webhook.
    """
    # First, discover the specific repository
    discovery = await discover_repositories(provider, current_user, db)

    # Find the repo in discovered list
    repo_data = next(
        (r for r in discovery.repositories if str(r["id"]) == repo_id),
        None
    )

    if not repo_data:
        raise HTTPException(
            status_code=status.HTTP_404_NOT_FOUND,
            detail="Repository not found"
        )

    # Check if already connected
    existing = await db.get(Repository, {"git_url": repo_data["git_url"]})
    if existing:
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail="Repository already connected"
        )

    # Create repository record
    repository = Repository(
        organization_id=current_user.organization_id,
        name=repo_data["name"],
        full_name=repo_data["full_name"],
        git_url=repo_data["git_url"],
        clone_url=repo_data["clone_url"],
        provider=RepositoryProvider[provider.upper()],
        provider_id=str(repo_data["id"]),
        default_branch=repo_data["default_branch"],
        description=repo_data.get("description"),
        private=repo_data["private"],
        language=repo_data.get("language"),
        status="pending",
    )

    db.add(repository)
    await db.commit()
    await db.refresh(repository)

    # TODO: Set up webhook
    # TODO: Trigger indexing job

    return RepositoryResponse.model_validate(repository)


@router.delete("/{provider}/disconnect")
async def disconnect_oauth_provider(
    provider: str,
    current_user: User = Depends(get_current_user),
    db: AsyncSession = Depends(get_db)
):
    """
    Disconnect OAuth provider from user account.

    Removes access token and provider connection.
    """
    if provider == "github":
        current_user.github_id = None
        current_user.github_username = None
        current_user.github_access_token = None
    elif provider == "gitlab":
        current_user.gitlab_id = None
        current_user.gitlab_username = None
        current_user.gitlab_access_token = None
    elif provider == "bitbucket":
        current_user.bitbucket_id = None
        current_user.bitbucket_username = None
        current_user.bitbucket_access_token = None
    else:
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail=f"Unknown provider: {provider}"
        )

    await db.commit()

    return {"message": f"{provider.title()} disconnected successfully"}
