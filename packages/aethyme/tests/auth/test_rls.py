"""Tests for row-level security on retained tenant-scoped graph tables."""

import uuid
from collections.abc import Generator
from typing import TypedDict, cast

import psycopg2
import pytest

from src.graph.connection_pool import db_pool
from tests.support import (
    create_edge,
    create_node,
    create_org_and_tenant,
    create_repository,
    delete_org,
    execute_as,
)
from tests.support.auth_db import RUNTIME_ROLE, DBPoolProtocol, TenantRecord


class TenantSetup(TypedDict):
    tenant_a: TenantRecord
    tenant_b: TenantRecord


class RepoSetup(TenantSetup):
    repo_a: str
    repo_b: str


typed_db_pool = cast(DBPoolProtocol, db_pool)


@pytest.fixture
def setup_tenants() -> Generator[TenantSetup, None, None]:
    """Create two test tenants."""
    tenant_a = create_org_and_tenant("rls-a")
    tenant_b = create_org_and_tenant("rls-b")
    yield {"tenant_a": tenant_a, "tenant_b": tenant_b}
    delete_org(tenant_a["org_id"])
    delete_org(tenant_b["org_id"])


@pytest.fixture
def setup_repos(setup_tenants: TenantSetup) -> RepoSetup:
    """Create repositories for both tenants."""
    tenant_a = setup_tenants["tenant_a"]
    tenant_b = setup_tenants["tenant_b"]

    repo_a = create_repository(
        org_id=tenant_a["org_id"],
        tenant_id=tenant_a["tenant_id"],
        name="repo_a",
        path="/path/a",
    )
    repo_b = create_repository(
        org_id=tenant_b["org_id"],
        tenant_id=tenant_b["tenant_id"],
        name="repo_b",
        path="/path/b",
    )

    return {
        "tenant_a": tenant_a,
        "tenant_b": tenant_b,
        "repo_a": repo_a,
        "repo_b": repo_b,
    }


