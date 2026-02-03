"""Code annotation models for Phase 14.3."""

from datetime import datetime
from typing import Optional
from uuid import UUID, uuid4

from sqlalchemy import Column, String, Integer, DateTime, Text, Boolean
from sqlalchemy.dialects.postgresql import UUID as PGUUID
from sqlalchemy.sql import func
from sqlalchemy import JSON

from app.database import Base


class CodeAnnotation(Base):
    """
    Persistent code highlights and annotations.
    Visible to all collaborators at a location.
    """

    __tablename__ = "code_annotations"

    id = Column(PGUUID(as_uuid=True), primary_key=True, default=uuid4)
    user_id = Column(PGUUID(as_uuid=True), nullable=False, index=True)
    location = Column(String(500), nullable=False, index=True)

    # Line range
    line_start = Column(Integer, nullable=False)
    line_end = Column(Integer, nullable=False)
    column_start = Column(Integer, nullable=True)
    column_end = Column(Integer, nullable=True)

    # Annotation properties
    color = Column(String(20), nullable=False, default="yellow")  # yellow, green, blue, red, purple, orange
    label = Column(String(200), nullable=True)  # Optional label/title
    description = Column(Text, nullable=True)  # Longer description

    # State
    is_temporary = Column(Boolean, default=False)  # Temporary vs persistent
    is_public = Column(Boolean, default=True)  # Visible to others

    # Timestamps
    created_at = Column(DateTime(timezone=True), server_default=func.now(), nullable=False, index=True)
    updated_at = Column(DateTime(timezone=True), server_default=func.now(), onupdate=func.now(), nullable=False)
    expires_at = Column(DateTime(timezone=True), nullable=True)  # Optional expiration

    # Additional metadata
    metadata = Column(JSON, default=dict)

    def __repr__(self):
        return f"<CodeAnnotation(id={self.id}, location={self.location}, line={self.line_start}-{self.line_end}, color={self.color})>"

    def to_dict(self):
        """Convert to dictionary."""
        return {
            "id": str(self.id),
            "user_id": str(self.user_id),
            "location": self.location,
            "line_start": self.line_start,
            "line_end": self.line_end,
            "column_start": self.column_start,
            "column_end": self.column_end,
            "color": self.color,
            "label": self.label,
            "description": self.description,
            "is_temporary": self.is_temporary,
            "is_public": self.is_public,
            "created_at": self.created_at.isoformat() if self.created_at else None,
            "updated_at": self.updated_at.isoformat() if self.updated_at else None,
            "expires_at": self.expires_at.isoformat() if self.expires_at else None,
            "metadata": self.metadata or {},
        }

    def is_expired(self) -> bool:
        """Check if annotation has expired."""
        if not self.expires_at:
            return False
        return datetime.utcnow() > self.expires_at
