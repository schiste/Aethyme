"""Tests for tenant isolation and multi-tenancy security.

These tests verify that tenant A cannot access tenant B's data
and that scoped tokens properly restrict access.
"""

import pytest
import uuid
from datetime import timedelta

from src.auth.oidc import JWTTokenGenerator
from src.auth.middleware import UserContext, verify_jwt_token
from src.auth.api_keys import APIKeyManager, verify_api_key, APIKeyRevokedError, APIKeyExpiredError
from src.graph.connection_pool import db_pool


class TestTenantIsolation:
    """Test tenant isolation via RLS policies."""

    @pytest.fixture
    def tenant_a_id(self):
        """Create tenant A."""
        tenant_id = str(uuid.uuid4())
        query = """
            INSERT INTO repograph.tenants (id, name, description)
            VALUES (%s, %s, %s)
            RETURNING id
        """
        db_pool.execute(query, (tenant_id, f"tenant_a_{tenant_id[:8]}", "Test Tenant A"), fetch=False)
        yield tenant_id

        # Cleanup
        db_pool.execute("DELETE FROM repograph.tenants WHERE id = %s", (tenant_id,), fetch=False)

    @pytest.fixture
    def tenant_b_id(self):
        """Create tenant B."""
        tenant_id = str(uuid.uuid4())
        query = """
            INSERT INTO repograph.tenants (id, name, description)
            VALUES (%s, %s, %s)
            RETURNING id
        """
        db_pool.execute(query, (tenant_id, f"tenant_b_{tenant_id[:8]}", "Test Tenant B"), fetch=False)
        yield tenant_id

        # Cleanup
        db_pool.execute("DELETE FROM repograph.tenants WHERE id = %s", (tenant_id,), fetch=False)

    @pytest.fixture
    def repo_tenant_a(self, tenant_a_id):
        """Create repository for tenant A."""
        repo_id = str(uuid.uuid4())
        query = """
            INSERT INTO repograph.repositories (id, tenant_id, name, path)
            VALUES (%s, %s, %s, %s)
            RETURNING id
        """
        db_pool.execute(
            query,
            (repo_id, tenant_a_id, "repo_a", "/path/to/repo_a"),
            fetch=False,
        )
        yield repo_id, tenant_a_id

    @pytest.fixture
    def repo_tenant_b(self, tenant_b_id):
        """Create repository for tenant B."""
        repo_id = str(uuid.uuid4())
        query = """
            INSERT INTO repograph.repositories (id, tenant_id, name, path)
            VALUES (%s, %s, %s, %s)
            RETURNING id
        """
        db_pool.execute(
            query,
            (repo_id, tenant_b_id, "repo_b", "/path/to/repo_b"),
            fetch=False,
        )
        yield repo_id, tenant_b_id

    def test_tenant_cannot_read_other_tenant_repos(self, repo_tenant_a, repo_tenant_b):
        """Test that tenant A cannot read tenant B's repositories."""
        repo_a_id, tenant_a_id = repo_tenant_a
        repo_b_id, tenant_b_id = repo_tenant_b

        # Set context for tenant A
        db_pool.execute(f"SET app.current_tenant = '{tenant_a_id}'", fetch=False)
        db_pool.execute("SET app.current_scopes = '[\"repo:read\"]'", fetch=False)

        # Query repositories - should only see tenant A's repos
        query = "SELECT id, name FROM repograph.repositories"
        results = db_pool.execute(query)

        # Should find repo A
        repo_ids = [str(r['id']) for r in results]
        assert repo_a_id in repo_ids, "Tenant A should see their own repo"
        assert repo_b_id not in repo_ids, "Tenant A should NOT see tenant B's repo"

        # Reset context
        db_pool.execute("RESET app.current_tenant", fetch=False)
        db_pool.execute("RESET app.current_scopes", fetch=False)

    def test_tenant_cannot_write_to_other_tenant_repos(self, repo_tenant_a, tenant_b_id):
        """Test that tenant B cannot modify tenant A's repositories."""
        repo_a_id, tenant_a_id = repo_tenant_a

        # Set context for tenant B with write scope
        db_pool.execute(f"SET app.current_tenant = '{tenant_b_id}'", fetch=False)
        db_pool.execute("SET app.current_scopes = '[\"repo:write\", \"org:admin\"]'", fetch=False)

        # Try to update tenant A's repo - should fail
        query = """
            UPDATE repograph.repositories
            SET name = 'hacked'
            WHERE id = %s
            RETURNING id
        """

        result = db_pool.execute(query, (repo_a_id,))

        # Update should return nothing (RLS blocks it)
        assert not result or len(result) == 0, "Tenant B should NOT be able to update tenant A's repo"

        # Verify repo was not modified
        db_pool.execute(f"SET app.current_tenant = '{tenant_a_id}'", fetch=False)
        check_query = "SELECT name FROM repograph.repositories WHERE id = %s"
        check_result = db_pool.execute(check_query, (repo_a_id,))

        assert check_result[0]['name'] == 'repo_a', "Repo name should not have changed"

        # Reset context
        db_pool.execute("RESET app.current_tenant", fetch=False)
        db_pool.execute("RESET app.current_scopes", fetch=False)

    def test_nodes_tenant_isolation(self, repo_tenant_a, repo_tenant_b):
        """Test that nodes are isolated by tenant."""
        repo_a_id, tenant_a_id = repo_tenant_a
        repo_b_id, tenant_b_id = repo_tenant_b

        # Create node for tenant A
        node_a_id = f"node_a_{uuid.uuid4().hex[:8]}"
        query = """
            INSERT INTO repograph.nodes (
                id, tenant_id, repository_id, symbol, file_path,
                line_number, column_number, kind, language
            ) VALUES (%s, %s, %s, %s, %s, %s, %s, %s, %s)
        """
        db_pool.execute(
            query,
            (node_a_id, tenant_a_id, repo_a_id, "test_symbol_a", "test.py", 1, 1, "function", "python"),
            fetch=False,
        )

        # Create node for tenant B
        node_b_id = f"node_b_{uuid.uuid4().hex[:8]}"
        db_pool.execute(
            query,
            (node_b_id, tenant_b_id, repo_b_id, "test_symbol_b", "test.py", 1, 1, "function", "python"),
            fetch=False,
        )

        # Set context for tenant A
        db_pool.execute(f"SET app.current_tenant = '{tenant_a_id}'", fetch=False)
        db_pool.execute("SET app.current_scopes = '[\"repo:read\"]'", fetch=False)

        # Query nodes - should only see tenant A's nodes
        query = "SELECT id FROM repograph.nodes"
        results = db_pool.execute(query)

        node_ids = [r['id'] for r in results]
        assert node_a_id in node_ids, "Tenant A should see their own nodes"
        assert node_b_id not in node_ids, "Tenant A should NOT see tenant B's nodes"

        # Reset context
        db_pool.execute("RESET app.current_tenant", fetch=False)
        db_pool.execute("RESET app.current_scopes", fetch=False)


