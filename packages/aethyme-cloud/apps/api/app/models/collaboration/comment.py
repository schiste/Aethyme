"""
Code Comments Model

Represents inline code comments with threading support for Phase 14.
"""

from sqlalchemy import (
    Column,
    String,
    Integer,
    Text,
    Boolean,
    DateTime,
    ForeignKey,
)
from sqlalchemy.dialects.postgresql import UUID as PGUUID, JSON
from sqlalchemy.orm import relationship, backref
from sqlalchemy.sql import func
from uuid import uuid4

from app.models.base import Base


class Comment(Base):
    """
    Code comment with support for inline, multi-line, and threaded comments.
    """

    __tablename__ = "collaboration_comments"

    id = Column(PGUUID(as_uuid=True), primary_key=True, default=uuid4)
    user_id = Column(PGUUID(as_uuid=True), nullable=False, index=True)
    location = Column(String(500), nullable=False, index=True)

    # Position in code
    line_start = Column(Integer, nullable=False)
    line_end = Column(Integer, nullable=True)  # For multi-line comments
    column_start = Column(Integer, nullable=True)
    column_end = Column(Integer, nullable=True)

    # Content
    content = Column(Text, nullable=False)

    # Threading
    parent_id = Column(
        PGUUID(as_uuid=True),
        ForeignKey("collaboration_comments.id", ondelete="CASCADE"),
        nullable=True,
        index=True,
    )

    # Resolution
    resolved = Column(Boolean, default=False, nullable=False, index=True)
    resolved_by = Column(PGUUID(as_uuid=True), nullable=True)
    resolved_at = Column(DateTime(timezone=True), nullable=True)

    # Timestamps
    created_at = Column(DateTime(timezone=True), server_default=func.now(), nullable=False)
    updated_at = Column(
        DateTime(timezone=True),
        server_default=func.now(),
        onupdate=func.now(),
        nullable=False,
    )

    # Metadata
    metadata = Column(JSON, default=dict, server_default="{}")

    # Relationships
    replies = relationship(
        "Comment",
        backref=backref("parent", remote_side=[id]),
        cascade="all, delete-orphan",
    )
    mentions = relationship(
        "Mention",
        back_populates="comment",
        cascade="all, delete-orphan",
    )

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
            "content": self.content,
            "parent_id": str(self.parent_id) if self.parent_id else None,
            "resolved": self.resolved,
            "resolved_by": str(self.resolved_by) if self.resolved_by else None,
            "resolved_at": self.resolved_at.isoformat() if self.resolved_at else None,
            "created_at": self.created_at.isoformat(),
            "updated_at": self.updated_at.isoformat(),
            "metadata": self.metadata,
            "reply_count": len(self.replies) if self.replies else 0,
        }


class Mention(Base):
    """
    User mention in a comment (@username).
    """

    __tablename__ = "collaboration_mentions"

    id = Column(PGUUID(as_uuid=True), primary_key=True, default=uuid4)
    comment_id = Column(
        PGUUID(as_uuid=True),
        ForeignKey("collaboration_comments.id", ondelete="CASCADE"),
        nullable=False,
        index=True,
    )
    mentioned_user_id = Column(PGUUID(as_uuid=True), nullable=False, index=True)
    notified = Column(Boolean, default=False, nullable=False)
    created_at = Column(DateTime(timezone=True), server_default=func.now(), nullable=False)

    # Relationships
    comment = relationship("Comment", back_populates="mentions")

    def to_dict(self):
        """Convert to dictionary."""
        return {
            "id": str(self.id),
            "comment_id": str(self.comment_id),
            "mentioned_user_id": str(self.mentioned_user_id),
            "notified": self.notified,
            "created_at": self.created_at.isoformat(),
        }
