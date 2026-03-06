-- Migration 002: Tenant-scoped RLS hardening

SET search_path TO aethyme, public;

CREATE OR REPLACE FUNCTION current_tenant_id()
RETURNS UUID AS $$
BEGIN
    RETURN current_setting('app.current_tenant', true)::uuid;
EXCEPTION
    WHEN OTHERS THEN
        RETURN NULL;
END;
$$ LANGUAGE plpgsql STABLE;

CREATE OR REPLACE FUNCTION current_org_id()
RETURNS UUID AS $$
BEGIN
    RETURN current_setting('app.current_org', true)::uuid;
EXCEPTION
    WHEN OTHERS THEN
        RETURN NULL;
END;
$$ LANGUAGE plpgsql STABLE;

CREATE OR REPLACE FUNCTION has_scope(required_scope TEXT)
RETURNS BOOLEAN AS $$
BEGIN
    RETURN current_setting('app.current_scopes', true)::jsonb ? required_scope;
EXCEPTION
    WHEN OTHERS THEN
        RETURN false;
END;
$$ LANGUAGE plpgsql STABLE;

DROP POLICY IF EXISTS org_isolation_self ON orgs;
CREATE POLICY org_isolation_self ON orgs
    FOR SELECT
    USING (id = current_org_id() OR current_org_id() IS NULL);

DROP POLICY IF EXISTS tenant_isolation_self ON tenants;
CREATE POLICY tenant_isolation_self ON tenants
    FOR SELECT
    USING (id = current_tenant_id());

DROP POLICY IF EXISTS users_tenant_read ON users;
CREATE POLICY users_tenant_read ON users
    FOR SELECT
    USING (tenant_id = current_tenant_id());

DROP POLICY IF EXISTS repositories_tenant_read ON repositories;
CREATE POLICY repositories_tenant_read ON repositories
    FOR SELECT
    USING (tenant_id = current_tenant_id());

DROP POLICY IF EXISTS repositories_tenant_write ON repositories;
CREATE POLICY repositories_tenant_write ON repositories
    FOR ALL
    USING (
        tenant_id = current_tenant_id()
        AND (has_scope('repo:write') OR has_scope('org:admin'))
    )
    WITH CHECK (
        tenant_id = current_tenant_id()
        AND (has_scope('repo:write') OR has_scope('org:admin'))
    );

DROP POLICY IF EXISTS nodes_tenant_read ON nodes;
CREATE POLICY nodes_tenant_read ON nodes
    FOR SELECT
    USING (tenant_id = current_tenant_id());

DROP POLICY IF EXISTS nodes_tenant_write ON nodes;
CREATE POLICY nodes_tenant_write ON nodes
    FOR ALL
    USING (
        tenant_id = current_tenant_id()
        AND (has_scope('repo:write') OR has_scope('org:admin'))
    )
    WITH CHECK (
        tenant_id = current_tenant_id()
        AND (has_scope('repo:write') OR has_scope('org:admin'))
    );

DROP POLICY IF EXISTS edges_tenant_read ON edges;
CREATE POLICY edges_tenant_read ON edges
    FOR SELECT
    USING (tenant_id = current_tenant_id());

DROP POLICY IF EXISTS edges_tenant_write ON edges;
CREATE POLICY edges_tenant_write ON edges
    FOR ALL
    USING (
        tenant_id = current_tenant_id()
        AND (has_scope('repo:write') OR has_scope('org:admin'))
    )
    WITH CHECK (
        tenant_id = current_tenant_id()
        AND (has_scope('repo:write') OR has_scope('org:admin'))
    );

DROP POLICY IF EXISTS indexing_jobs_tenant_read ON indexing_jobs;
CREATE POLICY indexing_jobs_tenant_read ON indexing_jobs
    FOR SELECT
    USING (tenant_id = current_tenant_id());

DROP POLICY IF EXISTS indexing_jobs_tenant_write ON indexing_jobs;
CREATE POLICY indexing_jobs_tenant_write ON indexing_jobs
    FOR ALL
    USING (
        tenant_id = current_tenant_id()
        AND (has_scope('repo:write') OR has_scope('org:admin'))
    )
    WITH CHECK (
        tenant_id = current_tenant_id()
        AND (has_scope('repo:write') OR has_scope('org:admin'))
    );

ALTER TABLE repositories FORCE ROW LEVEL SECURITY;
ALTER TABLE nodes FORCE ROW LEVEL SECURITY;
ALTER TABLE edges FORCE ROW LEVEL SECURITY;
ALTER TABLE indexing_jobs FORCE ROW LEVEL SECURITY;

DROP POLICY IF EXISTS api_keys_tenant_read ON api_keys;
CREATE POLICY api_keys_tenant_read ON api_keys
    FOR SELECT
    USING (tenant_id = current_tenant_id());

DROP POLICY IF EXISTS api_keys_tenant_write ON api_keys;
CREATE POLICY api_keys_tenant_write ON api_keys
    FOR ALL
    USING (
        tenant_id = current_tenant_id()
        AND has_scope('org:admin')
    )
    WITH CHECK (
        tenant_id = current_tenant_id()
        AND has_scope('org:admin')
    );