class TestScopedTokens:
    """Test scoped token access control."""

    def test_read_only_token_cannot_write(self):
        """Test that read-only scoped token cannot perform write operations."""
        user_id = str(uuid.uuid4())
        org_id = str(uuid.uuid4())

        # Create read-only token
        token = JWTTokenGenerator.create_token(
            user_id=user_id,
            org_id=org_id,
            scopes=["repo:read"],
        )

        # Decode token
        payload = JWTTokenGenerator.decode_token(token)

        assert "repo:read" in payload["scopes"]
        assert "repo:write" not in payload["scopes"]

        # Create user context
        user = UserContext(
            user_id=user_id,
            org_id=org_id,
            scopes=payload["scopes"],
        )

        # Verify scopes
        assert user.has_scope("repo:read")
        assert not user.has_scope("repo:write")
        assert not user.has_scope("org:admin")

    def test_write_token_has_read_access(self):
        """Test that write token can also read."""
        user_id = str(uuid.uuid4())
        org_id = str(uuid.uuid4())

        # Create write token
        token = JWTTokenGenerator.create_token(
            user_id=user_id,
            org_id=org_id,
            scopes=["repo:read", "repo:write"],
        )

        # Decode token
        payload = JWTTokenGenerator.decode_token(token)

        # Create user context
        user = UserContext(
            user_id=user_id,
            org_id=org_id,
            scopes=payload["scopes"],
        )

        # Verify scopes
        assert user.has_scope("repo:read")
        assert user.has_scope("repo:write")

    def test_admin_scope_grants_all_permissions(self):
        """Test that org:admin scope grants all permissions."""
        user_id = str(uuid.uuid4())
        org_id = str(uuid.uuid4())

        # Create admin token
        token = JWTTokenGenerator.create_token(
            user_id=user_id,
            org_id=org_id,
            scopes=["org:admin"],
        )

        # Decode token
        payload = JWTTokenGenerator.decode_token(token)

        # Create user context
        user = UserContext(
            user_id=user_id,
            org_id=org_id,
            scopes=payload["scopes"],
        )

        # Admin should have all scopes
        assert user.has_scope("repo:read")
        assert user.has_scope("repo:write")
        assert user.has_scope("org:admin")
        assert user.has_scope("any:scope")  # Admin has everything

    def test_multiple_scopes(self):
        """Test token with multiple scopes."""
        user_id = str(uuid.uuid4())
        org_id = str(uuid.uuid4())

        scopes = ["repo:read", "repo:write", "audit:read"]

        token = JWTTokenGenerator.create_token(
            user_id=user_id,
            org_id=org_id,
            scopes=scopes,
        )

        user = UserContext(
            user_id=user_id,
            org_id=org_id,
            scopes=scopes,
        )

        # Check individual scopes
        assert user.has_scope("repo:read")
        assert user.has_scope("repo:write")
        assert user.has_scope("audit:read")

        # Check has_any_scope
        assert user.has_any_scope(["repo:read", "nonexistent"])
        assert not user.has_any_scope(["nonexistent", "also_nonexistent"])

        # Check has_all_scopes
        assert user.has_all_scopes(["repo:read", "repo:write"])
        assert not user.has_all_scopes(["repo:read", "nonexistent"])


