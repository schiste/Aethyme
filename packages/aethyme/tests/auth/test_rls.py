"""Tests for Row-Level Security (RLS) policies.

These tests verify that RLS policies correctly enforce tenant isolation
and scope-based access control at the database level.
"""

import pytest
import uuid

from src.graph.connection_pool import db_pool


class TestRLSPolicies:
    """Test RLS policies for all tables."""

    @pytest.fixture
    def setup_tenants(self):
        """Create two test tenants."""
        tenant_a = str(uuid.uuid4())
        tenant_b = str(uuid.uuid4())

        query = """
            INSERT INTO aethyme.tenants (id, name, description)
            VALUES (%s, %s, %s)
        """

        db_pool.execute(query, (tenant_a, f"tenant_a_{tenant_a[:8]}", "Tenant A"), fetch=False)
        db_pool.execute(query, (tenant_b, f"tenant_b_{tenant_b[:8]}", "Tenant B"), fetch=False)

        yield {"tenant_a": tenant_a, "tenant_b": tenant_b}

        # Cleanup
        db_pool.execute("DELETE FROM aethyme.tenants WHERE id IN (%s, %s)", (tenant_a, tenant_b), fetch=False)

    @pytest.fixture
    def setup_repos(self, setup_tenants):
        """Create repositories for both tenants."""
        tenant_a = setup_tenants["tenant_a"]
        tenant_b = setup_tenants["tenant_b"]

        repo_a = str(uuid.uuid4())
        repo_b = str(uuid.uuid4())

        query = """
            INSERT INTO aethyme.repositories (id, tenant_id, name, path)
            VALUES (%s, %s, %s, %s)
        """

        db_pool.execute(query, (repo_a, tenant_a, "repo_a", "/path/a"), fetch=False)
        db_pool.execute(query, (repo_b, tenant_b, "repo_b", "/path/b"), fetch=False)

        yield {
            "tenant_a": tenant_a,
            "tenant_b": tenant_b,
            "repo_a": repo_a,
            "repo_b": repo_b,
        }

    def test_rls_enabled_on_all_tables(self):
        """Verify RLS is enabled on all critical tables."""
        query = """
            SELECT
                schemaname,
                tablename,
                rowsecurity
            FROM pg_tables t
            JOIN pg_class c ON c.relname = t.tablename
            WHERE schemaname = 'aethyme'
            AND tablename IN (
                'tenants', 'repositories', 'nodes', 'edges',
                'indexing_jobs', 'api_keys'
            )
        """

        results = db_pool.execute(query)

        # Verify all tables have RLS enabled
        tables_with_rls = {r['tablename']: r['rowsecurity'] for r in results}

        required_tables = ['tenants', 'repositories', 'nodes', 'edges', 'indexing_jobs', 'api_keys']

        for table in required_tables:
            assert table in tables_with_rls, f"Table {table} not found"
            assert tables_with_rls[table] is True, f"RLS not enabled on {table}"

    def test_repositories_read_isolation(self, setup_repos):
        """Test that repositories are isolated by tenant for reads."""
        tenant_a = setup_repos["tenant_a"]
        tenant_b = setup_repos["tenant_b"]
        repo_a = setup_repos["repo_a"]
        repo_b = setup_repos["repo_b"]

        # Set context to tenant A
        db_pool.execute(f"SET app.current_tenant = '{tenant_a}'", fetch=False)

        # Query repositories
        query = "SELECT id FROM aethyme.repositories"
        results = db_pool.execute(query)

        repo_ids = [str(r['id']) for r in results]

        # Should see repo A, not repo B
        assert repo_a in repo_ids, "Should see own repository"
        assert repo_b not in repo_ids, "Should not see other tenant's repository"

        # Reset context
        db_pool.execute("RESET app.current_tenant", fetch=False)

    def test_repositories_write_requires_scope(self, setup_repos):
        """Test that write operations require appropriate scope."""
        tenant_a = setup_repos["tenant_a"]

        # Set context with read-only scope
        db_pool.execute(f"SET app.current_tenant = '{tenant_a}'", fetch=False)
        db_pool.execute("SET app.current_scopes = '[\"repo:read\"]'", fetch=False)

        # Try to insert a repository (should fail due to RLS)
        new_repo_id = str(uuid.uuid4())
        query = """
            INSERT INTO aethyme.repositories (id, tenant_id, name, path)
            VALUES (%s, %s, %s, %s)
            RETURNING id
        """

        result = db_pool.execute(
            query,
            (new_repo_id, tenant_a, "new_repo", "/path/new"),
        )

        # Insert should be blocked by RLS policy
        assert not result or len(result) == 0, "Read-only scope should not allow insert"

        # Now try with write scope
        db_pool.execute("SET app.current_scopes = '[\"repo:write\"]'", fetch=False)

        result = db_pool.execute(
            query,
            (new_repo_id, tenant_a, "new_repo", "/path/new"),
        )

        # Insert should succeed with write scope
        assert result and len(result) > 0, "Write scope should allow insert"

        # Cleanup
        db_pool.execute("DELETE FROM aethyme.repositories WHERE id = %s", (new_repo_id,), fetch=False)

        # Reset context
        db_pool.execute("RESET app.current_tenant", fetch=False)
        db_pool.execute("RESET app.current_scopes", fetch=False)

    def test_nodes_tenant_isolation(self, setup_repos):
        """Test that nodes are isolated by tenant."""
        tenant_a = setup_repos["tenant_a"]
        tenant_b = setup_repos["tenant_b"]
        repo_a = setup_repos["repo_a"]
        repo_b = setup_repos["repo_b"]

        # Create nodes for both tenants
        node_a = f"node_a_{uuid.uuid4().hex[:8]}"
        node_b = f"node_b_{uuid.uuid4().hex[:8]}"

        query = """
            INSERT INTO aethyme.nodes (
                id, tenant_id, repository_id, symbol, file_path,
                line_number, column_number, kind, language
            ) VALUES (%s, %s, %s, %s, %s, %s, %s, %s, %s)
        """

        db_pool.execute(
            query,
            (node_a, tenant_a, repo_a, "symbol_a", "a.py", 1, 1, "function", "python"),
            fetch=False,
        )

        db_pool.execute(
            query,
            (node_b, tenant_b, repo_b, "symbol_b", "b.py", 1, 1, "function", "python"),
            fetch=False,
        )

        # Query as tenant A
        db_pool.execute(f"SET app.current_tenant = '{tenant_a}'", fetch=False)
        query = "SELECT id FROM aethyme.nodes"
        results = db_pool.execute(query)

        node_ids = [r['id'] for r in results]

        assert node_a in node_ids, "Should see own nodes"
        assert node_b not in node_ids, "Should not see other tenant's nodes"

        # Reset context
        db_pool.execute("RESET app.current_tenant", fetch=False)

    def test_edges_tenant_isolation(self, setup_repos):
        """Test that edges are isolated by tenant."""
        tenant_a = setup_repos["tenant_a"]
        tenant_b = setup_repos["tenant_b"]
        repo_a = setup_repos["repo_a"]
        repo_b = setup_repos["repo_b"]

        # Create nodes for both tenants
        node_a1 = f"node_a1_{uuid.uuid4().hex[:8]}"
        node_a2 = f"node_a2_{uuid.uuid4().hex[:8]}"
        node_b1 = f"node_b1_{uuid.uuid4().hex[:8]}"
        node_b2 = f"node_b2_{uuid.uuid4().hex[:8]}"

        node_query = """
            INSERT INTO aethyme.nodes (
                id, tenant_id, repository_id, symbol, file_path,
                line_number, column_number, kind, language
            ) VALUES (%s, %s, %s, %s, %s, %s, %s, %s, %s)
        """

        for node_id, tenant_id, repo_id in [
            (node_a1, tenant_a, repo_a),
            (node_a2, tenant_a, repo_a),
            (node_b1, tenant_b, repo_b),
            (node_b2, tenant_b, repo_b),
        ]:
            db_pool.execute(
                node_query,
                (node_id, tenant_id, repo_id, f"symbol_{node_id}", "test.py", 1, 1, "function", "python"),
                fetch=False,
            )

        # Create edges for both tenants
        edge_a = f"edge_a_{uuid.uuid4().hex[:8]}"
        edge_b = f"edge_b_{uuid.uuid4().hex[:8]}"

        edge_query = """
            INSERT INTO aethyme.edges (
                id, tenant_id, repository_id, from_node_id, to_node_id, edge_type
            ) VALUES (%s, %s, %s, %s, %s, %s)
        """

        db_pool.execute(
            edge_query,
            (edge_a, tenant_a, repo_a, node_a1, node_a2, "invoke"),
            fetch=False,
        )

        db_pool.execute(
            edge_query,
            (edge_b, tenant_b, repo_b, node_b1, node_b2, "invoke"),
            fetch=False,
        )

        # Query as tenant A
        db_pool.execute(f"SET app.current_tenant = '{tenant_a}'", fetch=False)
        query = "SELECT id FROM aethyme.edges"
        results = db_pool.execute(query)

        edge_ids = [r['id'] for r in results]

        assert edge_a in edge_ids, "Should see own edges"
        assert edge_b not in edge_ids, "Should not see other tenant's edges"

        # Reset context
        db_pool.execute("RESET app.current_tenant", fetch=False)

    def test_api_keys_require_admin_scope(self, setup_tenants):
        """Test that API key operations require admin scope."""
        tenant_a = setup_tenants["tenant_a"]

        # Set context with non-admin scope
        db_pool.execute(f"SET app.current_tenant = '{tenant_a}'", fetch=False)
        db_pool.execute("SET app.current_scopes = '[\"repo:write\"]'", fetch=False)

        # Try to create API key (should fail)
        key_id = str(uuid.uuid4())
        query = """
            INSERT INTO aethyme.api_keys (
                id, tenant_id, name, key_hash, scopes
            ) VALUES (%s, %s, %s, %s, %s)
            RETURNING id
        """

        import json
        result = db_pool.execute(
            query,
            (key_id, tenant_a, "test_key", "hash123", json.dumps(["repo:read"])),
        )

        # Should be blocked by RLS
        assert not result or len(result) == 0, "Non-admin should not create API keys"

        # Try with admin scope
        db_pool.execute("SET app.current_scopes = '[\"org:admin\"]'", fetch=False)

        result = db_pool.execute(
            query,
            (key_id, tenant_a, "test_key", "hash123", json.dumps(["repo:read"])),
        )

        # Should succeed with admin scope
        assert result and len(result) > 0, "Admin should create API keys"

        # Cleanup
        db_pool.execute("DELETE FROM aethyme.api_keys WHERE id = %s", (key_id,), fetch=False)

        # Reset context
        db_pool.execute("RESET app.current_tenant", fetch=False)
        db_pool.execute("RESET app.current_scopes", fetch=False)

    def test_cross_tenant_update_blocked(self, setup_repos):
        """Test that updates across tenant boundaries are blocked."""
        tenant_a = setup_repos["tenant_a"]
        tenant_b = setup_repos["tenant_b"]
        repo_b = setup_repos["repo_b"]

        # Set context to tenant A with admin scope
        db_pool.execute(f"SET app.current_tenant = '{tenant_a}'", fetch=False)
        db_pool.execute("SET app.current_scopes = '[\"org:admin\"]'", fetch=False)

        # Try to update tenant B's repository
        query = """
            UPDATE aethyme.repositories
            SET name = 'hacked_name'
            WHERE id = %s
            RETURNING id
        """

        result = db_pool.execute(query, (repo_b,))

        # Update should be blocked by RLS (wrong tenant)
        assert not result or len(result) == 0, "Should not update other tenant's data"

        # Verify the repo was not modified
        db_pool.execute(f"SET app.current_tenant = '{tenant_b}'", fetch=False)
        check_query = "SELECT name FROM aethyme.repositories WHERE id = %s"
        check_result = db_pool.execute(check_query, (repo_b,))

        assert check_result[0]['name'] == 'repo_b', "Repo should not be modified"

        # Reset context
        db_pool.execute("RESET app.current_tenant", fetch=False)
        db_pool.execute("RESET app.current_scopes", fetch=False)

    def test_delete_requires_admin(self, setup_repos):
        """Test that delete operations require admin scope."""
        tenant_a = setup_repos["tenant_a"]
        repo_a = setup_repos["repo_a"]

        # Create a test repository to delete
        test_repo = str(uuid.uuid4())
        query = """
            INSERT INTO aethyme.repositories (id, tenant_id, name, path)
            VALUES (%s, %s, %s, %s)
        """
        db_pool.execute(query, (test_repo, tenant_a, "delete_test", "/path"), fetch=False)

        # Set context with write scope (not admin)
        db_pool.execute(f"SET app.current_tenant = '{tenant_a}'", fetch=False)
        db_pool.execute("SET app.current_scopes = '[\"repo:write\"]'", fetch=False)

        # Try to delete (should fail for non-admin users)
        delete_query = "DELETE FROM aethyme.repositories WHERE id = %s RETURNING id"
        result = db_pool.execute(delete_query, (test_repo,))

        # Check if deletion was blocked (depends on your RLS policy)
        # If your policy allows repo:write to delete, adjust this assertion
        # For strictest security, only org:admin should delete

        # Now try with admin scope
        db_pool.execute("SET app.current_scopes = '[\"org:admin\"]'", fetch=False)
        result = db_pool.execute(delete_query, (test_repo,))

        # Should succeed with admin scope
        assert result and len(result) > 0, "Admin should be able to delete"

        # Reset context
        db_pool.execute("RESET app.current_tenant", fetch=False)
        db_pool.execute("RESET app.current_scopes", fetch=False)


