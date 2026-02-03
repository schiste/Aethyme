"""Search schemas for API responses."""

from typing import List, Optional, Dict, Any
from pydantic import BaseModel, Field
from datetime import datetime


class SearchRequest(BaseModel):
    """Search request parameters."""
    query: str = Field(..., min_length=1, max_length=500, description="Search query")
    repositories: Optional[List[str]] = Field(None, description="Filter by repository IDs")
    languages: Optional[List[str]] = Field(None, description="Filter by programming languages")
    file_path: Optional[str] = Field(None, description="Filter by file path pattern")
    page: int = Field(1, ge=1, description="Page number")
    per_page: int = Field(20, ge=1, le=100, description="Results per page")
    sort_by: str = Field("relevance", description="Sort by relevance or date")


class SearchResultItem(BaseModel):
    """Single search result item."""
    repository_id: str
    repository_name: str
    file_path: str
    file_name: str
    language: str
    score: float
    highlights: List[str]
    line_count: int
    size_bytes: int
    indexed_at: datetime


class SearchFacets(BaseModel):
    """Search result facets for filtering."""
    languages: Dict[str, int] = Field(default_factory=dict)
    repositories: Dict[str, int] = Field(default_factory=dict)


class SearchResponse(BaseModel):
    """Search response with results and metadata."""
    query: str
    total: int
    page: int
    per_page: int
    total_pages: int
    results: List[SearchResultItem]
    facets: SearchFacets
    took_ms: float


class FileContentRequest(BaseModel):
    """Request for file content."""
    path: str = Field(..., description="File path relative to repository root")
    format: str = Field("raw", description="Format: raw or html")
    start_line: Optional[int] = Field(None, ge=1, description="Start line number")
    end_line: Optional[int] = Field(None, ge=1, description="End line number")


class FileContentResponse(BaseModel):
    """File content response."""
    repository_id: str
    file_path: str
    language: str
    content: str
    line_count: int
    size_bytes: int
    last_indexed: datetime
