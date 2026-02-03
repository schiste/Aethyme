"""Organization model for multi-tenancy."""

from sqlalchemy import Column, String, Integer, Boolean
from sqlalchemy.orm import relationship
from app.models.base import Base, TimestampMixin


class Organization(Base, TimestampMixin):
    """Organization model for multi-tenant SaaS."""

    __tablename__ = "organizations"

    id = Column(String(36), primary_key=True)  # UUID
    name = Column(String(255), nullable=False)
    slug = Column(String(255), unique=True, nullable=False, index=True)

    # Subscription
    plan = Column(String(50), default="free", nullable=False)  # free, pro, team, enterprise
    is_active = Column(Boolean, default=True, nullable=False)

    # Stripe integration
    stripe_customer_id = Column(String(255), unique=True, nullable=True)
    stripe_subscription_id = Column(String(255), unique=True, nullable=True)

    # Usage limits
    max_repositories = Column(Integer, default=3, nullable=False)
    max_queries_per_month = Column(Integer, default=1000, nullable=False)
    current_queries_this_month = Column(Integer, default=0, nullable=False)

    # Relationships
    users = relationship("User", back_populates="organization")
    repositories = relationship("Repository", back_populates="organization")
    api_keys = relationship("APIKey", back_populates="organization")

    def __repr__(self):
        return f"<Organization {self.name}>"
