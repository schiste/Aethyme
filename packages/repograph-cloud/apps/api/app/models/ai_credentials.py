"""
AI Credentials Model

Stores customer-provided AI API keys (encrypted) for BYOK architecture.
"""

from sqlalchemy import Column, String, Integer, DateTime, ForeignKey, Text, Boolean, Index
from sqlalchemy.orm import relationship
from sqlalchemy.sql import func
from cryptography.fernet import Fernet
import json

from app.core.database import Base
from app.core.config import settings


class AICredential(Base):
    """
    Stores encrypted AI provider credentials for users.

    Each user can have multiple AI provider credentials (e.g., OpenAI + Claude).
    API keys are encrypted at rest using Fernet encryption.
    """

    __tablename__ = "ai_credentials"

    id = Column(Integer, primary_key=True, index=True)
    user_id = Column(Integer, ForeignKey("users.id", ondelete="CASCADE"), nullable=False)

    # Provider information
    provider_type = Column(String(50), nullable=False)  # openai, claude, azure_openai, etc.
    provider_name = Column(String(100))  # User-friendly name (e.g., "My OpenAI Key")

    # Encrypted credentials
    # Format: Fernet-encrypted JSON with provider-specific fields
    encrypted_credentials = Column(Text, nullable=False)

    # Status
    is_active = Column(Boolean, default=True)
    is_validated = Column(Boolean, default=False)  # Has the key been validated?
    last_validated_at = Column(DateTime(timezone=True))
    validation_error = Column(Text)  # Error message if validation failed

    # Usage tracking
    last_used_at = Column(DateTime(timezone=True))
    total_requests = Column(Integer, default=0)
    total_tokens_used = Column(Integer, default=0)

    # Metadata
    created_at = Column(DateTime(timezone=True), server_default=func.now())
    updated_at = Column(DateTime(timezone=True), onupdate=func.now())

    # Relationships
    user = relationship("User", back_populates="ai_credentials")

    # Indexes
    __table_args__ = (
        Index("ix_ai_credentials_user_provider", "user_id", "provider_type"),
        Index("ix_ai_credentials_is_active", "is_active"),
    )

    @staticmethod
    def get_cipher() -> Fernet:
        """Get Fernet cipher for encryption/decryption."""
        return Fernet(settings.ENCRYPTION_KEY.encode())

    @classmethod
    def encrypt_credentials(cls, credentials_dict: dict) -> str:
        """
        Encrypt credentials dictionary.

        Args:
            credentials_dict: Dict with provider-specific fields
                For OpenAI: {"api_key": "sk-...", "organization_id": "org-..."}
                For Azure: {"api_key": "...", "resource_name": "...", "deployment_name": "..."}
                For Claude: {"api_key": "sk-ant-..."}

        Returns:
            Encrypted string
        """
        cipher = cls.get_cipher()
        json_str = json.dumps(credentials_dict)
        encrypted_bytes = cipher.encrypt(json_str.encode())
        return encrypted_bytes.decode()

    @classmethod
    def decrypt_credentials(cls, encrypted_str: str) -> dict:
        """
        Decrypt credentials string.

        Args:
            encrypted_str: Encrypted credentials string

        Returns:
            Decrypted credentials dictionary
        """
        cipher = cls.get_cipher()
        decrypted_bytes = cipher.decrypt(encrypted_str.encode())
        json_str = decrypted_bytes.decode()
        return json.loads(json_str)

    def get_credentials_dict(self) -> dict:
        """Get decrypted credentials as dictionary."""
        return self.decrypt_credentials(self.encrypted_credentials)

    def set_credentials_dict(self, credentials_dict: dict):
        """Set credentials from dictionary (will be encrypted)."""
        self.encrypted_credentials = self.encrypt_credentials(credentials_dict)


class AIUsageLog(Base):
    """
    Logs AI API usage for billing and analytics.

    Tracks every AI API call with tokens used and estimated cost.
    """

    __tablename__ = "ai_usage_logs"

    id = Column(Integer, primary_key=True, index=True)
    user_id = Column(Integer, ForeignKey("users.id", ondelete="CASCADE"), nullable=False)
    credential_id = Column(Integer, ForeignKey("ai_credentials.id", ondelete="SET NULL"))

    # Request details
    provider_type = Column(String(50), nullable=False)
    operation_type = Column(String(50), nullable=False)  # embedding, chat_completion
    model = Column(String(100), nullable=False)

    # Usage
    tokens_used = Column(Integer, nullable=False)
    request_count = Column(Integer, default=1)

    # Cost estimation (in USD)
    estimated_cost = Column(String(20))  # Stored as string to avoid float precision issues

    # Metadata
    repository_id = Column(String(100))  # If related to a repository
    session_id = Column(String(100))     # For tracking multi-step operations
    created_at = Column(DateTime(timezone=True), server_default=func.now())

    # Relationships
    user = relationship("User")
    credential = relationship("AICredential")

    # Indexes
    __table_args__ = (
        Index("ix_ai_usage_logs_user_created", "user_id", "created_at"),
        Index("ix_ai_usage_logs_credential", "credential_id"),
        Index("ix_ai_usage_logs_repository", "repository_id"),
    )


class AIProviderQuota(Base):
    """
    Optional: Set usage quotas per user to prevent runaway costs.

    Example: Limit user to 1M tokens per month across all providers.
    """

    __tablename__ = "ai_provider_quotas"

    id = Column(Integer, primary_key=True, index=True)
    user_id = Column(Integer, ForeignKey("users.id", ondelete="CASCADE"), nullable=False)

    # Quota limits (per month)
    max_tokens_per_month = Column(Integer)
    max_requests_per_month = Column(Integer)
    max_cost_per_month = Column(String(20))  # USD

    # Current usage (reset monthly)
    current_tokens = Column(Integer, default=0)
    current_requests = Column(Integer, default=0)
    current_cost = Column(String(20), default="0.00")

    # Reset tracking
    quota_period_start = Column(DateTime(timezone=True), server_default=func.now())
    quota_period_end = Column(DateTime(timezone=True))

    # Metadata
    created_at = Column(DateTime(timezone=True), server_default=func.now())
    updated_at = Column(DateTime(timezone=True), onupdate=func.now())

    # Relationships
    user = relationship("User")

    # Indexes
    __table_args__ = (
        Index("ix_ai_quotas_user", "user_id", unique=True),
    )
