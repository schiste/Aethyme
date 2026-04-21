"""User model for authentication and profile."""

from sqlalchemy import Column, String, Boolean, ForeignKey
from sqlalchemy.orm import relationship
from app.models.base import Base, TimestampMixin


class User(Base, TimestampMixin):
    """User model for authentication and profile management."""

    __tablename__ = "users"

    id = Column(String(36), primary_key=True)  # UUID
    email = Column(String(255), unique=True, nullable=False, index=True)
    hashed_password = Column(String(255), nullable=True)  # Nullable for OAuth-only users
    full_name = Column(String(255), nullable=True)
    avatar_url = Column(String(512), nullable=True)

    is_active = Column(Boolean, default=True, nullable=False)
    is_verified = Column(Boolean, default=False, nullable=False)
    is_superuser = Column(Boolean, default=False, nullable=False)

    # OAuth providers - GitHub
    github_id = Column(String(255), unique=True, nullable=True, index=True)
    github_username = Column(String(255), nullable=True)
    github_access_token = Column(String(512), nullable=True)  # Encrypted

    # OAuth providers - GitLab
    gitlab_id = Column(String(255), unique=True, nullable=True, index=True)
    gitlab_username = Column(String(255), nullable=True)
    gitlab_access_token = Column(String(512), nullable=True)  # Encrypted

    # OAuth providers - Bitbucket
    bitbucket_id = Column(String(255), unique=True, nullable=True, index=True)
    bitbucket_username = Column(String(255), nullable=True)
    bitbucket_access_token = Column(String(512), nullable=True)  # Encrypted

    # Organization relationship
    organization_id = Column(String(36), ForeignKey("organizations.id"), nullable=True)
    organization = relationship("Organization", back_populates="users")

    # GitHub account relationship (one-to-one)
    github_account = relationship("GitHubAccount", back_populates="user", uselist=False, cascade="all, delete-orphan")

    def __repr__(self):
        return f"<User {self.email}>"