class TestRLSPolicies:
    """Test RLS policies for retained graph tables."""

    def test_rls_enabled_on_all_tables(self) -> None:
        results = typed_db_pool.execute(
            """
            SELECT schemaname, tablename, rowsecurity
            FROM pg_tables t
            JOIN pg_class c ON c.relname = t.tablename
            WHERE schemaname = 'aethyme'
              AND tablename IN (
                'tenants', 'repositories', 'nodes', 'edges', 'indexing_jobs', 'api_keys'
              )
            """
        )
        assert results is not None
        tables_with_rls = {row["tablename"]: row["rowsecurity"] for row in results}

        for table in ["tenants", "repositories", "nodes", "edges", "indexing_jobs", "api_keys"]:
            assert table in tables_with_rls
            assert tables_with_rls[table] is True

    def test_repositories_read_isolation(self, setup_repos: RepoSetup) -> None:
        tenant_a = setup_repos["tenant_a"]
        repo_a = setup_repos["repo_a"]
        repo_b = setup_repos["repo_b"]

        results = execute_as(
            tenant_id=tenant_a["tenant_id"],
            org_id=tenant_a["org_id"],
            scopes=["repo:read"],
            query="SELECT id FROM aethyme.repositories",
        )
        repo_ids = [str(row["id"]) for row in results or []]

        assert repo_a in repo_ids
        assert repo_b not in repo_ids

    def test_repositories_write_requires_scope(self, setup_repos: RepoSetup) -> None:
        tenant_a = setup_repos["tenant_a"]
        new_repo_id = str(uuid.uuid4())

        with pytest.raises(psycopg2.Error):
            execute_as(
                tenant_id=tenant_a["tenant_id"],
                org_id=tenant_a["org_id"],
                scopes=["repo:read"],
                query="""
                    INSERT INTO aethyme.repositories (id, org_id, tenant_id, name, path)
                    VALUES (%s, %s, %s, %s, %s)
                    RETURNING id
                """,
                params=(new_repo_id, tenant_a["org_id"], tenant_a["tenant_id"], "new_repo", "/path/new"),
            )

        result = execute_as(
            tenant_id=tenant_a["tenant_id"],
            org_id=tenant_a["org_id"],
            scopes=["repo:write"],
            query="""
                INSERT INTO aethyme.repositories (id, org_id, tenant_id, name, path)
                VALUES (%s, %s, %s, %s, %s)
                RETURNING id
            """,
            params=(new_repo_id, tenant_a["org_id"], tenant_a["tenant_id"], "new_repo", "/path/new"),
        )

        assert result and len(result) > 0

    def test_nodes_tenant_isolation(self, setup_repos: RepoSetup) -> None:
        tenant_a = setup_repos["tenant_a"]
        tenant_b = setup_repos["tenant_b"]
        repo_a = setup_repos["repo_a"]
        repo_b = setup_repos["repo_b"]

        node_a = f"node_a_{uuid.uuid4().hex[:8]}"
        node_b = f"node_b_{uuid.uuid4().hex[:8]}"

        create_node(
            org_id=tenant_a["org_id"],
            tenant_id=tenant_a["tenant_id"],
            repository_id=repo_a,
            node_id=node_a,
            symbol="symbol_a",
            file_path="a.py",
        )
        create_node(
            org_id=tenant_b["org_id"],
            tenant_id=tenant_b["tenant_id"],
            repository_id=repo_b,
            node_id=node_b,
            symbol="symbol_b",
            file_path="b.py",
        )

        results = execute_as(
            tenant_id=tenant_a["tenant_id"],
            org_id=tenant_a["org_id"],
            scopes=["repo:read"],
            query="SELECT id FROM aethyme.nodes",
        )
        node_ids = [row["id"] for row in results or []]

        assert node_a in node_ids
        assert node_b not in node_ids

    def test_edges_tenant_isolation(self, setup_repos: RepoSetup) -> None:
        tenant_a = setup_repos["tenant_a"]
        tenant_b = setup_repos["tenant_b"]
        repo_a = setup_repos["repo_a"]
        repo_b = setup_repos["repo_b"]

        node_a1 = f"node_a1_{uuid.uuid4().hex[:8]}"
        node_a2 = f"node_a2_{uuid.uuid4().hex[:8]}"
        node_b1 = f"node_b1_{uuid.uuid4().hex[:8]}"
        node_b2 = f"node_b2_{uuid.uuid4().hex[:8]}"

        node_specs: list[tuple[str, TenantRecord, str]] = [
            (node_a1, tenant_a, repo_a),
            (node_a2, tenant_a, repo_a),
            (node_b1, tenant_b, repo_b),
            (node_b2, tenant_b, repo_b),
        ]
        for node_id, tenant, repo_id in node_specs:
            create_node(
                org_id=tenant["org_id"],
                tenant_id=tenant["tenant_id"],
                repository_id=repo_id,
                node_id=node_id,
                symbol=f"symbol_{node_id}",
                file_path="test.py",
            )

        edge_a = f"edge_a_{uuid.uuid4().hex[:8]}"
        edge_b = f"edge_b_{uuid.uuid4().hex[:8]}"
        create_edge(
            org_id=tenant_a["org_id"],
            tenant_id=tenant_a["tenant_id"],
            repository_id=repo_a,
            edge_id=edge_a,
            from_node_id=node_a1,
            to_node_id=node_a2,
        )
        create_edge(
            org_id=tenant_b["org_id"],
            tenant_id=tenant_b["tenant_id"],
            repository_id=repo_b,
            edge_id=edge_b,
            from_node_id=node_b1,
            to_node_id=node_b2,
        )

        results = execute_as(
            tenant_id=tenant_a["tenant_id"],
            org_id=tenant_a["org_id"],
            scopes=["repo:read"],
            query="SELECT id FROM aethyme.edges",
        )
        edge_ids = [row["id"] for row in results or []]

        assert edge_a in edge_ids
        assert edge_b not in edge_ids

    def test_repository_write_scope_is_enforced(self, setup_tenants: TenantSetup) -> None:
        tenant_a = setup_tenants["tenant_a"]
        with pytest.raises(psycopg2.Error):
            execute_as(
                tenant_id=tenant_a["tenant_id"],
                org_id=tenant_a["org_id"],
                scopes=["repo:read"],
                query="""
                    INSERT INTO aethyme.repositories (id, org_id, tenant_id, name, path)
                    VALUES (%s, %s, %s, %s, %s)
                """,
                params=(
                    str(uuid.uuid4()),
                    tenant_a["org_id"],
                    tenant_a["tenant_id"],
                    "denied_repo",
                    "/denied",
                ),
                fetch=False,
            )

    def test_cross_tenant_update_blocked(self, setup_repos: RepoSetup) -> None:
        tenant_a = setup_repos["tenant_a"]
        tenant_b = setup_repos["tenant_b"]
        repo_b = setup_repos["repo_b"]

        result = execute_as(
            tenant_id=tenant_a["tenant_id"],
            org_id=tenant_a["org_id"],
            scopes=["org:admin"],
            query="""
                UPDATE aethyme.repositories
                SET name = 'hacked_name'
                WHERE id = %s
                RETURNING id
            """,
            params=(repo_b,),
        )
        assert not result or len(result) == 0

        check_result = execute_as(
            tenant_id=tenant_b["tenant_id"],
            org_id=tenant_b["org_id"],
            scopes=["repo:read"],
            query="SELECT name FROM aethyme.repositories WHERE id = %s",
            params=(repo_b,),
        )
        assert check_result is not None
        assert check_result[0]["name"] == "repo_b"

    def test_delete_requires_write_or_admin(self, setup_repos: RepoSetup) -> None:
        tenant_a = setup_repos["tenant_a"]
        test_repo = create_repository(
            org_id=tenant_a["org_id"],
            tenant_id=tenant_a["tenant_id"],
            name="delete_test",
            path="/path",
        )

        result = execute_as(
            tenant_id=tenant_a["tenant_id"],
            org_id=tenant_a["org_id"],
            scopes=["repo:write"],
            query="DELETE FROM aethyme.repositories WHERE id = %s RETURNING id",
            params=(test_repo,),
        )
        assert result and len(result) > 0

        replacement_repo = create_repository(
            org_id=tenant_a["org_id"],
            tenant_id=tenant_a["tenant_id"],
            name="delete_test_admin",
            path="/path-admin",
        )
        result = execute_as(
            tenant_id=tenant_a["tenant_id"],
            org_id=tenant_a["org_id"],
            scopes=["org:admin"],
            query="DELETE FROM aethyme.repositories WHERE id = %s RETURNING id",
            params=(replacement_repo,),
        )
        assert result and len(result) > 0


