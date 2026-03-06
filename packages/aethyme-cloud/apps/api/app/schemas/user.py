"""User-related Pydantic schemas."""

from typing import Optional
from pydantic import BaseModel, EmailStr, Field
from datetime import datetime


class UserBase(BaseModel):
    """Base user schema with common fields."""

    email: EmailStr
    full_name: Optional[str] = None


class UserCreate(UserBase):
    """Schema for user registration."""

    password: str = Field(..., min_length=8, description="Password must be at least 8 characters")


class UserLogin(BaseModel):
    """Schema for user login."""

    email: EmailStr
    password: str


class UserUpdate(BaseModel):
    """Schema for updating user profile."""

    full_name: Optional[str] = None
    avatar_url: Optional[str] = None


class UserResponse(UserBase):
    """Schema for user response (excludes sensitive data)."""

    id: str
    is_active: bool
    is_verified: bool
    is_superuser: bool
    organization_id: Optional[str] = None
    avatar_url: Optional[str] = None
    created_at: datetime
    updated_at: datetime

    class Config:
        from_attributes = True


class Token(BaseModel):
    """JWT token schema."""

    access_token: str
    token_type: str = "bearer"


class TokenResponse(Token):
    """Extended token response with refresh token."""

    refresh_token: str
    expires_in: int  # seconds
    user: UserResponse


class DevTokenRequest(BaseModel):
    """Schema for requesting a development-only access token."""

    email: EmailStr


class DevTokenResponse(Token):
    """Development-only access token response."""

    expires_in: int  # seconds
    user: UserResponse


class RefreshTokenRequest(BaseModel):
    """Schema for refresh token request."""

    refresh_token: str
