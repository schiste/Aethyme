"""Dashboard schema definitions."""

from typing import List
from pydantic import BaseModel, Field


class LanguageStats(BaseModel):
    """Statistics for a specific programming language."""

    language: str = Field(..., description="Programming language name")
    file_count: int = Field(..., description="Number of files in this language", ge=0)
    line_count: int = Field(..., description="Number of lines of code", ge=0)
    percentage: float = Field(..., description="Percentage of total files", ge=0, le=100)


class DashboardStatsResponse(BaseModel):
    """Dashboard statistics response."""

    total_repositories: int = Field(..., description="Total number of repositories", ge=0)
    indexed_repositories: int = Field(..., description="Number of fully indexed repositories", ge=0)
    indexing_repositories: int = Field(..., description="Number of repositories currently being indexed", ge=0)
    failed_repositories: int = Field(..., description="Number of repositories that failed to index", ge=0)
    total_files: int = Field(..., description="Total number of indexed files", ge=0)
    total_lines: int = Field(..., description="Total lines of code across all repositories", ge=0)
    languages: List[LanguageStats] = Field(default_factory=list, description="Language breakdown (top 10)")

    class Config:
        json_schema_extra = {
            "example": {
                "total_repositories": 15,
                "indexed_repositories": 12,
                "indexing_repositories": 2,
                "failed_repositories": 1,
                "total_files": 3542,
                "total_lines": 284671,
                "languages": [
                    {
                        "language": "TypeScript",
                        "file_count": 1523,
                        "line_count": 142536,
                        "percentage": 43.0
                    },
                    {
                        "language": "Python",
                        "file_count": 892,
                        "line_count": 87234,
                        "percentage": 25.2
                    },
                    {
                        "language": "JavaScript",
                        "file_count": 645,
                        "line_count": 34521,
                        "percentage": 18.2
                    }
                ]
            }
        }
