-- Row-Level Security (RLS) Policies for RepoGraph
-- Multi-tenant isolation and scope-based access control
--
-- This file contains the complete RLS policy definitions for all tables.
-- Policies enforce tenant isolation based on the current session's tenant context.

SET search_path TO repograph, public;

-- =============================================================================
-- HELPER FUNCTIONS
-- =============================================================================

-- Get current tenant from session variable
CREATE OR REPLACE FUNCTION current_tenant_id()
RETURNS UUID AS $$
BEGIN
    RETURN current_setting('app.current_tenant', true)::uuid;
EXCEPTION
    WHEN OTHERS THEN
        RETURN NULL;
END;
$$ LANGUAGE plpgsql STABLE;

COMMENT ON FUNCTION current_tenant_id IS 'Get current tenant ID from session variable';

-- Check if current user has a specific scope
CREATE OR REPLACE FUNCTION has_scope(required_scope TEXT)
RETURNS BOOLEAN AS $$
BEGIN
    -- This would be set by the application based on JWT token scopes
    -- For now, we'll check a session variable
    RETURN current_setting('app.current_scopes', true)::jsonb ? required_scope;
EXCEPTION
    WHEN OTHERS THEN
        RETURN false;
END;
$$ LANGUAGE plpgsql STABLE;

COMMENT ON FUNCTION has_scope IS 'Check if current session has a specific scope';

-- =============================================================================
-- TENANTS TABLE
-- =============================================================================

-- Tenants can only see their own tenant record
DROP POLICY IF EXISTS tenant_isolation_self ON tenants;
CREATE POLICY tenant_isolation_self ON tenants
    FOR ALL
    USING (id = current_tenant_id());

COMMENT ON POLICY tenant_isolation_self ON tenants IS
    'Tenant can only access their own tenant record';

-- =============================================================================
-- REPOSITORIES TABLE
-- =============================================================================

-- Read policy: Users can read repos in their tenant
DROP POLICY IF EXISTS repositories_tenant_read ON repositories;
CREATE POLICY repositories_tenant_read ON repositories
    FOR SELECT
    USING (tenant_id = current_tenant_id());

-- Insert policy: Users with write scope can create repos in their tenant
DROP POLICY IF EXISTS repositories_tenant_insert ON repositories;
CREATE POLICY repositories_tenant_insert ON repositories
    FOR INSERT
    WITH CHECK (
        tenant_id = current_tenant_id()
        AND (
            has_scope('repo:write')
            OR has_scope('org:admin')
        )
    );

-- Update policy: Users with write scope can update repos in their tenant
DROP POLICY IF EXISTS repositories_tenant_update ON repositories;
CREATE POLICY repositories_tenant_update ON repositories
    FOR UPDATE
    USING (tenant_id = current_tenant_id())
    WITH CHECK (
        tenant_id = current_tenant_id()
        AND (
            has_scope('repo:write')
            OR has_scope('org:admin')
        )
    );

-- Delete policy: Only org admins can delete repos
DROP POLICY IF EXISTS repositories_tenant_delete ON repositories;
CREATE POLICY repositories_tenant_delete ON repositories
    FOR DELETE
    USING (
        tenant_id = current_tenant_id()
        AND has_scope('org:admin')
    );

COMMENT ON POLICY repositories_tenant_read ON repositories IS
    'Users can read repositories in their tenant';
COMMENT ON POLICY repositories_tenant_insert ON repositories IS
    'Users with repo:write scope can create repositories';
COMMENT ON POLICY repositories_tenant_update ON repositories IS
    'Users with repo:write scope can update repositories';
COMMENT ON POLICY repositories_tenant_delete ON repositories IS
    'Only org admins can delete repositories';

-- =============================================================================
-- NODES TABLE
-- =============================================================================

-- Read policy: Users can read nodes in their tenant
DROP POLICY IF EXISTS nodes_tenant_read ON nodes;
CREATE POLICY nodes_tenant_read ON nodes
    FOR SELECT
    USING (tenant_id = current_tenant_id());

-- Insert policy: Users with write scope can create nodes
DROP POLICY IF EXISTS nodes_tenant_insert ON nodes;
CREATE POLICY nodes_tenant_insert ON nodes
    FOR INSERT
    WITH CHECK (
        tenant_id = current_tenant_id()
        AND (
            has_scope('repo:write')
            OR has_scope('org:admin')
        )
    );