class TestAPIKeyAuth:
    """Test API key authentication and scoping."""

    @pytest.fixture
    def test_tenant(self):
        """Create test tenant."""
        tenant_id = str(uuid.uuid4())
        query = """
            INSERT INTO repograph.tenants (id, name, description)
            VALUES (%s, %s, %s)
            RETURNING id
        """
        db_pool.execute(query, (tenant_id, f"api_test_{tenant_id[:8]}", "API Key Test Tenant"), fetch=False)
        yield tenant_id

        # Cleanup
        db_pool.execute("DELETE FROM repograph.tenants WHERE id = %s", (tenant_id,), fetch=False)

    def test_api_key_creation_and_verification(self, test_tenant):
        """Test creating and verifying API keys."""
        # Create API key
        key_data = APIKeyManager.create(
            tenant_id=test_tenant,
            name="Test Key",
            scopes=["repo:read", "repo:write"],
        )

        assert "api_key" in key_data
        assert key_data["api_key"].startswith("rg_live_")

        # Verify API key
        verified = verify_api_key(key_data["api_key"])
        assert verified is not None
        assert str(verified["tenant_id"]) == test_tenant

    def test_api_key_scoping(self, test_tenant):
        """Test API key with limited scopes."""
        # Create read-only API key
        key_data = APIKeyManager.create(
            tenant_id=test_tenant,
            name="Read-Only Key",
            scopes=["repo:read"],
        )

        verified = verify_api_key(key_data["api_key"])

        import json
        scopes = verified["scopes"]
        if isinstance(scopes, str):
            scopes = json.loads(scopes)

        assert "repo:read" in scopes
        assert "repo:write" not in scopes

    def test_api_key_revocation(self, test_tenant):
        """Test revoking an API key."""
        # Create API key
        key_data = APIKeyManager.create(
            tenant_id=test_tenant,
            name="Revoke Test Key",
            scopes=["repo:read"],
        )

        # Verify it works
        verified = verify_api_key(key_data["api_key"])
        assert verified is not None

        # Revoke it
        revoked = APIKeyManager.revoke(key_data["key_id"], test_tenant)
        assert revoked is True

        # Verify it no longer works
        with pytest.raises(APIKeyRevokedError):
            verify_api_key(key_data["api_key"])

    def test_api_key_rotation(self, test_tenant):
        """Test rotating an API key."""
        # Create API key
        old_key_data = APIKeyManager.create(
            tenant_id=test_tenant,
            name="Rotation Test Key",
            scopes=["repo:read", "repo:write"],
        )

        old_key = old_key_data["api_key"]

        # Rotate key
        new_key_data = APIKeyManager.rotate(
            key_id=old_key_data["key_id"],
            tenant_id=test_tenant,
        )

        new_key = new_key_data["api_key"]

        # Old key should be revoked
        with pytest.raises(APIKeyRevokedError):
            verify_api_key(old_key)

        # New key should work
        verified = verify_api_key(new_key)
        assert verified is not None

    def test_api_key_list(self, test_tenant):
        """Test listing API keys for a tenant."""
        # Create multiple keys
        key1 = APIKeyManager.create(
            tenant_id=test_tenant,
            name="Key 1",
            scopes=["repo:read"],
        )

        key2 = APIKeyManager.create(
            tenant_id=test_tenant,
            name="Key 2",
            scopes=["repo:read", "repo:write"],
        )

        # List keys
        keys = APIKeyManager.list_keys(test_tenant)

        assert len(keys) >= 2
        key_names = [k["name"] for k in keys]
        assert "Key 1" in key_names
        assert "Key 2" in key_names

        # Verify actual keys are not returned
        for key in keys:
            assert "api_key" not in key
            assert "key_id" in key