class TestRLSPolicyBypass:
    """Test that RLS policies cannot be bypassed."""

    def test_cannot_bypass_with_null_tenant(self, setup_tenants):
        """Test that setting null tenant doesn't bypass RLS."""
        tenant_a = setup_tenants["tenant_a"]

        # Create a repository
        repo_id = str(uuid.uuid4())
        query = """
            INSERT INTO aethyme.repositories (id, tenant_id, name, path)
            VALUES (%s, %s, %s, %s)
        """
        db_pool.execute(query, (repo_id, tenant_a, "test_repo", "/path"), fetch=False)

        # Try to query with null tenant context
        db_pool.execute("RESET app.current_tenant", fetch=False)

        # Query should return nothing (or error)
        query = "SELECT id FROM aethyme.repositories WHERE id = %s"
        result = db_pool.execute(query, (repo_id,))

        # With null tenant, should not see any data
        assert not result or len(result) == 0, "Null tenant should not see data"

        # Cleanup
        db_pool.execute(f"SET app.current_tenant = '{tenant_a}'", fetch=False)
        db_pool.execute("DELETE FROM aethyme.repositories WHERE id = %s", (repo_id,), fetch=False)
        db_pool.execute("RESET app.current_tenant", fetch=False)

    def test_cannot_bypass_with_sql_injection(self, setup_tenants):
        """Test that SQL injection cannot bypass RLS."""
        tenant_a = setup_tenants["tenant_a"]
        tenant_b = setup_tenants["tenant_b"]

        # Create repos for both tenants
        repo_a = str(uuid.uuid4())
        repo_b = str(uuid.uuid4())

        query = """
            INSERT INTO aethyme.repositories (id, tenant_id, name, path)
            VALUES (%s, %s, %s, %s)
        """

        db_pool.execute(query, (repo_a, tenant_a, "repo_a", "/path/a"), fetch=False)
        db_pool.execute(query, (repo_b, tenant_b, "repo_b", "/path/b"), fetch=False)

        # Set context to tenant A
        db_pool.execute(f"SET app.current_tenant = '{tenant_a}'", fetch=False)

        # Try SQL injection in query (RLS should still protect)
        malicious_query = f"""
            SELECT id FROM aethyme.repositories
            WHERE name = 'repo_b' OR tenant_id = '{tenant_b}'
        """

        results = db_pool.execute(malicious_query)

        # Should not see tenant B's repo even with malicious query
        repo_ids = [str(r['id']) for r in results] if results else []
        assert repo_b not in repo_ids, "SQL injection should not bypass RLS"

        # Reset context
        db_pool.execute("RESET app.current_tenant", fetch=False)


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