-- Update policy: Users with write scope can update nodes
DROP POLICY IF EXISTS nodes_tenant_update ON nodes;
CREATE POLICY nodes_tenant_update ON nodes
    FOR UPDATE
    USING (tenant_id = current_tenant_id())
    WITH CHECK (
        tenant_id = current_tenant_id()
        AND (
            has_scope('repo:write')
            OR has_scope('org:admin')
        )
    );

-- Delete policy: Users with write scope can delete nodes
DROP POLICY IF EXISTS nodes_tenant_delete ON nodes;
CREATE POLICY nodes_tenant_delete ON nodes
    FOR DELETE
    USING (
        tenant_id = current_tenant_id()
        AND (
            has_scope('repo:write')
            OR has_scope('org:admin')
        )
    );

COMMENT ON POLICY nodes_tenant_read ON nodes IS
    'Users can read nodes in their tenant';
COMMENT ON POLICY nodes_tenant_insert ON nodes IS
    'Users with repo:write scope can create nodes';
COMMENT ON POLICY nodes_tenant_update ON nodes IS
    'Users with repo:write scope can update nodes';
COMMENT ON POLICY nodes_tenant_delete ON nodes IS
    'Users with repo:write scope can delete nodes';

-- =============================================================================
-- EDGES TABLE
-- =============================================================================

-- Read policy: Users can read edges in their tenant
DROP POLICY IF EXISTS edges_tenant_read ON edges;
CREATE POLICY edges_tenant_read ON edges
    FOR SELECT
    USING (tenant_id = current_tenant_id());

-- Insert policy: Users with write scope can create edges
DROP POLICY IF EXISTS edges_tenant_insert ON edges;
CREATE POLICY edges_tenant_insert ON edges
    FOR INSERT
    WITH CHECK (
        tenant_id = current_tenant_id()
        AND (
            has_scope('repo:write')
            OR has_scope('org:admin')
        )
    );

-- Update policy: Users with write scope can update edges
DROP POLICY IF EXISTS edges_tenant_update ON edges;
CREATE POLICY edges_tenant_update ON edges
    FOR UPDATE
    USING (tenant_id = current_tenant_id())
    WITH CHECK (
        tenant_id = current_tenant_id()
        AND (
            has_scope('repo:write')
            OR has_scope('org:admin')
        )
    );

-- Delete policy: Users with write scope can delete edges
DROP POLICY IF EXISTS edges_tenant_delete ON edges;
CREATE POLICY edges_tenant_delete ON edges
    FOR DELETE
    USING (
        tenant_id = current_tenant_id()
        AND (
            has_scope('repo:write')
            OR has_scope('org:admin')
        )
    );

COMMENT ON POLICY edges_tenant_read ON edges IS
    'Users can read edges in their tenant';
COMMENT ON POLICY edges_tenant_insert ON edges IS
    'Users with repo:write scope can create edges';
COMMENT ON POLICY edges_tenant_update ON edges IS
    'Users with repo:write scope can update edges';
COMMENT ON POLICY edges_tenant_delete ON edges IS
    'Users with repo:write scope can delete edges';

-- =============================================================================
-- INDEXING_JOBS TABLE
-- =============================================================================

-- Read policy: Users can read indexing jobs in their tenant
DROP POLICY IF EXISTS indexing_jobs_tenant_read ON indexing_jobs;
CREATE POLICY indexing_jobs_tenant_read ON indexing_jobs
    FOR SELECT
    USING (tenant_id = current_tenant_id());

-- Insert policy: Users with write scope can create indexing jobs
DROP POLICY IF EXISTS indexing_jobs_tenant_insert ON indexing_jobs;
CREATE POLICY indexing_jobs_tenant_insert ON indexing_jobs
    FOR INSERT
    WITH CHECK (
        tenant_id = current_tenant_id()
        AND (
            has_scope('repo:write')
            OR has_scope('org:admin')
        )
    );

-- Update policy: Users with write scope can update indexing jobs
DROP POLICY IF EXISTS indexing_jobs_tenant_update ON indexing_jobs;
CREATE POLICY indexing_jobs_tenant_update ON indexing_jobs
    FOR UPDATE
    USING (tenant_id = current_tenant_id())
    WITH CHECK (
        tenant_id = current_tenant_id()
        AND (
            has_scope('repo:write')
            OR has_scope('org:admin')
        )
    );

