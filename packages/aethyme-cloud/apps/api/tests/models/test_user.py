"""
Tests for User model.
"""
import pytest
from sqlalchemy.exc import IntegrityError
from app.models import User, Organization


class TestUserModel:
    """Test User model validation and constraints."""

    @pytest.mark.asyncio
    async def test_create_user(self, db_session, test_organization):
        """Test creating a valid user."""
        user = User(
            id="user-123",
            email="test@example.com",
            organization_id=test_organization.id,
            is_active=True,
        )
        db_session.add(user)
        await db_session.commit()
        await db_session.refresh(user)

        assert user.id == "user-123"
        assert user.email == "test@example.com"
        assert user.organization_id == test_organization.id
        assert user.is_active is True

    @pytest.mark.asyncio
    async def test_user_email_required(self, db_session, test_organization):
        """Test that email is required."""
        user = User(
            id="user-no-email",
            organization_id=test_organization.id,
        )
        db_session.add(user)

        with pytest.raises(IntegrityError):
            await db_session.commit()

    @pytest.mark.asyncio
    async def test_user_email_unique(self, db_session, test_organization):
        """Test that email must be unique."""
        # Create first user
        user1 = User(
            id="user-1",
            email="duplicate@example.com",
            organization_id=test_organization.id,
        )
        db_session.add(user1)
        await db_session.commit()

        # Try to create second user with same email
        user2 = User(
            id="user-2",
            email="duplicate@example.com",
            organization_id=test_organization.id,
        )
        db_session.add(user2)

        with pytest.raises(IntegrityError):
            await db_session.commit()

    @pytest.mark.asyncio
    async def test_user_organization_relationship(self, db_session, test_organization):
        """Test user-organization relationship."""
        user = User(
            id="user-rel",
            email="relation@example.com",
            organization_id=test_organization.id,
        )
        db_session.add(user)
        await db_session.commit()
        await db_session.refresh(user)

        # Load relationship
        await db_session.refresh(user, ["organization"])
        assert user.organization.id == test_organization.id
        assert user.organization.name == test_organization.name

    @pytest.mark.asyncio
    async def test_user_defaults(self, db_session, test_organization):
        """Test default values for user fields."""
        user = User(
            id="user-defaults",
            email="defaults@example.com",
            organization_id=test_organization.id,
        )
        db_session.add(user)
        await db_session.commit()
        await db_session.refresh(user)

        # Check defaults
        assert user.is_active is True  # Default should be True
        assert user.created_at is not None
        assert user.updated_at is not None

    @pytest.mark.asyncio
    async def test_user_timestamps(self, db_session, test_organization):
        """Test that timestamps are set correctly."""
        user = User(
            id="user-timestamps",
            email="timestamps@example.com",
            organization_id=test_organization.id,
        )
        db_session.add(user)
        await db_session.commit()
        await db_session.refresh(user)

        assert user.created_at is not None
        assert user.updated_at is not None
        assert user.created_at == user.updated_at

    @pytest.mark.asyncio
    async def test_user_str_representation(self, db_session, test_organization):
        """Test user string representation."""
        user = User(
            id="user-str",
            email="str@example.com",
            organization_id=test_organization.id,
        )
        db_session.add(user)
        await db_session.commit()

        # String representation should include email
        user_str = str(user)
        assert "str@example.com" in user_str or "user-str" in user_str


class TestUserValidation:
    """Test User model validation logic."""

    @pytest.mark.asyncio
    async def test_invalid_email_format(self, db_session, test_organization):
        """Test that invalid email format is handled."""
        # Note: This depends on whether email validation is done at model or application level
        user = User(
            id="user-invalid-email",
            email="not-an-email",
            organization_id=test_organization.id,
        )
        db_session.add(user)

        # This might pass at model level but should be caught at application level
        await db_session.commit()
        assert user.email == "not-an-email"

    @pytest.mark.asyncio
    async def test_inactive_user(self, db_session, test_organization):
        """Test creating an inactive user."""
        user = User(
            id="user-inactive",
            email="inactive@example.com",
            organization_id=test_organization.id,
            is_active=False,
        )
        db_session.add(user)
        await db_session.commit()
        await db_session.refresh(user)

        assert user.is_active is False


class TestUserQueries:
    """Test User model queries."""

    @pytest.mark.asyncio
    async def test_query_by_email(self, db_session, test_organization):
        """Test querying user by email."""
        from sqlalchemy import select

        user = User(
            id="user-query",
            email="query@example.com",
            organization_id=test_organization.id,
        )
        db_session.add(user)
        await db_session.commit()

        # Query by email
        result = await db_session.execute(
            select(User).where(User.email == "query@example.com")
        )
        found_user = result.scalar_one_or_none()

        assert found_user is not None
        assert found_user.id == "user-query"
        assert found_user.email == "query@example.com"

    @pytest.mark.asyncio
    async def test_query_by_organization(self, db_session, test_organization):
        """Test querying users by organization."""
        from sqlalchemy import select

        # Create multiple users in same organization
        user1 = User(
            id="user-org-1",
            email="org1@example.com",
            organization_id=test_organization.id,
        )
        user2 = User(
            id="user-org-2",
            email="org2@example.com",
            organization_id=test_organization.id,
        )
        db_session.add_all([user1, user2])
        await db_session.commit()

        # Query users by organization
        result = await db_session.execute(
            select(User).where(User.organization_id == test_organization.id)
        )
        users = result.scalars().all()

        assert len(users) >= 2
        org_user_ids = [u.id for u in users]
        assert "user-org-1" in org_user_ids
        assert "user-org-2" in org_user_ids
