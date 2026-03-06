-- Aethyme Initial Schema
-- Canonical hierarchy: Platform > Org > Tenant > Repository > Graph

CREATE SCHEMA IF NOT EXISTS aethyme;
SET search_path TO aethyme, public;

CREATE EXTENSION IF NOT EXISTS "pgcrypto";
CREATE EXTENSION IF NOT EXISTS "pg_trgm";

CREATE TABLE IF NOT EXISTS orgs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(255) NOT NULL,
    slug VARCHAR(100) UNIQUE NOT NULL,
    description TEXT,
    settings JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS tenants (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id UUID NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    name VARCHAR(255) NOT NULL,
    slug VARCHAR(100) NOT NULL,
    description TEXT,
    settings JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(org_id, name),
    UNIQUE(org_id, slug)
);

CREATE TABLE IF NOT EXISTS users (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id UUID NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    tenant_id UUID REFERENCES tenants(id) ON DELETE SET NULL,
    email VARCHAR(255) UNIQUE NOT NULL,
    password_hash VARCHAR(255),
    scopes JSONB NOT NULL DEFAULT '["repo:read"]'::jsonb,
    is_active BOOLEAN NOT NULL DEFAULT TRUE,
    last_login_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS repositories (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id UUID NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name VARCHAR(255) NOT NULL,
    path VARCHAR(512) NOT NULL,
    languages TEXT[] NOT NULL DEFAULT ARRAY['python', 'typescript'],
    last_indexed_at TIMESTAMPTZ,
    index_status VARCHAR(50) NOT NULL DEFAULT 'pending',
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(tenant_id, name)
);

CREATE TABLE IF NOT EXISTS nodes (
    id VARCHAR(64) PRIMARY KEY,
    display_id TEXT,
    org_id UUID NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    repository_id UUID NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    symbol VARCHAR(512) NOT NULL,
    file_path VARCHAR(512) NOT NULL,
    line_number INTEGER NOT NULL,
    column_number INTEGER NOT NULL,
    kind VARCHAR(32) NOT NULL CHECK (kind IN ('def', 'ref', 'file', 'class', 'function', 'method', 'variable', 'import')),
    language VARCHAR(32) NOT NULL,
    signature TEXT,
    documentation TEXT,
    search_vector tsvector GENERATED ALWAYS AS (
        setweight(to_tsvector('english', coalesce(symbol, '')), 'A') ||
        setweight(to_tsvector('english', coalesce(documentation, '')), 'B')
    ) STORED,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    indexed_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(tenant_id, repository_id, symbol, file_path, line_number, column_number)
);

CREATE TABLE IF NOT EXISTS edges (
    id VARCHAR(64) PRIMARY KEY,
    org_id UUID NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    repository_id UUID NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    from_node_id VARCHAR(64) NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    to_node_id VARCHAR(64) NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    edge_type VARCHAR(32) NOT NULL CHECK (edge_type IN ('invoke', 'import', 'contain', 'inherit', 'implement', 'props_flow', 'return', 'parameter')),
    weight FLOAT NOT NULL DEFAULT 1.0,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(tenant_id, repository_id, from_node_id, to_node_id, edge_type)
);

CREATE TABLE IF NOT EXISTS indexing_jobs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id UUID NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    repository_id UUID NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    status VARCHAR(50) NOT NULL DEFAULT 'pending',
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    error_message TEXT,
    statistics JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS api_keys (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id UUID NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    repo_id UUID REFERENCES repositories(id) ON DELETE CASCADE,
    name VARCHAR(255) NOT NULL,
    key_hash VARCHAR(255) UNIQUE NOT NULL,
    scopes JSONB NOT NULL DEFAULT '["repo:read"]'::jsonb,
    last_used_at TIMESTAMPTZ,
    expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    revoked_at TIMESTAMPTZ,
    UNIQUE(tenant_id, name)
);

CREATE INDEX IF NOT EXISTS idx_orgs_slug ON orgs(slug);
CREATE INDEX IF NOT EXISTS idx_tenants_org ON tenants(org_id);
CREATE INDEX IF NOT EXISTS idx_tenants_slug ON tenants(org_id, slug);
CREATE INDEX IF NOT EXISTS idx_users_org ON users(org_id);
CREATE INDEX IF NOT EXISTS idx_users_tenant ON users(tenant_id);
CREATE INDEX IF NOT EXISTS idx_users_email ON users(email);
CREATE INDEX IF NOT EXISTS idx_repositories_org ON repositories(org_id);
CREATE INDEX IF NOT EXISTS idx_repositories_tenant ON repositories(tenant_id);
CREATE INDEX IF NOT EXISTS idx_repositories_status ON repositories(tenant_id, index_status);
CREATE INDEX IF NOT EXISTS idx_nodes_tenant_symbol ON nodes(tenant_id, symbol);
CREATE INDEX IF NOT EXISTS idx_nodes_tenant_kind ON nodes(tenant_id, kind);
CREATE INDEX IF NOT EXISTS idx_nodes_repository ON nodes(repository_id);
CREATE INDEX IF NOT EXISTS idx_nodes_search ON nodes USING GIN(search_vector);
CREATE INDEX IF NOT EXISTS idx_nodes_symbol_trgm ON nodes USING GIN(symbol gin_trgm_ops);
CREATE INDEX IF NOT EXISTS idx_edges_tenant_from ON edges(tenant_id, from_node_id);
CREATE INDEX IF NOT EXISTS idx_edges_tenant_to ON edges(tenant_id, to_node_id);
CREATE INDEX IF NOT EXISTS idx_edges_repository ON edges(repository_id);
CREATE INDEX IF NOT EXISTS idx_jobs_tenant_status ON indexing_jobs(tenant_id, status);
CREATE INDEX IF NOT EXISTS idx_api_keys_tenant ON api_keys(tenant_id);
CREATE INDEX IF NOT EXISTS idx_api_keys_user ON api_keys(user_id);

CREATE OR REPLACE FUNCTION update_updated_at()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = CURRENT_TIMESTAMP;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER update_orgs_updated_at BEFORE UPDATE ON orgs
    FOR EACH ROW EXECUTE FUNCTION update_updated_at();
CREATE TRIGGER update_tenants_updated_at BEFORE UPDATE ON tenants
    FOR EACH ROW EXECUTE FUNCTION update_updated_at();
CREATE TRIGGER update_users_updated_at BEFORE UPDATE ON users
    FOR EACH ROW EXECUTE FUNCTION update_updated_at();
CREATE TRIGGER update_repositories_updated_at BEFORE UPDATE ON repositories
    FOR EACH ROW EXECUTE FUNCTION update_updated_at();
CREATE TRIGGER update_jobs_updated_at BEFORE UPDATE ON indexing_jobs
    FOR EACH ROW EXECUTE FUNCTION update_updated_at();

ALTER TABLE orgs ENABLE ROW LEVEL SECURITY;
ALTER TABLE tenants ENABLE ROW LEVEL SECURITY;
ALTER TABLE users ENABLE ROW LEVEL SECURITY;
ALTER TABLE repositories ENABLE ROW LEVEL SECURITY;
ALTER TABLE nodes ENABLE ROW LEVEL SECURITY;
ALTER TABLE edges ENABLE ROW LEVEL SECURITY;
ALTER TABLE indexing_jobs ENABLE ROW LEVEL SECURITY;
ALTER TABLE api_keys ENABLE ROW LEVEL SECURITY;

INSERT INTO orgs (id, name, slug, description)
VALUES ('00000000-0000-0000-0000-000000000001', 'Default Org', 'default', 'Default bootstrap org')
ON CONFLICT (slug) DO NOTHING;

INSERT INTO tenants (id, org_id, name, slug, description)
VALUES (
    '00000000-0000-0000-0000-000000000001',
    '00000000-0000-0000-0000-000000000001',
    'Default Tenant',
    'default',
    'Default bootstrap tenant'
)
ON CONFLICT (org_id, slug) DO NOTHING;

COMMENT ON TABLE orgs IS 'Business owner accounts above tenant isolation';
COMMENT ON TABLE tenants IS 'Runtime isolation boundaries inside an org';
COMMENT ON TABLE repositories IS 'Repositories indexed inside a tenant';
COMMENT ON TABLE nodes IS 'Graph nodes extracted from repositories';
COMMENT ON TABLE edges IS 'Graph edges between indexed nodes';
COMMENT ON TABLE indexing_jobs IS 'Indexing job tracking per tenant and repository';
COMMENT ON TABLE api_keys IS 'Scoped API keys for programmatic access';
