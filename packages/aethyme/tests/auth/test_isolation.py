"""Tests for tenant isolation and scoped authentication."""

import json
import uuid
from collections.abc import Generator
from typing import cast

import pytest

from src.auth.api_keys import APIKeyManager, APIKeyRevokedError, verify_api_key
from src.auth.middleware import UserContext
from src.auth.oidc import JWTTokenGenerator
from tests.support import (
    create_node,
    create_org_and_tenant,
    create_repository,
    delete_org,
    execute_as,
)
from tests.support.auth_db import TenantRecord

RepoFixture = tuple[str, TenantRecord]


def _scope_list(raw_scopes: object) -> list[str]:
    parsed: object = raw_scopes
    if isinstance(parsed, str):
        parsed = json.loads(parsed)
    if not isinstance(parsed, list):
        raise TypeError(f"Expected list[str] scopes, got {type(parsed).__name__}")
    parsed_list = cast(list[object], parsed)
    return [str(scope) for scope in parsed_list]


class TestTenantIsolation:
    """Test tenant isolation via RLS policies."""

    @pytest.fixture
    def tenant_a(self) -> Generator[TenantRecord, None, None]:
        tenant = create_org_and_tenant("tenant-a")
        yield tenant
        delete_org(tenant["org_id"])

    @pytest.fixture
    def tenant_b(self) -> Generator[TenantRecord, None, None]:
        tenant = create_org_and_tenant("tenant-b")
        yield tenant
        delete_org(tenant["org_id"])

    @pytest.fixture
    def repo_tenant_a(self, tenant_a: TenantRecord) -> Generator[RepoFixture, None, None]:
        repo_id = create_repository(
            org_id=tenant_a["org_id"],
            tenant_id=tenant_a["tenant_id"],
            name="repo_a",
            path="/path/to/repo_a",
        )
        yield repo_id, tenant_a

    @pytest.fixture
    def repo_tenant_b(self, tenant_b: TenantRecord) -> Generator[RepoFixture, None, None]:
        repo_id = create_repository(
            org_id=tenant_b["org_id"],
            tenant_id=tenant_b["tenant_id"],
            name="repo_b",
            path="/path/to/repo_b",
        )
        yield repo_id, tenant_b

    def test_tenant_cannot_read_other_tenant_repos(
        self,
        repo_tenant_a: RepoFixture,
        repo_tenant_b: RepoFixture,
    ) -> None:
        repo_a_id, tenant_a = repo_tenant_a
        repo_b_id, _ = repo_tenant_b

        results = execute_as(
            tenant_id=tenant_a["tenant_id"],
            org_id=tenant_a["org_id"],
            scopes=["repo:read"],
            query="SELECT id, name FROM aethyme.repositories",
        )

        repo_ids = [str(row["id"]) for row in results or []]
        assert repo_a_id in repo_ids
        assert repo_b_id not in repo_ids

    def test_tenant_cannot_write_to_other_tenant_repos(
        self,
        repo_tenant_a: RepoFixture,
        tenant_b: TenantRecord,
    ) -> None:
        repo_a_id, tenant_a = repo_tenant_a

        result = execute_as(
            tenant_id=tenant_b["tenant_id"],
            org_id=tenant_b["org_id"],
            scopes=["repo:write", "org:admin"],
            query="""
                UPDATE aethyme.repositories
                SET name = 'hacked'
                WHERE id = %s
                RETURNING id
            """,
            params=(repo_a_id,),
        )

        assert not result or len(result) == 0

        check_result = execute_as(
            tenant_id=tenant_a["tenant_id"],
            org_id=tenant_a["org_id"],
            scopes=["repo:read"],
            query="SELECT name FROM aethyme.repositories WHERE id = %s",
            params=(repo_a_id,),
        )

        assert check_result is not None
        assert check_result[0]["name"] == "repo_a"

    def test_nodes_tenant_isolation(
        self,
        repo_tenant_a: RepoFixture,
        repo_tenant_b: RepoFixture,
    ) -> None:
        repo_a_id, tenant_a = repo_tenant_a
        repo_b_id, tenant_b = repo_tenant_b

        node_a_id = f"node_a_{uuid.uuid4().hex[:8]}"
        create_node(
            org_id=tenant_a["org_id"],
            tenant_id=tenant_a["tenant_id"],
            repository_id=repo_a_id,
            node_id=node_a_id,
            symbol="test_symbol_a",
            file_path="test.py",
        )

        node_b_id = f"node_b_{uuid.uuid4().hex[:8]}"
        create_node(
            org_id=tenant_b["org_id"],
            tenant_id=tenant_b["tenant_id"],
            repository_id=repo_b_id,
            node_id=node_b_id,
            symbol="test_symbol_b",
            file_path="test.py",
        )

        results = execute_as(
            tenant_id=tenant_a["tenant_id"],
            org_id=tenant_a["org_id"],
            scopes=["repo:read"],
            query="SELECT id FROM aethyme.nodes",
        )

        node_ids = [row["id"] for row in results or []]
        assert node_a_id in node_ids
        assert node_b_id not in node_ids


