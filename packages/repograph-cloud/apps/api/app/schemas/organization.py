"""Organization-related Pydantic schemas."""

from typing import Optional
from pydantic import BaseModel, Field
from datetime import datetime


class OrganizationBase(BaseModel):
    """Base organization schema."""

    name: str = Field(..., min_length=1, max_length=255)
    slug: Optional[str] = None


class OrganizationCreate(OrganizationBase):
    """Schema for creating an organization."""

    plan: str = Field(default="free", pattern="^(free|pro|team|enterprise)$")


class OrganizationUpdate(BaseModel):
    """Schema for updating an organization."""

    name: Optional[str] = Field(None, min_length=1, max_length=255)


class OrganizationResponse(OrganizationBase):
    """Schema for organization response."""

    id: str
    plan: str
    is_active: bool
    max_repositories: int
    max_queries_per_month: int
    current_queries_this_month: int
    stripe_customer_id: Optional[str] = None
    stripe_subscription_id: Optional[str] = None
    created_at: datetime
    updated_at: datetime

    class Config:
        from_attributes = True


class OrganizationStats(BaseModel):
    """Schema for organization statistics."""

    total_users: int
    total_repositories: int
    queries_this_month: int
    queries_remaining: int
    repositories_remaining: int
