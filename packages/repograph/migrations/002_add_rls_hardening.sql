-- Migration 002: RLS Hardening and Enhanced Security
-- This migration adds enhanced RLS policies, scope checking, and additional security features

SET search_path TO repograph, public;

-- =============================================================================
-- ADD MISSING COLUMNS
-- =============================================================================

-- Add org_id column to tables that don't have it yet (if any)
-- Note: Most tables already have tenant_id from migration 001

-- Add scopes column to api_keys if not exists
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'repograph'
        AND table_name = 'api_keys'
        AND column_name = 'scopes'
    ) THEN
        ALTER TABLE api_keys ADD COLUMN scopes JSONB DEFAULT '["repo:read"]';
    END IF;
END $$;

-- Add repo_id column to api_keys for repository-scoped keys
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'repograph'
        AND table_name = 'api_keys'
        AND column_name = 'repo_id'
    ) THEN
        ALTER TABLE api_keys ADD COLUMN repo_id UUID REFERENCES repositories(id) ON DELETE CASCADE;
    END IF;
END $$;

-- Create users table for proper user management
CREATE TABLE IF NOT EXISTS users (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    email VARCHAR(255) UNIQUE NOT NULL,
    password_hash VARCHAR(255),
    scopes JSONB DEFAULT '["repo:read"]',
    is_active BOOLEAN DEFAULT true,
    last_login_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ DEFAULT CURRENT_TIMESTAMP
);

-- Create index on email for faster lookups
CREATE INDEX IF NOT EXISTS idx_users_email ON users(email);
CREATE INDEX IF NOT EXISTS idx_users_tenant ON users(tenant_id);

