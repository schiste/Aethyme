"""Pydantic schemas for GitHub OAuth and account management."""

from typing import Optional, List
from pydantic import BaseModel, Field
from datetime import datetime


class GitHubAuthorizationURL(BaseModel):
    """Response schema for GitHub authorization URL."""

    authorization_url: str = Field(..., description="GitHub OAuth authorization URL to redirect user to")
    state: str = Field(..., description="CSRF protection state token (store in session)")


class GitHubCallbackRequest(BaseModel):
    """Request schema for GitHub OAuth callback."""

    code: str = Field(..., description="Authorization code from GitHub")
    state: str = Field(..., description="State token for CSRF verification")


class GitHubAccountResponse(BaseModel):
    """Response schema for GitHub account information."""

    id: str
    github_user_id: str
    github_username: str
    github_email: Optional[str] = None
    avatar_url: Optional[str] = None
    scopes: Optional[str] = None
    created_at: datetime
    updated_at: datetime

    class Config:
        from_attributes = True


class GitHubConnectionStatus(BaseModel):
    """Response schema for GitHub connection status."""

    is_connected: bool = Field(..., description="Whether user has connected GitHub account")
    github_username: Optional[str] = Field(None, description="GitHub username if connected")
    avatar_url: Optional[str] = Field(None, description="GitHub avatar URL if connected")
    scopes: Optional[List[str]] = Field(None, description="OAuth scopes granted")


class GitHubRepositoryImport(BaseModel):
    """Request schema for importing GitHub repositories."""

    repository_ids: List[str] = Field(..., description="List of GitHub repository IDs to import", min_items=1)


class GitHubRepositoryInfo(BaseModel):
    """Schema for GitHub repository information."""

    id: str = Field(..., description="GitHub repository ID")
    name: str = Field(..., description="Repository name")
    full_name: str = Field(..., description="Full repository name (owner/repo)")
    description: Optional[str] = Field(None, description="Repository description")
    html_url: str = Field(..., description="GitHub repository URL")
    private: bool = Field(..., description="Whether repository is private")
    fork: bool = Field(..., description="Whether repository is a fork")
    stargazers_count: int = Field(..., description="Number of stars")
    language: Optional[str] = Field(None, description="Primary programming language")
    default_branch: str = Field(default="main", description="Default branch name")
    updated_at: datetime = Field(..., description="Last updated timestamp")


class GitHubRepositoryListResponse(BaseModel):
    """Response schema for listing GitHub repositories."""

    repositories: List[GitHubRepositoryInfo]
    total: int = Field(..., description="Total number of repositories")