class TestRateLimit:
    """Test rate limiting functionality."""

    @pytest.mark.asyncio
    async def test_rate_limit_basic(self):
        """Test basic rate limiting."""
        from src.middleware.rate_limit import RateLimiter

        limiter = RateLimiter(default_limit=5, default_window=60)
        await limiter.connect()

        identifier = "test_user_123"
        endpoint = "/api/test"

        # First 5 requests should succeed
        for i in range(5):
            allowed, remaining, _ = await limiter.check_rate_limit(
                identifier, endpoint, limit=5, window=60
            )
            assert allowed is True
            assert remaining == 4 - i

        # 6th request should be rate limited
        allowed, remaining, retry_after = await limiter.check_rate_limit(
            identifier, endpoint, limit=5, window=60
        )
        assert allowed is False
        assert remaining == 0
        assert retry_after > 0

        # Reset and try again
        await limiter.reset(identifier, endpoint)

        allowed, remaining, _ = await limiter.check_rate_limit(
            identifier, endpoint, limit=5, window=60
        )
        assert allowed is True

        await limiter.disconnect()

    @pytest.mark.asyncio
    async def test_rate_limit_different_endpoints(self):
        """Test rate limits are separate per endpoint."""
        from src.middleware.rate_limit import RateLimiter

        limiter = RateLimiter(default_limit=3, default_window=60)
        await limiter.connect()

        identifier = "test_user_456"

        # Use up limit on endpoint 1
        for i in range(3):
            allowed, _, _ = await limiter.check_rate_limit(
                identifier, "/api/endpoint1", limit=3, window=60
            )
            assert allowed is True

        # Endpoint 1 should be rate limited
        allowed, _, _ = await limiter.check_rate_limit(
            identifier, "/api/endpoint1", limit=3, window=60
        )
        assert allowed is False

        # Endpoint 2 should still allow requests
        allowed, _, _ = await limiter.check_rate_limit(
            identifier, "/api/endpoint2", limit=3, window=60
        )
        assert allowed is True

        await limiter.disconnect()


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
