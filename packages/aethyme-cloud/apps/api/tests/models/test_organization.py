"""
Tests for Organization model.
"""
import pytest
from sqlalchemy.exc import IntegrityError
from app.models import Organization, User, Repository, APIKey


class TestOrganizationModel:
    """Test Organization model validation and constraints."""

    @pytest.mark.asyncio
    async def test_create_organization(self, db_session):
        """Test creating a valid organization."""
        org = Organization(
            id="org-123",
            name="Test Organization",
            slug="test-org",
            plan="free",
            is_active=True,
        )
        db_session.add(org)
        await db_session.commit()
        await db_session.refresh(org)

        assert org.id == "org-123"
        assert org.name == "Test Organization"
        assert org.slug == "test-org"
        assert org.plan == "free"
        assert org.is_active is True

    @pytest.mark.asyncio
    async def test_organization_name_required(self, db_session):
        """Test that name is required."""
        org = Organization(
            id="org-no-name",
            slug="no-name-org",
        )
        db_session.add(org)

        with pytest.raises(IntegrityError):
            await db_session.commit()

    @pytest.mark.asyncio
    async def test_organization_slug_required(self, db_session):
        """Test that slug is required."""
        org = Organization(
            id="org-no-slug",
            name="No Slug Org",
        )
        db_session.add(org)

        with pytest.raises(IntegrityError):
            await db_session.commit()

    @pytest.mark.asyncio
    async def test_organization_slug_unique(self, db_session):
        """Test that slug must be unique."""
        # Create first organization
        org1 = Organization(
            id="org-1",
            name="Organization 1",
            slug="duplicate-slug",
        )
        db_session.add(org1)
        await db_session.commit()

        # Try to create second organization with same slug
        org2 = Organization(
            id="org-2",
            name="Organization 2",
            slug="duplicate-slug",
        )
        db_session.add(org2)

        with pytest.raises(IntegrityError):
            await db_session.commit()

    @pytest.mark.asyncio
    async def test_organization_defaults(self, db_session):
        """Test default values for organization fields."""
        org = Organization(
            id="org-defaults",
            name="Defaults Org",
            slug="defaults-org",
        )
        db_session.add(org)
        await db_session.commit()
        await db_session.refresh(org)

        # Check defaults
        assert org.plan == "free"
        assert org.is_active is True
        assert org.max_repositories == 3
        assert org.max_queries_per_month == 1000
        assert org.current_queries_this_month == 0
        assert org.stripe_customer_id is None
        assert org.stripe_subscription_id is None
        assert org.created_at is not None
        assert org.updated_at is not None

    @pytest.mark.asyncio
    async def test_organization_timestamps(self, db_session):
        """Test that timestamps are set correctly."""
        org = Organization(
            id="org-timestamps",
            name="Timestamps Org",
            slug="timestamps-org",
        )
        db_session.add(org)
        await db_session.commit()
        await db_session.refresh(org)

        assert org.created_at is not None
        assert org.updated_at is not None
        assert org.created_at == org.updated_at

    @pytest.mark.asyncio
    async def test_organization_str_representation(self, db_session):
        """Test organization string representation."""
        org = Organization(
            id="org-str",
            name="String Org",
            slug="string-org",
        )
        db_session.add(org)
        await db_session.commit()

        # String representation should include name
        org_str = str(org)
        assert "String Org" in org_str or "org-str" in org_str


