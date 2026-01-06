-- RepoGraph Initial Schema
-- Multi-tenant support with row-level security

-- Create schema
CREATE SCHEMA IF NOT EXISTS repograph;
SET search_path TO repograph, public;

-- Enable extensions
CREATE EXTENSION IF NOT EXISTS "uuid-ossp";
CREATE EXTENSION IF NOT EXISTS "pg_trgm";  -- For fuzzy text search

-- Tenants table (for multi-repo future)
CREATE TABLE IF NOT EXISTS tenants (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    name VARCHAR(255) UNIQUE NOT NULL,
    description TEXT,
    settings JSONB DEFAULT '{}',
    created_at TIMESTAMPTZ DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ DEFAULT CURRENT_TIMESTAMP
);

-- Create default tenant for Aeptus
INSERT INTO tenants (name, description)
VALUES ('aeptus', 'Default tenant for Aeptus codebase')
ON CONFLICT (name) DO NOTHING;

-- Repositories table
CREATE TABLE IF NOT EXISTS repositories (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name VARCHAR(255) NOT NULL,
    path VARCHAR(512) NOT NULL,
    languages TEXT[] DEFAULT ARRAY['python', 'typescript'],
    last_indexed_at TIMESTAMPTZ,
    index_status VARCHAR(50) DEFAULT 'pending',
    metadata JSONB DEFAULT '{}',
    created_at TIMESTAMPTZ DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(tenant_id, name)
);

-- Nodes table with tenant isolation
CREATE TABLE IF NOT EXISTS nodes (
    id VARCHAR(64) PRIMARY KEY,
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
    metadata JSONB DEFAULT '{}',
    indexed_at TIMESTAMPTZ DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(tenant_id, repository_id, symbol, file_path, line_number, column_number)
);

-- Edges table with tenant isolation
CREATE TABLE IF NOT EXISTS edges (
    id VARCHAR(64) PRIMARY KEY,
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    repository_id UUID NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    from_node_id VARCHAR(64) NOT NULL,
    to_node_id VARCHAR(64) NOT NULL,
    edge_type VARCHAR(32) NOT NULL CHECK (edge_type IN ('invoke', 'import', 'contain', 'inherit', 'implement', 'props_flow', 'return', 'parameter')),
    weight FLOAT DEFAULT 1.0,
    metadata JSONB DEFAULT '{}',
    created_at TIMESTAMPTZ DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (from_node_id) REFERENCES nodes(id) ON DELETE CASCADE,
    FOREIGN KEY (to_node_id) REFERENCES nodes(id) ON DELETE CASCADE,
    UNIQUE(tenant_id, repository_id, from_node_id, to_node_id, edge_type)
);

-- Index job tracking
CREATE TABLE IF NOT EXISTS indexing_jobs (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    repository_id UUID NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    status VARCHAR(50) NOT NULL DEFAULT 'pending',
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    error_message TEXT,
    statistics JSONB DEFAULT '{}',
    created_at TIMESTAMPTZ DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ DEFAULT CURRENT_TIMESTAMP
);

-- API Keys for authentication
CREATE TABLE IF NOT EXISTS api_keys (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name VARCHAR(255) NOT NULL,
    key_hash VARCHAR(255) UNIQUE NOT NULL,
    permissions JSONB DEFAULT '["read"]',
    last_used_at TIMESTAMPTZ,
    expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ DEFAULT CURRENT_TIMESTAMP,
    revoked_at TIMESTAMPTZ,
    UNIQUE(tenant_id, name)
);

-- Create indexes for performance
CREATE INDEX idx_nodes_tenant_symbol ON nodes(tenant_id, symbol);
CREATE INDEX idx_nodes_tenant_kind ON nodes(tenant_id, kind);
CREATE INDEX idx_nodes_repository ON nodes(repository_id);
CREATE INDEX idx_nodes_file_path ON nodes(file_path);
CREATE INDEX idx_nodes_search ON nodes USING GIN(search_vector);
CREATE INDEX idx_nodes_symbol_trgm ON nodes USING GIN(symbol gin_trgm_ops);

CREATE INDEX idx_edges_tenant_from ON edges(tenant_id, from_node_id);
CREATE INDEX idx_edges_tenant_to ON edges(tenant_id, to_node_id);
CREATE INDEX idx_edges_type ON edges(edge_type);
CREATE INDEX idx_edges_repository ON edges(repository_id);

CREATE INDEX idx_jobs_tenant_status ON indexing_jobs(tenant_id, status);
CREATE INDEX idx_jobs_repository ON indexing_jobs(repository_id);

-- Row-level security policies
ALTER TABLE nodes ENABLE ROW LEVEL SECURITY;
ALTER TABLE edges ENABLE ROW LEVEL SECURITY;
ALTER TABLE repositories ENABLE ROW LEVEL SECURITY;
ALTER TABLE indexing_jobs ENABLE ROW LEVEL SECURITY;

-- Create RLS policies
CREATE POLICY tenant_isolation_nodes ON nodes
    USING (tenant_id = current_setting('app.current_tenant', true)::uuid);

CREATE POLICY tenant_isolation_edges ON edges
    USING (tenant_id = current_setting('app.current_tenant', true)::uuid);

CREATE POLICY tenant_isolation_repositories ON repositories
    USING (tenant_id = current_setting('app.current_tenant', true)::uuid);

CREATE POLICY tenant_isolation_jobs ON indexing_jobs
    USING (tenant_id = current_setting('app.current_tenant', true)::uuid);

-- Update triggers
CREATE OR REPLACE FUNCTION update_updated_at()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = CURRENT_TIMESTAMP;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER update_tenants_updated_at BEFORE UPDATE ON tenants
    FOR EACH ROW EXECUTE FUNCTION update_updated_at();

CREATE TRIGGER update_repositories_updated_at BEFORE UPDATE ON repositories
    FOR EACH ROW EXECUTE FUNCTION update_updated_at();

CREATE TRIGGER update_jobs_updated_at BEFORE UPDATE ON indexing_jobs
    FOR EACH ROW EXECUTE FUNCTION update_updated_at();

-- Add comments for documentation
COMMENT ON SCHEMA repograph IS 'RepoGraph code intelligence system';
COMMENT ON TABLE tenants IS 'Multi-tenant isolation for different codebases';
COMMENT ON TABLE repositories IS 'Code repositories to index';
COMMENT ON TABLE nodes IS 'Code symbols (definitions and references)';
COMMENT ON TABLE edges IS 'Relationships between code symbols';
COMMENT ON TABLE indexing_jobs IS 'Background job tracking for indexing';
COMMENT ON TABLE api_keys IS 'API authentication keys';