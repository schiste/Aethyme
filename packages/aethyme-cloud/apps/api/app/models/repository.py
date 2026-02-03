"""Repository model for code repositories."""

from sqlalchemy import Column, String, Boolean, Integer, ForeignKey, Text, DateTime
from sqlalchemy.dialects.postgresql import JSONB
from sqlalchemy.orm import relationship
from app.models.base import Base, TimestampMixin


class Repository(Base, TimestampMixin):
    """Repository model for tracking indexed code repositories."""

    __tablename__ = "repositories"

    id = Column(String(36), primary_key=True)  # UUID
    organization_id = Column(String(36), ForeignKey("organizations.id"), nullable=False, index=True)

    # Repository info
    name = Column(String(255), nullable=False)
    full_name = Column(String(512), nullable=False)  # e.g., "owner/repo"
    url = Column(String(512), nullable=False)
    description = Column(Text, nullable=True)

    # Source control provider
    provider = Column(String(50), nullable=False)  # github, gitlab, bitbucket
    provider_id = Column(String(255), nullable=False)  # Repository ID from provider
    default_branch = Column(String(255), default="main", nullable=False)

    # Visibility
    is_private = Column(Boolean, default=False, nullable=False)

    # Indexing status
    is_indexed = Column(Boolean, default=False, nullable=False)
    indexing_status = Column(String(50), default="pending", nullable=False)  # pending, indexing, completed, failed
    last_indexed_at = Column(DateTime, nullable=True)
    last_commit_sha = Column(String(40), nullable=True)

    # Statistics
    file_count = Column(Integer, default=0, nullable=False)
    line_count = Column(Integer, default=0, nullable=False)
    language_stats = Column(Text, nullable=True)  # JSON stored as text

    # New indexing fields (Week 8)
    github_id = Column(String(255), nullable=True, index=True)  # GitHub repository ID
    clone_url = Column(String(512), nullable=True)  # Git clone URL
    git_url = Column(String(512), nullable=True)  # Git SSH URL
    stars = Column(Integer, default=0, nullable=False)
    forks = Column(Integer, default=0, nullable=False)
    total_files = Column(Integer, default=0, nullable=False)  # Total files indexed
    total_lines = Column(Integer, default=0, nullable=False)  # Total lines of code
    languages = Column(JSONB, nullable=True)  # Language breakdown
    indexed_at = Column(DateTime, nullable=True)  # Last successful index timestamp
    primary_language = Column(String(100), nullable=True)  # Primary programming language

    # Indexing status (Phase 6)
    status = Column(String(50), default="pending", nullable=False)  # pending, indexing, active, error
    last_error = Column(Text, nullable=True)  # Last indexing error message
    last_synced_at = Column(DateTime, nullable=True)  # Last webhook sync timestamp

    # Relationships
    organization = relationship("Organization", back_populates="repositories")

    def __repr__(self):
        return f"<Repository {self.full_name}>"