class TestOrganizationPlans:
    """Test Organization plan functionality."""

    @pytest.mark.asyncio
    async def test_free_plan_limits(self, db_session):
        """Test free plan has correct limits."""
        org = Organization(
            id="org-free",
            name="Free Plan Org",
            slug="free-plan",
            plan="free",
        )
        db_session.add(org)
        await db_session.commit()
        await db_session.refresh(org)

        assert org.plan == "free"
        assert org.max_repositories == 3
        assert org.max_queries_per_month == 1000

    @pytest.mark.asyncio
    async def test_pro_plan(self, db_session):
        """Test pro plan creation."""
        org = Organization(
            id="org-pro",
            name="Pro Plan Org",
            slug="pro-plan",
            plan="pro",
            max_repositories=20,
            max_queries_per_month=10000,
        )
        db_session.add(org)
        await db_session.commit()
        await db_session.refresh(org)

        assert org.plan == "pro"
        assert org.max_repositories == 20
        assert org.max_queries_per_month == 10000

    @pytest.mark.asyncio
    async def test_enterprise_plan(self, db_session):
        """Test enterprise plan creation."""
        org = Organization(
            id="org-enterprise",
            name="Enterprise Plan Org",
            slug="enterprise-plan",
            plan="enterprise",
            max_repositories=100,
            max_queries_per_month=100000,
        )
        db_session.add(org)
        await db_session.commit()
        await db_session.refresh(org)

        assert org.plan == "enterprise"
        assert org.max_repositories == 100
        assert org.max_queries_per_month == 100000


class TestOrganizationRelationships:
    """Test Organization model relationships."""

    @pytest.mark.asyncio
    async def test_organization_users_relationship(self, db_session):
        """Test organization-users relationship."""
        org = Organization(
            id="org-users-rel",
            name="Users Relationship Org",
            slug="users-relationship",
        )
        db_session.add(org)
        await db_session.commit()

        # Create users for this organization
        user1 = User(
            id="user-1",
            email="user1@example.com",
            organization_id=org.id,
        )
        user2 = User(
            id="user-2",
            email="user2@example.com",
            organization_id=org.id,
        )
        db_session.add_all([user1, user2])
        await db_session.commit()

        # Refresh and load relationship
        await db_session.refresh(org, ["users"])
        assert len(org.users) == 2
        user_emails = [u.email for u in org.users]
        assert "user1@example.com" in user_emails
        assert "user2@example.com" in user_emails

    @pytest.mark.asyncio
    async def test_organization_repositories_relationship(self, db_session, test_organization):
        """Test organization-repositories relationship."""
        # Create repositories for this organization
        repo1 = Repository(
            id="repo-1",
            organization_id=test_organization.id,
            name="repo1",
            full_name="org/repo1",
            url="https://github.com/org/repo1",
            provider="github",
            provider_id="1234",
        )
        repo2 = Repository(
            id="repo-2",
            organization_id=test_organization.id,
            name="repo2",
            full_name="org/repo2",
            url="https://github.com/org/repo2",
            provider="github",
            provider_id="5678",
        )
        db_session.add_all([repo1, repo2])
        await db_session.commit()

        # Refresh and load relationship
        await db_session.refresh(test_organization, ["repositories"])
        assert len(test_organization.repositories) == 2
        repo_names = [r.name for r in test_organization.repositories]
        assert "repo1" in repo_names
        assert "repo2" in repo_names