class TestScopedTokens:
    """Test scoped token access control."""

    def test_read_only_token_cannot_write(self) -> None:
        user_id = str(uuid.uuid4())
        org_id = str(uuid.uuid4())
        tenant_id = str(uuid.uuid4())

        token = JWTTokenGenerator.create_token(
            user_id=user_id,
            tenant_id=tenant_id,
            org_id=org_id,
            scopes=["repo:read"],
        )
        payload = JWTTokenGenerator.decode_token(token)

        user = UserContext(
            user_id=user_id,
            tenant_id=tenant_id,
            org_id=org_id,
            scopes=_scope_list(payload["scopes"]),
        )

        assert user.has_scope("repo:read")
        assert not user.has_scope("repo:write")
        assert not user.has_scope("org:admin")

    def test_write_token_has_read_access(self) -> None:
        user_id = str(uuid.uuid4())
        org_id = str(uuid.uuid4())
        tenant_id = str(uuid.uuid4())

        token = JWTTokenGenerator.create_token(
            user_id=user_id,
            tenant_id=tenant_id,
            org_id=org_id,
            scopes=["repo:read", "repo:write"],
        )
        payload = JWTTokenGenerator.decode_token(token)

        user = UserContext(
            user_id=user_id,
            tenant_id=tenant_id,
            org_id=org_id,
            scopes=_scope_list(payload["scopes"]),
        )

        assert user.has_scope("repo:read")
        assert user.has_scope("repo:write")

    def test_admin_scope_grants_all_permissions(self) -> None:
        user_id = str(uuid.uuid4())
        org_id = str(uuid.uuid4())
        tenant_id = str(uuid.uuid4())

        token = JWTTokenGenerator.create_token(
            user_id=user_id,
            tenant_id=tenant_id,
            org_id=org_id,
            scopes=["org:admin"],
        )
        payload = JWTTokenGenerator.decode_token(token)

        user = UserContext(
            user_id=user_id,
            tenant_id=tenant_id,
            org_id=org_id,
            scopes=_scope_list(payload["scopes"]),
        )

        assert user.has_scope("repo:read")
        assert user.has_scope("repo:write")
        assert user.has_scope("org:admin")
        assert user.has_scope("any:scope")

    def test_multiple_scopes(self) -> None:
        user_id = str(uuid.uuid4())
        org_id = str(uuid.uuid4())
        tenant_id = str(uuid.uuid4())
        scopes = ["repo:read", "repo:write", "audit:read"]

        JWTTokenGenerator.create_token(
            user_id=user_id,
            tenant_id=tenant_id,
            org_id=org_id,
            scopes=scopes,
        )

        user = UserContext(
            user_id=user_id,
            tenant_id=tenant_id,
            org_id=org_id,
            scopes=scopes,
        )

        assert user.has_scope("repo:read")
        assert user.has_scope("repo:write")
        assert user.has_scope("audit:read")
        assert user.has_any_scope(["repo:read", "nonexistent"])
        assert not user.has_any_scope(["nonexistent", "also_nonexistent"])
        assert user.has_all_scopes(["repo:read", "repo:write"])
        assert not user.has_all_scopes(["repo:read", "nonexistent"])


class TestAPIKeyAuth:
    """Test API key authentication and scoping."""

    @pytest.fixture
    def test_tenant(self) -> Generator[TenantRecord, None, None]:
        tenant = create_org_and_tenant("api-test")
        yield tenant
        delete_org(tenant["org_id"])

    def test_api_key_creation_and_verification(self, test_tenant: TenantRecord) -> None:
        key_data = APIKeyManager.create(
            tenant_id=test_tenant["tenant_id"],
            name="Test Key",
            scopes=["repo:read", "repo:write"],
        )

        assert key_data["api_key"].startswith("rg_live_")

        verified = verify_api_key(key_data["api_key"])
        assert verified is not None
        assert str(verified["tenant_id"]) == test_tenant["tenant_id"]

    def test_api_key_scoping(self, test_tenant: TenantRecord) -> None:
        key_data = APIKeyManager.create(
            tenant_id=test_tenant["tenant_id"],
            name="Read-Only Key",
            scopes=["repo:read"],
        )

        verified = verify_api_key(key_data["api_key"])
        assert verified is not None
        scopes = _scope_list(verified["scopes"])

        assert "repo:read" in scopes
        assert "repo:write" not in scopes

    def test_api_key_revocation(self, test_tenant: TenantRecord) -> None:
        key_data = APIKeyManager.create(
            tenant_id=test_tenant["tenant_id"],
            name="Revoke Test Key",
            scopes=["repo:read"],
        )

        verified = verify_api_key(key_data["api_key"])
        assert verified is not None

        revoked = APIKeyManager.revoke(key_data["key_id"], test_tenant["tenant_id"])
        assert revoked is True

        with pytest.raises(APIKeyRevokedError):
            verify_api_key(key_data["api_key"])

    def test_api_key_rotation(self, test_tenant: TenantRecord) -> None:
        old_key_data = APIKeyManager.create(
            tenant_id=test_tenant["tenant_id"],
            name="Rotation Test Key",
            scopes=["repo:read", "repo:write"],
        )
        old_key = old_key_data["api_key"]

        new_key_data = APIKeyManager.rotate(
            key_id=old_key_data["key_id"],
            tenant_id=test_tenant["tenant_id"],
        )
        new_key = new_key_data["api_key"]

        with pytest.raises(APIKeyRevokedError):
            verify_api_key(old_key)

        verified = verify_api_key(new_key)
        assert verified is not None

    def test_api_key_list(self, test_tenant: TenantRecord) -> None:
        APIKeyManager.create(
            tenant_id=test_tenant["tenant_id"],
            name="Key 1",
            scopes=["repo:read"],
        )
        APIKeyManager.create(
            tenant_id=test_tenant["tenant_id"],
            name="Key 2",
            scopes=["repo:read", "repo:write"],
        )

        keys = APIKeyManager.list_keys(test_tenant["tenant_id"])

        assert len(keys) >= 2
        key_names = [key["name"] for key in keys]
        assert "Key 1" in key_names
        assert "Key 2" in key_names

        for key in keys:
            assert "api_key" not in key
            assert "key_id" in key

