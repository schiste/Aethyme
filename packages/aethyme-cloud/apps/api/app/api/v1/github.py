"""GitHub OAuth and integration endpoints."""

import uuid
from fastapi import APIRouter, Depends, HTTPException, status
from sqlalchemy.ext.asyncio import AsyncSession
from sqlalchemy import select
from datetime import datetime

from app.core.database import get_db
from app.core.deps import get_current_user
from app.core.github import GitHubOAuthClient
from app.core.encryption import encrypt_token, decrypt_token
from app.models.user import User
from app.models.github_account import GitHubAccount
from app.schemas.github import (
    GitHubAuthorizationURL,
    GitHubConnectionStatus,
    GitHubRepositoryListResponse,
    GitHubRepositoryInfo,
)

router = APIRouter()

# Redis client for state token storage (would need to be initialized)
# For now, we'll use in-memory dict for development
_state_tokens = {}


@router.get("/authorize", response_model=GitHubAuthorizationURL)
async def get_github_authorization_url(
    current_user: User = Depends(get_current_user),
):
    """
    Generate GitHub OAuth authorization URL.

    Returns a URL to redirect the user to GitHub for authorization.
    The state token is stored temporarily for CSRF protection.
    """
    oauth_client = GitHubOAuthClient()

    # Generate state token for CSRF protection
    state = oauth_client.generate_state_token()

    # Store state token with user ID (in production, use Redis with TTL)
    _state_tokens[state] = {
        "user_id": current_user.id,
        "created_at": datetime.utcnow(),
    }

    # Generate authorization URL
    authorization_url = oauth_client.get_authorization_url(
        state=state,
        scopes=["repo", "read:user", "user:email"],
    )

    return GitHubAuthorizationURL(
        authorization_url=authorization_url,
        state=state,
    )


@router.get("/callback")
async def github_oauth_callback(
    code: str,
    state: str,
    db: AsyncSession = Depends(get_db),
):
    """
    Handle GitHub OAuth callback.

    Exchanges the authorization code for an access token,
    fetches user profile, and creates/updates GitHub account record.

    Returns JWT tokens for authentication.
    """
    # Verify state token for CSRF protection
    if state not in _state_tokens:
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail="Invalid or expired state token",
        )

    state_data = _state_tokens.pop(state)  # Remove after use

    # Check if state token is expired (10 minutes)
    time_diff = (datetime.utcnow() - state_data["created_at"]).total_seconds()
    if time_diff > 600:  # 10 minutes
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail="State token expired",
        )

    user_id = state_data["user_id"]

    # Get user from database
    result = await db.execute(select(User).where(User.id == user_id))
    user = result.scalar_one_or_none()

    if not user:
        raise HTTPException(
            status_code=status.HTTP_404_NOT_FOUND,
            detail="User not found",
        )

    oauth_client = GitHubOAuthClient()

    try:
        # Exchange authorization code for access token
        token_data = await oauth_client.exchange_code_for_token(code)
        access_token = token_data["access_token"]
        scopes = token_data.get("scope", "")

        # Fetch GitHub user profile
        profile = await oauth_client.get_user_profile(access_token)
        github_user_id = str(profile["id"])
        github_username = profile["login"]
        avatar_url = profile.get("avatar_url")

        # Try to get primary email
        github_email = profile.get("email")
        if not github_email:
            # Fetch from emails endpoint
            emails = await oauth_client.get_user_emails(access_token)
            primary_email = next((e for e in emails if e.get("primary")), None)
            if primary_email:
                github_email = primary_email["email"]

        # Encrypt access token for storage
        encrypted_token = encrypt_token(access_token)

        # Check if GitHub account already exists
        result = await db.execute(
            select(GitHubAccount).where(
                GitHubAccount.github_user_id == github_user_id
            )
        )
        github_account = result.scalar_one_or_none()

        if github_account:
            # Update existing account
            github_account.user_id = user.id
            github_account.github_username = github_username
            github_account.github_email = github_email
            github_account.access_token_encrypted = encrypted_token
            github_account.scopes = scopes
            github_account.avatar_url = avatar_url
            github_account.updated_at = datetime.utcnow()
        else:
            # Create new GitHub account
            github_account = GitHubAccount(
                id=str(uuid.uuid4()),
                user_id=user.id,
                github_user_id=github_user_id,
                github_username=github_username,
                github_email=github_email,
                access_token_encrypted=encrypted_token,
                scopes=scopes,
                avatar_url=avatar_url,
            )
            db.add(github_account)

        await db.commit()
        await db.refresh(github_account)

        # Return success response (redirect to frontend)
        return {
            "message": "GitHub account connected successfully",
            "github_username": github_username,
            "redirect_url": "http://localhost:3000/settings/integrations?github=success",
        }

    except Exception as e:
        # Log error and return failure
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail=f"Failed to connect GitHub account: {str(e)}",
        )


