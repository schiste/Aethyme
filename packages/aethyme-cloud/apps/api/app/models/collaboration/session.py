"""Collaboration session models."""

from datetime import datetime
from typing import Optional
from uuid import UUID, uuid4

from sqlalchemy import Column, String, Integer, DateTime, JSON
from sqlalchemy.dialects.postgresql import UUID as PGUUID
from sqlalchemy.sql import func

from app.database import Base


class CollaborationSession(Base):
    """
    Represents a collaboration session where multiple users
    are viewing/working on the same location (file/symbol).
    """

    __tablename__ = "collaboration_sessions"

    id = Column(PGUUID(as_uuid=True), primary_key=True, default=uuid4)
    location = Column(String(500), nullable=False, index=True)
    participant_count = Column(Integer, default=0, nullable=False)
    created_at = Column(DateTime(timezone=True), server_default=func.now(), nullable=False)
    last_activity = Column(
        DateTime(timezone=True),
        server_default=func.now(),
        onupdate=func.now(),
        nullable=False,
        index=True,
    )
    metadata = Column(JSON, default=dict)

    def __repr__(self):
        return f"<CollaborationSession(id={self.id}, location={self.location}, participants={self.participant_count})>"

    def to_dict(self):
        """Convert to dictionary."""
        return {
            "id": str(self.id),
            "location": self.location,
            "participant_count": self.participant_count,
            "created_at": self.created_at.isoformat() if self.created_at else None,
            "last_activity": self.last_activity.isoformat() if self.last_activity else None,
            "metadata": self.metadata or {},
        }
