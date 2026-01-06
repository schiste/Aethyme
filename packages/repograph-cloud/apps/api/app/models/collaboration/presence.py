"""User presence models."""

from datetime import datetime
from typing import Optional
from uuid import UUID, uuid4

from sqlalchemy import Column, String, Integer, DateTime, ForeignKey, JSON
from sqlalchemy.dialects.postgresql import UUID as PGUUID
from sqlalchemy.sql import func
from sqlalchemy.orm import relationship

from app.database import Base


class UserPresence(Base):
    """
    Tracks user presence - where they are, what they're doing,
    cursor position, selection, etc.
    """

    __tablename__ = "user_presence"

    id = Column(PGUUID(as_uuid=True), primary_key=True, default=uuid4)
    user_id = Column(PGUUID(as_uuid=True), nullable=False, index=True)
    session_id = Column(
        PGUUID(as_uuid=True),
        ForeignKey("collaboration_sessions.id", ondelete="CASCADE"),
        nullable=False,
        index=True,
    )
    status = Column(String(20), nullable=False, default="online", index=True)  # online, idle, offline
    location = Column(String(500), nullable=False, index=True)

    # Cursor position
    cursor_line = Column(Integer, nullable=True)
    cursor_column = Column(Integer, nullable=True)

    # Selection range
    selection_start_line = Column(Integer, nullable=True)
    selection_start_column = Column(Integer, nullable=True)
    selection_end_line = Column(Integer, nullable=True)
    selection_end_column = Column(Integer, nullable=True)

    # Viewport (visible line range for follow mode)
    viewport_start_line = Column(Integer, nullable=True)
    viewport_end_line = Column(Integer, nullable=True)
    viewport_scroll_top = Column(Integer, nullable=True)

    # Timestamps
    last_seen = Column(
        DateTime(timezone=True),
        server_default=func.now(),
        onupdate=func.now(),
        nullable=False,
        index=True,
    )
    joined_at = Column(DateTime(timezone=True), server_default=func.now(), nullable=False)

    # Additional metadata (user agent, viewport, etc.)
    metadata = Column(JSON, default=dict)

    # Relationships
    session = relationship("CollaborationSession", backref="participants")

    def __repr__(self):
        return f"<UserPresence(user_id={self.user_id}, location={self.location}, status={self.status})>"

    def to_dict(self):
        """Convert to dictionary."""
        result = {
            "id": str(self.id),
            "user_id": str(self.user_id),
            "session_id": str(self.session_id),
            "status": self.status,
            "location": self.location,
            "last_seen": self.last_seen.isoformat() if self.last_seen else None,
            "joined_at": self.joined_at.isoformat() if self.joined_at else None,
            "metadata": self.metadata or {},
        }

        # Add cursor if present
        if self.cursor_line is not None and self.cursor_column is not None:
            result["cursor"] = {"line": self.cursor_line, "column": self.cursor_column}

        # Add selection if present
        if all(
            [
                self.selection_start_line is not None,
                self.selection_start_column is not None,
                self.selection_end_line is not None,
                self.selection_end_column is not None,
            ]
        ):
            result["selection"] = {
                "start": {"line": self.selection_start_line, "column": self.selection_start_column},
                "end": {"line": self.selection_end_line, "column": self.selection_end_column},
            }

        # Add viewport if present
        if self.viewport_start_line is not None and self.viewport_end_line is not None:
            result["viewport"] = {
                "start_line": self.viewport_start_line,
                "end_line": self.viewport_end_line,
                "scroll_top": self.viewport_scroll_top,
            }

        return result