class TestRLSPolicyBypass:
    """Test that RLS policies cannot be bypassed."""

    def test_cannot_bypass_with_null_tenant(self, setup_tenants: TenantSetup) -> None:
        tenant_a = setup_tenants["tenant_a"]
        repo_id = create_repository(
            org_id=tenant_a["org_id"],
            tenant_id=tenant_a["tenant_id"],
            name="test_repo",
            path="/path",
        )

        with typed_db_pool.get_connection() as conn:
            with conn.cursor() as cur:
                cur.execute(f"SET ROLE {RUNTIME_ROLE}")
                cur.execute("SELECT id FROM aethyme.repositories WHERE id = %s", (repo_id,))
                result = cur.fetchall()

        assert not result or len(result) == 0

    def test_cannot_bypass_with_sql_injection(self, setup_tenants: TenantSetup) -> None:
        tenant_a = setup_tenants["tenant_a"]
        tenant_b = setup_tenants["tenant_b"]

        create_repository(
            org_id=tenant_a["org_id"],
            tenant_id=tenant_a["tenant_id"],
            name="repo_a",
            path="/path/a",
        )
        repo_b = create_repository(
            org_id=tenant_b["org_id"],
            tenant_id=tenant_b["tenant_id"],
            name="repo_b",
            path="/path/b",
        )

        results = execute_as(
            tenant_id=tenant_a["tenant_id"],
            org_id=tenant_a["org_id"],
            scopes=["repo:read"],
            query=f"""
                SELECT id FROM aethyme.repositories
                WHERE name = 'repo_b' OR tenant_id = '{tenant_b['tenant_id']}'
            """,
        )
        repo_ids = [str(row["id"]) for row in results or []]
        assert repo_b not in repo_ids
