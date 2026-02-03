"""GitHub Account model for OAuth integration."""

import uuid
from datetime import datetime
from typing import Optional
from sqlalchemy import String, Text, DateTime, ForeignKey
from sqlalchemy.orm import Mapped, mapped_column, relationship

from app.models.base import Base


class GitHubAccount(Base):
    """
    GitHub OAuth account linked to a user.

    Stores encrypted GitHub access tokens and user information.
    """

    __tablename__ = "github_accounts"

    id: Mapped[str] = mapped_column(String(36), primary_key=True, default=lambda: str(uuid.uuid4()))
    user_id: Mapped[str] = mapped_column(String(36), ForeignKey("users.id", ondelete="CASCADE"), index=True, nullable=False)

    # GitHub user information
    github_user_id: Mapped[str] = mapped_column(String(255), unique=True, index=True, nullable=False)
    github_username: Mapped[str] = mapped_column(String(255), nullable=False)
    github_email: Mapped[Optional[str]] = mapped_column(String(255), nullable=True)
    avatar_url: Mapped[Optional[str]] = mapped_column(Text, nullable=True)

    # Encrypted OAuth tokens
    access_token_encrypted: Mapped[str] = mapped_column(Text, nullable=False)
    refresh_token_encrypted: Mapped[Optional[str]] = mapped_column(Text, nullable=True)
    token_expires_at: Mapped[Optional[datetime]] = mapped_column(DateTime, nullable=True)
    scopes: Mapped[Optional[str]] = mapped_column(Text, nullable=True)

    # Timestamps
    created_at: Mapped[datetime] = mapped_column(DateTime, default=datetime.utcnow, nullable=False)
    updated_at: Mapped[datetime] = mapped_column(DateTime, default=datetime.utcnow, onupdate=datetime.utcnow, nullable=False)

    # Relationships
    user: Mapped["User"] = relationship("User", back_populates="github_account")

    def __repr__(self) -> str:
        return f"<GitHubAccount(id={self.id}, github_username={self.github_username}, user_id={self.user_id})>"