@router.get("/status", response_model=GitHubConnectionStatus)
async def get_github_connection_status(
    current_user: User = Depends(get_current_user),
    db: AsyncSession = Depends(get_db),
):
    """
    Check if user has connected their GitHub account.

    Returns connection status and basic account information.
    """
    result = await db.execute(
        select(GitHubAccount).where(GitHubAccount.user_id == current_user.id)
    )
    github_account = result.scalar_one_or_none()

    if github_account:
        scopes_list = github_account.scopes.split() if github_account.scopes else None
        return GitHubConnectionStatus(
            is_connected=True,
            github_username=github_account.github_username,
            avatar_url=github_account.avatar_url,
            scopes=scopes_list,
        )
    else:
        return GitHubConnectionStatus(is_connected=False)


@router.delete("/disconnect", status_code=status.HTTP_204_NO_CONTENT)
async def disconnect_github_account(
    current_user: User = Depends(get_current_user),
    db: AsyncSession = Depends(get_db),
):
    """
    Disconnect GitHub account from user.

    Revokes the GitHub access token and deletes the account record.
    """
    result = await db.execute(
        select(GitHubAccount).where(GitHubAccount.user_id == current_user.id)
    )
    github_account = result.scalar_one_or_none()

    if not github_account:
        raise HTTPException(
            status_code=status.HTTP_404_NOT_FOUND,
            detail="No GitHub account connected",
        )

    try:
        # Decrypt access token
        access_token = decrypt_token(github_account.access_token_encrypted)

        # Revoke token on GitHub
        oauth_client = GitHubOAuthClient()
        await oauth_client.revoke_token(access_token)

    except Exception as e:
        # Log error but continue with deletion
        print(f"Failed to revoke GitHub token: {e}")

    # Delete GitHub account record
    await db.delete(github_account)
    await db.commit()

    return None


@router.get("/repositories", response_model=GitHubRepositoryListResponse)
async def list_github_repositories(
    current_user: User = Depends(get_current_user),
    db: AsyncSession = Depends(get_db),
):
    """
    List user's GitHub repositories.

    Fetches repositories from GitHub API using stored access token.
    """
    # Get GitHub account
    result = await db.execute(
        select(GitHubAccount).where(GitHubAccount.user_id == current_user.id)
    )
    github_account = result.scalar_one_or_none()

    if not github_account:
        raise HTTPException(
            status_code=status.HTTP_404_NOT_FOUND,
            detail="No GitHub account connected. Please connect your GitHub account first.",
        )

    try:
        # Decrypt access token
        access_token = decrypt_token(github_account.access_token_encrypted)

        # Fetch repositories from GitHub
        oauth_client = GitHubOAuthClient()
        repos = await oauth_client.get_user_repositories(access_token)

        # Convert to response schema
        repository_list = [
            GitHubRepositoryInfo(
                id=str(repo["id"]),
                name=repo["name"],
                full_name=repo["full_name"],
                description=repo.get("description"),
                html_url=repo["html_url"],
                private=repo["private"],
                fork=repo["fork"],
                stargazers_count=repo["stargazers_count"],
                language=repo.get("language"),
                default_branch=repo.get("default_branch", "main"),
                updated_at=datetime.fromisoformat(repo["updated_at"].replace("Z", "+00:00")),
            )
            for repo in repos
        ]

        return GitHubRepositoryListResponse(
            repositories=repository_list,
            total=len(repository_list),
        )

    except Exception as e:
        raise HTTPException(
            status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
            detail=f"Failed to fetch GitHub repositories: {str(e)}",
        )
