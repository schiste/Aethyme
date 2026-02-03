"""API Key model for programmatic access."""

from sqlalchemy import Column, String, Boolean, DateTime, ForeignKey
from sqlalchemy.orm import relationship
from app.models.base import Base, TimestampMixin


class APIKey(Base, TimestampMixin):
    """API Key for programmatic access to the API."""

    __tablename__ = "api_keys"

    id = Column(String(36), primary_key=True)  # UUID
    organization_id = Column(String(36), ForeignKey("organizations.id"), nullable=False, index=True)

    # Key info
    name = Column(String(255), nullable=False)
    key_prefix = Column(String(20), nullable=False, index=True)  # First 8 chars for identification
    key_hash = Column(String(255), nullable=False, unique=True)  # bcrypt hash of full key
    
    # Permissions
    scopes = Column(String(512), nullable=True)  # Comma-separated scopes (e.g., "read,write")
    
    # Status
    is_active = Column(Boolean, default=True, nullable=False)
    last_used_at = Column(DateTime, nullable=True)
    expires_at = Column(DateTime, nullable=True)
    
    # Relationships
    organization = relationship("Organization", back_populates="api_keys")

    def __repr__(self):
        return f"<APIKey {self.key_prefix}... ({self.name})>"