-- Delete policy: Only org admins can delete indexing jobs
DROP POLICY IF EXISTS indexing_jobs_tenant_delete ON indexing_jobs;
CREATE POLICY indexing_jobs_tenant_delete ON indexing_jobs
    FOR DELETE
    USING (
        tenant_id = current_tenant_id()
        AND has_scope('org:admin')
    );

COMMENT ON POLICY indexing_jobs_tenant_read ON indexing_jobs IS
    'Users can read indexing jobs in their tenant';
COMMENT ON POLICY indexing_jobs_tenant_insert ON indexing_jobs IS
    'Users with repo:write scope can create indexing jobs';
COMMENT ON POLICY indexing_jobs_tenant_update ON indexing_jobs IS
    'Users with repo:write scope can update indexing jobs';
COMMENT ON POLICY indexing_jobs_tenant_delete ON indexing_jobs IS
    'Only org admins can delete indexing jobs';

-- =============================================================================
-- API_KEYS TABLE
-- =============================================================================

-- Read policy: Users can read API keys in their tenant
DROP POLICY IF EXISTS api_keys_tenant_read ON api_keys;
CREATE POLICY api_keys_tenant_read ON api_keys
    FOR SELECT
    USING (tenant_id = current_tenant_id());

-- Insert policy: Only org admins can create API keys
DROP POLICY IF EXISTS api_keys_tenant_insert ON api_keys;
CREATE POLICY api_keys_tenant_insert ON api_keys
    FOR INSERT
    WITH CHECK (
        tenant_id = current_tenant_id()
        AND has_scope('org:admin')
    );

-- Update policy: Only org admins can update API keys
DROP POLICY IF EXISTS api_keys_tenant_update ON api_keys;
CREATE POLICY api_keys_tenant_update ON api_keys
    FOR UPDATE
    USING (tenant_id = current_tenant_id())
    WITH CHECK (
        tenant_id = current_tenant_id()
        AND has_scope('org:admin')
    );

-- Delete policy: Only org admins can delete API keys
DROP POLICY IF EXISTS api_keys_tenant_delete ON api_keys;
CREATE POLICY api_keys_tenant_delete ON api_keys
    FOR DELETE
    USING (
        tenant_id = current_tenant_id()
        AND has_scope('org:admin')
    );

COMMENT ON POLICY api_keys_tenant_read ON api_keys IS
    'Users can read API keys in their tenant';
COMMENT ON POLICY api_keys_tenant_insert ON api_keys IS
    'Only org admins can create API keys';
COMMENT ON POLICY api_keys_tenant_update ON api_keys IS
    'Only org admins can update API keys';
COMMENT ON POLICY api_keys_tenant_delete ON api_keys IS
    'Only org admins can delete API keys';

-- =============================================================================
-- ENABLE RLS ON ALL TABLES
-- =============================================================================

-- Enable RLS (if not already enabled)
ALTER TABLE tenants ENABLE ROW LEVEL SECURITY;
ALTER TABLE repositories ENABLE ROW LEVEL SECURITY;
ALTER TABLE nodes ENABLE ROW LEVEL SECURITY;
ALTER TABLE edges ENABLE ROW LEVEL SECURITY;
ALTER TABLE indexing_jobs ENABLE ROW LEVEL SECURITY;
ALTER TABLE api_keys ENABLE ROW LEVEL SECURITY;

-- Force RLS for table owners (important for security)
ALTER TABLE tenants FORCE ROW LEVEL SECURITY;
ALTER TABLE repositories FORCE ROW LEVEL SECURITY;
ALTER TABLE nodes FORCE ROW LEVEL SECURITY;
ALTER TABLE edges FORCE ROW LEVEL SECURITY;
ALTER TABLE indexing_jobs FORCE ROW LEVEL SECURITY;
ALTER TABLE api_keys FORCE ROW LEVEL SECURITY;

-- =============================================================================
-- VERIFICATION QUERIES
-- =============================================================================

-- To test RLS policies, use these queries:
--
-- 1. Set tenant context:
--    SET app.current_tenant = 'your-tenant-uuid';
--    SET app.current_scopes = '["repo:read", "repo:write"]';
--
-- 2. Try querying:
--    SELECT * FROM repograph.repositories;
--    -- Should only return repos for the current tenant
--
-- 3. Try inserting without write scope:
--    SET app.current_scopes = '["repo:read"]';
--    INSERT INTO repograph.repositories (...);
--    -- Should fail with permission denied
--
-- 4. Reset context:
--    RESET app.current_tenant;
--    RESET app.current_scopes;