-- Create refresh tokens table
CREATE TABLE IF NOT EXISTS refresh_tokens (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash VARCHAR(255) UNIQUE NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ DEFAULT CURRENT_TIMESTAMP,
    last_used_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_refresh_tokens_user ON refresh_tokens(user_id);
CREATE INDEX IF NOT EXISTS idx_refresh_tokens_expires ON refresh_tokens(expires_at);

-- Create audit log table for security events
CREATE TABLE IF NOT EXISTS audit_logs (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    action VARCHAR(100) NOT NULL,
    resource_type VARCHAR(100),
    resource_id UUID,
    details JSONB,
    ip_address INET,
    user_agent TEXT,
    created_at TIMESTAMPTZ DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_audit_logs_tenant ON audit_logs(tenant_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_audit_logs_user ON audit_logs(user_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_audit_logs_action ON audit_logs(action);

-- =============================================================================
-- CREATE HELPER FUNCTIONS
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

-- Check if current user has a specific scope
CREATE OR REPLACE FUNCTION has_scope(required_scope TEXT)
RETURNS BOOLEAN AS $$
BEGIN
    RETURN current_setting('app.current_scopes', true)::jsonb ? required_scope;
EXCEPTION
    WHEN OTHERS THEN
        RETURN false;
END;
$$ LANGUAGE plpgsql STABLE;

-- Log audit event
CREATE OR REPLACE FUNCTION log_audit_event(
    p_tenant_id UUID,
    p_user_id UUID,
    p_action VARCHAR(100),
    p_resource_type VARCHAR(100) DEFAULT NULL,
    p_resource_id UUID DEFAULT NULL,
    p_details JSONB DEFAULT NULL
)
RETURNS UUID AS $$
DECLARE
    v_log_id UUID;
BEGIN
    INSERT INTO audit_logs (
        tenant_id, user_id, action, resource_type,
        resource_id, details
    ) VALUES (
        p_tenant_id, p_user_id, p_action, p_resource_type,
        p_resource_id, p_details
    )
    RETURNING id INTO v_log_id;

    RETURN v_log_id;
END;
$$ LANGUAGE plpgsql;

-- =============================================================================
-- UPDATE EXISTING TABLES
-- =============================================================================

-- Add updated_at trigger to new tables
CREATE TRIGGER update_users_updated_at BEFORE UPDATE ON users
    FOR EACH ROW EXECUTE FUNCTION update_updated_at();

-- =============================================================================
-- APPLY RLS POLICIES FROM rls_policies.sql
-- =============================================================================

-- Source the RLS policies file
-- Note: This assumes the rls_policies.sql file is in the same directory
-- In production, you would run: \i src/database/rls_policies.sql

-- For this migration, we'll inline the critical policies:

-- REPOSITORIES
DROP POLICY IF EXISTS repositories_tenant_read ON repositories;
CREATE POLICY repositories_tenant_read ON repositories
    FOR SELECT
    USING (tenant_id = current_tenant_id());

DROP POLICY IF EXISTS repositories_tenant_insert ON repositories;
CREATE POLICY repositories_tenant_insert ON repositories
    FOR INSERT
    WITH CHECK (
        tenant_id = current_tenant_id()
        AND (has_scope('repo:write') OR has_scope('org:admin'))
    );

DROP POLICY IF EXISTS repositories_tenant_update ON repositories;
CREATE POLICY repositories_tenant_update ON repositories
    FOR UPDATE
    USING (tenant_id = current_tenant_id())
    WITH CHECK (
        tenant_id = current_tenant_id()
        AND (has_scope('repo:write') OR has_scope('org:admin'))
    );

DROP POLICY IF EXISTS repositories_tenant_delete ON repositories;
CREATE POLICY repositories_tenant_delete ON repositories
    FOR DELETE
    USING (
        tenant_id = current_tenant_id()
        AND has_scope('org:admin')
    );

-- NODES
DROP POLICY IF EXISTS nodes_tenant_read ON nodes;
CREATE POLICY nodes_tenant_read ON nodes
    FOR SELECT
    USING (tenant_id = current_tenant_id());

DROP POLICY IF EXISTS nodes_tenant_insert ON nodes;
CREATE POLICY nodes_tenant_insert ON nodes
    FOR INSERT
    WITH CHECK (
        tenant_id = current_tenant_id()
        AND (has_scope('repo:write') OR has_scope('org:admin'))
    );

DROP POLICY IF EXISTS nodes_tenant_update ON nodes;
CREATE POLICY nodes_tenant_update ON nodes
    FOR UPDATE
    USING (tenant_id = current_tenant_id())
    WITH CHECK (
        tenant_id = current_tenant_id()
        AND (has_scope('repo:write') OR has_scope('org:admin'))
    );

DROP POLICY IF EXISTS nodes_tenant_delete ON nodes;
CREATE POLICY nodes_tenant_delete ON nodes
    FOR DELETE
    USING (
        tenant_id = current_tenant_id()
        AND (has_scope('repo:write') OR has_scope('org:admin'))
    );

-- EDGES
DROP POLICY IF EXISTS edges_tenant_read ON edges;
CREATE POLICY edges_tenant_read ON edges
    FOR SELECT
    USING (tenant_id = current_tenant_id());

DROP POLICY IF EXISTS edges_tenant_insert ON edges;
CREATE POLICY edges_tenant_insert ON edges
    FOR INSERT
    WITH CHECK (
        tenant_id = current_tenant_id()
        AND (has_scope('repo:write') OR has_scope('org:admin'))
    );

DROP POLICY IF EXISTS edges_tenant_update ON edges;
CREATE POLICY edges_tenant_update ON edges
    FOR UPDATE
    USING (tenant_id = current_tenant_id())
    WITH CHECK (
        tenant_id = current_tenant_id()
        AND (has_scope('repo:write') OR has_scope('org:admin'))
    );

DROP POLICY IF EXISTS edges_tenant_delete ON edges;
CREATE POLICY edges_tenant_delete ON edges
    FOR DELETE
    USING (
        tenant_id = current_tenant_id()
        AND (has_scope('repo:write') OR has_scope('org:admin'))
    );

-- INDEXING_JOBS
DROP POLICY IF EXISTS indexing_jobs_tenant_read ON indexing_jobs;
CREATE POLICY indexing_jobs_tenant_read ON indexing_jobs
    FOR SELECT
    USING (tenant_id = current_tenant_id());

DROP POLICY IF EXISTS indexing_jobs_tenant_insert ON indexing_jobs;
CREATE POLICY indexing_jobs_tenant_insert ON indexing_jobs
    FOR INSERT
    WITH CHECK (
        tenant_id = current_tenant_id()
        AND (has_scope('repo:write') OR has_scope('org:admin'))
    );

-- API_KEYS
DROP POLICY IF EXISTS api_keys_tenant_read ON api_keys;
CREATE POLICY api_keys_tenant_read ON api_keys
    FOR SELECT
    USING (tenant_id = current_tenant_id());

DROP POLICY IF EXISTS api_keys_tenant_insert ON api_keys;
CREATE POLICY api_keys_tenant_insert ON api_keys
    FOR INSERT
    WITH CHECK (
        tenant_id = current_tenant_id()
        AND has_scope('org:admin')
    );

-- USERS
ALTER TABLE users ENABLE ROW LEVEL SECURITY;
ALTER TABLE users FORCE ROW LEVEL SECURITY;

DROP POLICY IF EXISTS users_tenant_read ON users;
CREATE POLICY users_tenant_read ON users
    FOR SELECT
    USING (tenant_id = current_tenant_id());

DROP POLICY IF EXISTS users_tenant_update ON users;
CREATE POLICY users_tenant_update ON users
    FOR UPDATE
    USING (tenant_id = current_tenant_id())
    WITH CHECK (tenant_id = current_tenant_id());

-- REFRESH_TOKENS
ALTER TABLE refresh_tokens ENABLE ROW LEVEL SECURITY;
ALTER TABLE refresh_tokens FORCE ROW LEVEL SECURITY;

DROP POLICY IF EXISTS refresh_tokens_user_access ON refresh_tokens;
CREATE POLICY refresh_tokens_user_access ON refresh_tokens
    FOR ALL
    USING (
        user_id IN (
            SELECT id FROM users WHERE tenant_id = current_tenant_id()
        )
    );

-- AUDIT_LOGS
ALTER TABLE audit_logs ENABLE ROW LEVEL SECURITY;
ALTER TABLE audit_logs FORCE ROW LEVEL SECURITY;

DROP POLICY IF EXISTS audit_logs_tenant_read ON audit_logs;
CREATE POLICY audit_logs_tenant_read ON audit_logs
    FOR SELECT
    USING (
        tenant_id = current_tenant_id()
        AND (has_scope('org:admin') OR has_scope('audit:read'))
    );

DROP POLICY IF EXISTS audit_logs_insert ON audit_logs;
CREATE POLICY audit_logs_insert ON audit_logs
    FOR INSERT
    WITH CHECK (tenant_id = current_tenant_id());

-- =============================================================================
-- UPDATE API KEYS PERMISSIONS COLUMN
-- =============================================================================

-- Migrate old 'permissions' JSONB column to 'scopes' if needed
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'repograph'
        AND table_name = 'api_keys'
        AND column_name = 'permissions'
    ) THEN
        -- Copy permissions to scopes if scopes is empty
        UPDATE api_keys
        SET scopes = permissions
        WHERE scopes IS NULL OR scopes = '["repo:read"]';

        -- Drop old column
        ALTER TABLE api_keys DROP COLUMN IF EXISTS permissions;
    END IF;
END $$;

-- =============================================================================
-- COMMENTS
-- =============================================================================

COMMENT ON TABLE users IS 'User accounts for authentication';
COMMENT ON TABLE refresh_tokens IS 'Refresh tokens for JWT token renewal';
COMMENT ON TABLE audit_logs IS 'Audit trail for security-critical events';

COMMENT ON COLUMN api_keys.scopes IS 'List of scopes granted to this API key';
COMMENT ON COLUMN api_keys.repo_id IS 'Optional: scope key to specific repository';

-- =============================================================================
-- VERIFICATION
-- =============================================================================

-- Verify all tables have RLS enabled
DO $$
DECLARE
    v_table RECORD;
    v_count INTEGER;
BEGIN
    SELECT COUNT(*)
    INTO v_count
    FROM pg_tables t
    JOIN pg_class c ON c.relname = t.tablename
    WHERE t.schemaname = 'repograph'
    AND t.tablename IN ('tenants', 'repositories', 'nodes', 'edges', 'indexing_jobs', 'api_keys', 'users', 'refresh_tokens', 'audit_logs')
    AND c.relrowsecurity = false;

    IF v_count > 0 THEN
        RAISE WARNING 'Some tables do not have RLS enabled!';
    ELSE
        RAISE NOTICE 'All tables have RLS enabled successfully';
    END IF;
END $$;
