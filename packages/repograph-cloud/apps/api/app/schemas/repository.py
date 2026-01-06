"""Repository-related Pydantic schemas."""

from typing import Optional, List, Dict
from pydantic import BaseModel, Field, HttpUrl
from datetime import datetime


class RepositoryBase(BaseModel):
    """Base repository schema."""

    name: str = Field(..., min_length=1, max_length=255)
    full_name: str = Field(..., min_length=1, max_length=512, description="e.g., owner/repo")
    url: HttpUrl
    description: Optional[str] = None
    provider: str = Field(..., pattern="^(github|gitlab|bitbucket)$")
    default_branch: str = Field(default="main", max_length=255)
    is_private: bool = False


class RepositoryCreate(RepositoryBase):
    """Schema for creating a repository."""

    provider_id: str = Field(..., max_length=255, description="Repository ID from provider")


class RepositoryUpdate(BaseModel):
    """Schema for updating a repository."""

    name: Optional[str] = Field(None, min_length=1, max_length=255)
    description: Optional[str] = None
    default_branch: Optional[str] = Field(None, max_length=255)


class RepositoryResponse(RepositoryBase):
    """Schema for repository response."""

    id: str
    organization_id: str
    provider_id: str
    is_indexed: bool
    indexing_status: str
    last_indexed_at: Optional[datetime] = None
    last_commit_sha: Optional[str] = None
    file_count: int
    line_count: int
    language_stats: Optional[str] = None
    created_at: datetime
    updated_at: datetime

    class Config:
        from_attributes = True


class RepositoryStats(BaseModel):
    """Schema for repository statistics."""

    total_repositories: int
    indexed_repositories: int
    pending_repositories: int
    failed_repositories: int
    total_files: int
    total_lines: int


class RepositoryListResponse(BaseModel):
    """Schema for paginated repository list."""

    items: list[RepositoryResponse]
    total: int
    page: int
    page_size: int
    has_next: bool
    has_prev: bool


class FileTreeNode(BaseModel):
    """Represents a file or folder in the repository tree."""

    name: str = Field(..., description="File or folder name")
    path: str = Field(..., description="Full path from repository root")
    type: str = Field(..., pattern="^(file|folder)$", description="Node type")
    language: Optional[str] = Field(None, description="Programming language (for files only)")
    size_bytes: Optional[int] = Field(None, description="File size in bytes (for files only)")
    line_count: Optional[int] = Field(None, description="Number of lines (for files only)")
    children: Optional[List['FileTreeNode']] = Field(None, description="Child nodes (for folders only)")

    class Config:
        from_attributes = True


class RepositoryTreeResponse(BaseModel):
    """Repository file tree response."""

    repository_id: str
    repository_name: str
    total_files: int
    total_folders: int
    tree: List[FileTreeNode]
    indexed_at: Optional[datetime]

    class Config:
        from_attributes = True


class RepositoryStatsResponse(BaseModel):
    """Repository statistics and metadata."""

    repository_id: str
    name: str
    full_name: str
    url: str
    description: Optional[str]
    stars: int = Field(default=0)
    forks: int = Field(default=0)
    total_files: int = Field(default=0)
    total_lines: int = Field(default=0)
    languages: Dict[str, int] = Field(default_factory=dict, description="Language name to line count mapping")
    primary_language: Optional[str]
    indexed_at: Optional[datetime]
    created_at: datetime
    updated_at: datetime

    class Config:
        from_attributes = True
