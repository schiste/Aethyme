"""Activity event models."""

from uuid import uuid4

from sqlalchemy import Column, String, DateTime, JSON
from sqlalchemy.dialects.postgresql import UUID as PGUUID
from sqlalchemy.sql import func

from app.database import Base


class ActivityEvent(Base):
    """
    Records user activity events for the activity feed.
    """

    __tablename__ = "activity_events"

    id = Column(PGUUID(as_uuid=True), primary_key=True, default=uuid4)
    user_id = Column(PGUUID(as_uuid=True), nullable=False, index=True)
    event_type = Column(String(50), nullable=False, index=True)  # user.joined, file.viewed, etc.
    location = Column(String(500), nullable=True, index=True)
    payload = Column(JSON, default=dict)
    created_at = Column(DateTime(timezone=True), server_default=func.now(), nullable=False, index=True)

    def __repr__(self):
        return f"<ActivityEvent(user_id={self.user_id}, type={self.event_type}, location={self.location})>"

    def to_dict(self):
        """Convert to dictionary."""
        return {
            "id": str(self.id),
            "user_id": str(self.user_id),
            "event_type": self.event_type,
            "location": self.location,
            "payload": self.payload or {},
            "created_at": self.created_at.isoformat() if self.created_at else None,
        }