class TestOrganizationQueries:
    """Test Organization model queries."""

    @pytest.mark.asyncio
    async def test_query_by_slug(self, db_session):
        """Test querying organization by slug."""
        from sqlalchemy import select

        org = Organization(
            id="org-query-slug",
            name="Query Slug Org",
            slug="query-slug",
        )
        db_session.add(org)
        await db_session.commit()

        # Query by slug
        result = await db_session.execute(
            select(Organization).where(Organization.slug == "query-slug")
        )
        found_org = result.scalar_one_or_none()

        assert found_org is not None
        assert found_org.id == "org-query-slug"
        assert found_org.slug == "query-slug"

    @pytest.mark.asyncio
    async def test_query_active_organizations(self, db_session):
        """Test querying active organizations."""
        from sqlalchemy import select

        # Create active and inactive organizations
        org1 = Organization(
            id="org-active-1",
            name="Active Org 1",
            slug="active-1",
            is_active=True,
        )
        org2 = Organization(
            id="org-inactive",
            name="Inactive Org",
            slug="inactive-1",
            is_active=False,
        )
        org3 = Organization(
            id="org-active-2",
            name="Active Org 2",
            slug="active-2",
            is_active=True,
        )
        db_session.add_all([org1, org2, org3])
        await db_session.commit()

        # Query active organizations
        result = await db_session.execute(
            select(Organization).where(Organization.is_active == True)
        )
        active_orgs = result.scalars().all()

        assert len(active_orgs) >= 2
        active_ids = [o.id for o in active_orgs]
        assert "org-active-1" in active_ids
        assert "org-active-2" in active_ids
        assert "org-inactive" not in active_ids

    @pytest.mark.asyncio
    async def test_query_by_plan(self, db_session):
        """Test querying organizations by plan."""
        from sqlalchemy import select

        # Create organizations with different plans
        org_free = Organization(
            id="org-free-query",
            name="Free Org",
            slug="free-query",
            plan="free",
        )
        org_pro = Organization(
            id="org-pro-query",
            name="Pro Org",
            slug="pro-query",
            plan="pro",
        )
        db_session.add_all([org_free, org_pro])
        await db_session.commit()

        # Query pro organizations
        result = await db_session.execute(
            select(Organization).where(Organization.plan == "pro")
        )
        pro_orgs = result.scalars().all()

        pro_ids = [o.id for o in pro_orgs]
        assert "org-pro-query" in pro_ids


class TestOrganizationUsage:
    """Test Organization usage tracking."""

    @pytest.mark.asyncio
    async def test_track_query_usage(self, db_session):
        """Test tracking query usage."""
        org = Organization(
            id="org-usage",
            name="Usage Tracking Org",
            slug="usage-tracking",
            current_queries_this_month=0,
            max_queries_per_month=1000,
        )
        db_session.add(org)
        await db_session.commit()

        # Simulate query usage
        org.current_queries_this_month = 50
        await db_session.commit()
        await db_session.refresh(org)

        assert org.current_queries_this_month == 50

    @pytest.mark.asyncio
    async def test_check_query_limit_exceeded(self, db_session):
        """Test checking if query limit is exceeded."""
        org = Organization(
            id="org-limit",
            name="Limit Check Org",
            slug="limit-check",
            current_queries_this_month=1001,
            max_queries_per_month=1000,
        )
        db_session.add(org)
        await db_session.commit()
        await db_session.refresh(org)

        # Check if limit exceeded
        assert org.current_queries_this_month > org.max_queries_per_month


class TestOrganizationStripe:
    """Test Organization Stripe integration."""

    @pytest.mark.asyncio
    async def test_stripe_customer_id(self, db_session):
        """Test setting Stripe customer ID."""
        org = Organization(
            id="org-stripe",
            name="Stripe Org",
            slug="stripe-org",
            stripe_customer_id="cus_123456789",
        )
        db_session.add(org)
        await db_session.commit()
        await db_session.refresh(org)

        assert org.stripe_customer_id == "cus_123456789"

    @pytest.mark.asyncio
    async def test_stripe_customer_id_unique(self, db_session):
        """Test that Stripe customer ID must be unique."""
        # Create first organization with Stripe customer ID
        org1 = Organization(
            id="org-stripe-1",
            name="Stripe Org 1",
            slug="stripe-org-1",
            stripe_customer_id="cus_duplicate",
        )
        db_session.add(org1)
        await db_session.commit()

        # Try to create second organization with same Stripe customer ID
        org2 = Organization(
            id="org-stripe-2",
            name="Stripe Org 2",
            slug="stripe-org-2",
            stripe_customer_id="cus_duplicate",
        )
        db_session.add(org2)

        with pytest.raises(IntegrityError):
            await db_session.commit()

    @pytest.mark.asyncio
    async def test_stripe_subscription_id(self, db_session):
        """Test setting Stripe subscription ID."""
        org = Organization(
            id="org-sub",
            name="Subscription Org",
            slug="subscription-org",
            stripe_subscription_id="sub_123456789",
        )
        db_session.add(org)
        await db_session.commit()
        await db_session.refresh(org)

        assert org.stripe_subscription_id == "sub_123456789"
