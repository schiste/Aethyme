"""API Key-related Pydantic schemas."""

from typing import Optional, List
from pydantic import BaseModel, Field
from datetime import datetime


class APIKeyBase(BaseModel):
    """Base API key schema."""

    name: str = Field(..., min_length=1, max_length=255, description="Human-readable name for the API key")
    scopes: Optional[List[str]] = Field(default=None, description="List of permission scopes")


class APIKeyCreate(APIKeyBase):
    """Schema for creating an API key."""

    expires_in_days: Optional[int] = Field(None, ge=1, le=365, description="Key expiration in days (optional)")


class APIKeyUpdate(BaseModel):
    """Schema for updating an API key."""

    name: Optional[str] = Field(None, min_length=1, max_length=255)
    scopes: Optional[List[str]] = None
    is_active: Optional[bool] = None


class APIKeyResponse(APIKeyBase):
    """Schema for API key response (without full key)."""

    id: str
    organization_id: str
    key_prefix: str
    is_active: bool
    last_used_at: Optional[datetime] = None
    expires_at: Optional[datetime] = None
    created_at: datetime
    updated_at: datetime

    class Config:
        from_attributes = True


class APIKeyCreateResponse(APIKeyResponse):
    """
    Schema for API key creation response.
    
    This is the ONLY time the full key is returned.
    It should be saved by the client immediately.
    """

    key: str = Field(..., description="Full API key - SAVE THIS NOW, it won't be shown again")


class APIKeyListResponse(BaseModel):
    """Schema for paginated API key list."""

    items: List[APIKeyResponse]
    total: int
