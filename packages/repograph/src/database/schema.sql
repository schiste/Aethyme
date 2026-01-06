-- RepoGraph Database Schema
-- Version: 1.0
-- PostgreSQL 15+
-- Features: Multi-tenant RLS, Vector search (pgvector), Graph queries

-- ============================================================================
-- EXTENSIONS
-- ============================================================================

CREATE EXTENSION IF NOT EXISTS "uuid-ossp";       -- UUID generation
CREATE EXTENSION IF NOT EXISTS "pgcrypto";        -- Cryptographic functions
CREATE EXTENSION IF NOT EXISTS "vector";          -- pgvector for semantic search

-- ============================================================================
-- CORE TABLES
-- ============================================================================

-- ----------------------------------------------------------------------------
-- Organizations (Tenants)
-- ----------------------------------------------------------------------------
CREATE TABLE orgs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(255) NOT NULL,
    slug VARCHAR(100) UNIQUE NOT NULL,  -- URL-safe identifier (e.g., 'acme-corp')

    -- Settings
    settings JSONB DEFAULT '{}'::jsonb,  -- Tenant-specific configuration

    -- Subscription (placeholder for future billing)
    plan VARCHAR(50) DEFAULT 'free',  -- 'free', 'pro', 'enterprise'
    max_repos INT DEFAULT 5,
    max_symbols INT DEFAULT 100000,

    -- Timestamps
    created_at TIMESTAMP DEFAULT NOW(),
    updated_at TIMESTAMP DEFAULT NOW(),
    deleted_at TIMESTAMP DEFAULT NULL  -- Soft delete
);

CREATE INDEX idx_orgs_slug ON orgs(slug);
CREATE INDEX idx_orgs_deleted ON orgs(deleted_at) WHERE deleted_at IS NULL;

COMMENT ON TABLE orgs IS 'Top-level tenant organizations';
COMMENT ON COLUMN orgs.slug IS 'URL-safe unique identifier for organization';

-- ----------------------------------------------------------------------------
-- Users
-- ----------------------------------------------------------------------------
CREATE TABLE users (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    email VARCHAR(255) UNIQUE NOT NULL,
    password_hash VARCHAR(255) NOT NULL,  -- bcrypt hash
    org_id UUID NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,

    -- Profile
    name VARCHAR(255),
    avatar_url TEXT,

    -- RBAC
    role VARCHAR(50) NOT NULL DEFAULT 'member',  -- 'owner', 'admin', 'member', 'readonly'

    -- OIDC integration
    oidc_provider VARCHAR(50),  -- 'auth0', 'keycloak', 'google', etc.
    oidc_subject VARCHAR(255),  -- Subject claim from OIDC provider

    -- Metadata
    last_login TIMESTAMP DEFAULT NULL,
    email_verified BOOLEAN DEFAULT FALSE,
    is_active BOOLEAN DEFAULT TRUE,

    -- Timestamps
    created_at TIMESTAMP DEFAULT NOW(),
    updated_at TIMESTAMP DEFAULT NOW(),
    deleted_at TIMESTAMP DEFAULT NULL  -- Soft delete
);

CREATE INDEX idx_users_email ON users(email);
CREATE INDEX idx_users_org ON users(org_id);
CREATE INDEX idx_users_oidc ON users(oidc_provider, oidc_subject);
CREATE INDEX idx_users_active ON users(is_active) WHERE is_active = TRUE;

COMMENT ON TABLE users IS 'User accounts with organization membership';
COMMENT ON COLUMN users.role IS 'owner: full control | admin: manage settings | member: read/write | readonly: read only';

-- ----------------------------------------------------------------------------
-- API Keys
-- ----------------------------------------------------------------------------
CREATE TABLE api_keys (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id UUID NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,

    -- Key metadata
    name VARCHAR(100) NOT NULL,  -- User-friendly name (e.g., 'CI/CD Pipeline')
    description TEXT,

    -- Key data (NEVER store plain-text keys!)
    key_hash VARCHAR(255) UNIQUE NOT NULL,  -- bcrypt hash of full key
    key_prefix VARCHAR(16) NOT NULL,  -- First 8 chars for display: 'rgph_live_xxxxxxxx...'

    -- Permissions (JSON array of scopes)
    scopes JSONB NOT NULL DEFAULT '["repo:read"]'::jsonb,
    -- Available scopes:
    --   repo:read     - Read repository data
    --   repo:write    - Trigger indexing
    --   query:*       - Run queries
    --   ai:scorecard  - Run AI-readiness scorecard
    --   ai:autofix    - Run autofixers

    -- Lifecycle
    expires_at TIMESTAMP DEFAULT NULL,  -- NULL = never expires
    last_used_at TIMESTAMP DEFAULT NULL,
    is_active BOOLEAN DEFAULT TRUE,
    revoked_at TIMESTAMP DEFAULT NULL,
    revoked_by UUID REFERENCES users(id),
    revoke_reason TEXT,

    -- Timestamps
    created_at TIMESTAMP DEFAULT NOW(),
    updated_at TIMESTAMP DEFAULT NOW(),

    CONSTRAINT unique_key_name UNIQUE(org_id, name),
    CONSTRAINT valid_scopes CHECK (jsonb_typeof(scopes) = 'array')
);

CREATE INDEX idx_api_keys_org ON api_keys(org_id);
CREATE INDEX idx_api_keys_user ON api_keys(user_id);
CREATE INDEX idx_api_keys_prefix ON api_keys(key_prefix);
CREATE INDEX idx_api_keys_active ON api_keys(is_active, expires_at)
    WHERE is_active = TRUE;

COMMENT ON TABLE api_keys IS 'API keys for programmatic access (CI/CD, integrations)';
COMMENT ON COLUMN api_keys.key_hash IS 'bcrypt hash of full key (format: rgph_live_{64_hex_chars})';
COMMENT ON COLUMN api_keys.scopes IS 'JSON array of permission scopes';

-- ----------------------------------------------------------------------------
-- Repositories
-- ----------------------------------------------------------------------------
CREATE TABLE repos (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id UUID NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,

    -- Repository identification
    name VARCHAR(255) NOT NULL,  -- e.g., 'my-project'
    full_name VARCHAR(500),      -- e.g., 'acme-corp/my-project'
    url TEXT NOT NULL,           -- Git clone URL
    provider VARCHAR(50) NOT NULL,  -- 'github', 'gitlab', 'bitbucket', 'other'

    -- Provider-specific IDs
    provider_id VARCHAR(255),    -- GitHub repo ID, GitLab project ID, etc.
    default_branch VARCHAR(100) DEFAULT 'main',

    -- Indexing metadata
    last_indexed_at TIMESTAMP DEFAULT NULL,
    last_indexed_commit VARCHAR(40),  -- Git SHA
    symbol_count INT DEFAULT 0,
    file_count INT DEFAULT 0,

    -- Status tracking
    status VARCHAR(50) DEFAULT 'pending',  -- 'pending', 'cloning', 'indexing', 'completed', 'failed'
    error_message TEXT DEFAULT NULL,

    -- Settings
    auto_index BOOLEAN DEFAULT TRUE,  -- Auto-index on webhook push
    index_schedule VARCHAR(50),       -- Cron expression for scheduled indexing

    -- Timestamps
    created_at TIMESTAMP DEFAULT NOW(),
    updated_at TIMESTAMP DEFAULT NOW(),
    deleted_at TIMESTAMP DEFAULT NULL,

    CONSTRAINT unique_repo_per_org UNIQUE(org_id, provider, url),
    CONSTRAINT valid_status CHECK (status IN ('pending', 'cloning', 'indexing', 'completed', 'failed'))
);

CREATE INDEX idx_repos_org ON repos(org_id);
CREATE INDEX idx_repos_status ON repos(status);
CREATE INDEX idx_repos_provider ON repos(provider, provider_id);
CREATE INDEX idx_repos_updated ON repos(updated_at DESC);

COMMENT ON TABLE repos IS 'Git repositories being indexed';
COMMENT ON COLUMN repos.status IS 'Current indexing status';

-- ----------------------------------------------------------------------------
-- Symbols (Graph Nodes)
-- ----------------------------------------------------------------------------
CREATE TABLE symbols (
    id VARCHAR(64) PRIMARY KEY,  -- Hash of (repo_id, file_path, symbol_name, line_number)
    org_id UUID NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    repo_id UUID NOT NULL REFERENCES repos(id) ON DELETE CASCADE,

    -- Symbol identification
    symbol_name VARCHAR(500) NOT NULL,  -- e.g., 'MyClass.myMethod'
    kind VARCHAR(50) NOT NULL,  -- 'function', 'class', 'method', 'variable', 'import', 'type'
    language VARCHAR(50) NOT NULL,  -- 'python', 'typescript', 'javascript', 'java', etc.

    -- Location in source code
    file_path TEXT NOT NULL,
    line_number INT NOT NULL,
    col_number INT DEFAULT 0,
    end_line INT,
    end_col INT,

    -- Code context
    signature TEXT,  -- Function signature, class definition, etc.
    docstring TEXT,  -- Documentation string
    text TEXT,       -- Source code snippet (up to 20 lines)

    -- Semantic search (OpenAI ada-002 or custom embeddings)
    embedding vector(1536),

    -- Metadata
    is_exported BOOLEAN DEFAULT FALSE,  -- Public API?
    is_deprecated BOOLEAN DEFAULT FALSE,
    tags JSONB DEFAULT '[]'::jsonb,  -- Custom tags

    -- Timestamps
    indexed_at TIMESTAMP DEFAULT NOW(),

    CONSTRAINT unique_symbol UNIQUE(org_id, repo_id, file_path, symbol_name, line_number)
);

-- Performance indexes
CREATE INDEX idx_symbols_org ON symbols(org_id);
CREATE INDEX idx_symbols_repo ON symbols(repo_id);
CREATE INDEX idx_symbols_name ON symbols(symbol_name);
CREATE INDEX idx_symbols_kind ON symbols(kind);
CREATE INDEX idx_symbols_language ON symbols(language);
CREATE INDEX idx_symbols_org_kind ON symbols(org_id, kind);
CREATE INDEX idx_symbols_org_lang ON symbols(org_id, language);
CREATE INDEX idx_symbols_file ON symbols(file_path);

-- Semantic search index (pgvector IVFFlat)
-- Note: Run AFTER inserting data: CREATE INDEX CONCURRENTLY ...
CREATE INDEX idx_symbols_embedding ON symbols
    USING ivfflat (embedding vector_cosine_ops)
    WITH (lists = 100);

COMMENT ON TABLE symbols IS 'Code symbols (functions, classes, etc.) - graph nodes';
COMMENT ON COLUMN symbols.id IS 'Hash of (repo_id, file_path, symbol_name, line_number)';
COMMENT ON COLUMN symbols.embedding IS 'Vector embedding for semantic search (1536 dimensions)';

-- ----------------------------------------------------------------------------
-- Edges (Graph Relationships)
-- ----------------------------------------------------------------------------
CREATE TABLE edges (
    id VARCHAR(64) PRIMARY KEY,  -- Hash of (source_id, target_id, edge_type)
    org_id UUID NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,

    -- Relationship
    source_id VARCHAR(64) NOT NULL REFERENCES symbols(id) ON DELETE CASCADE,
    target_id VARCHAR(64) NOT NULL REFERENCES symbols(id) ON DELETE CASCADE,
    edge_type VARCHAR(50) NOT NULL,  -- 'calls', 'imports', 'extends', 'implements', 'contains', 'references'

    -- Metadata
    weight FLOAT DEFAULT 1.0,  -- Relationship strength (e.g., call frequency)
    context TEXT,  -- Additional context (e.g., call site code)

    -- Timestamps
    created_at TIMESTAMP DEFAULT NOW(),

    CONSTRAINT unique_edge UNIQUE(org_id, source_id, target_id, edge_type),
    CONSTRAINT no_self_loops CHECK (source_id != target_id)
);

-- Performance indexes for graph traversal
CREATE INDEX idx_edges_org ON edges(org_id);
CREATE INDEX idx_edges_source ON edges(source_id);
CREATE INDEX idx_edges_target ON edges(target_id);
CREATE INDEX idx_edges_type ON edges(edge_type);
CREATE INDEX idx_edges_org_source ON edges(org_id, source_id);
CREATE INDEX idx_edges_org_target ON edges(org_id, target_id);
CREATE INDEX idx_edges_org_type ON edges(org_id, edge_type);

-- Composite index for bi-directional traversal
CREATE INDEX idx_edges_bidirectional ON edges(source_id, target_id);

COMMENT ON TABLE edges IS 'Relationships between symbols - graph edges';
COMMENT ON COLUMN edges.edge_type IS 'calls, imports, extends, implements, contains, references';
COMMENT ON COLUMN edges.weight IS 'Relationship strength (higher = more important)';

-- ----------------------------------------------------------------------------
-- AI-Readiness Scorecard Results
-- ----------------------------------------------------------------------------
CREATE TABLE scorecard_results (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id UUID NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    repo_id UUID NOT NULL REFERENCES repos(id) ON DELETE CASCADE,

    -- Scorecard data
    score INT NOT NULL CHECK (score >= 0 AND score <= 100),

    -- Violations (JSON array)
    violations JSONB NOT NULL DEFAULT '[]'::jsonb,
    -- Structure: [{severity, category, message, file, line, evidence}, ...]

    -- Summary statistics
    summary JSONB NOT NULL DEFAULT '{}'::jsonb,
    -- Structure: {total, blockers, warnings, info, by_category: {...}}

    -- Metadata
    git_commit VARCHAR(40),  -- Git SHA at time of scan
    created_at TIMESTAMP DEFAULT NOW(),
    created_by UUID REFERENCES users(id),

    CONSTRAINT valid_violations CHECK (jsonb_typeof(violations) = 'array'),
    CONSTRAINT valid_summary CHECK (jsonb_typeof(summary) = 'object')
);

CREATE INDEX idx_scorecard_org ON scorecard_results(org_id);
CREATE INDEX idx_scorecard_repo ON scorecard_results(repo_id);
CREATE INDEX idx_scorecard_created ON scorecard_results(created_at DESC);
CREATE INDEX idx_scorecard_score ON scorecard_results(score);

COMMENT ON TABLE scorecard_results IS 'AI-readiness scorecard results';
COMMENT ON COLUMN scorecard_results.violations IS 'JSON array of violation objects';
COMMENT ON COLUMN scorecard_results.summary IS 'Summary statistics by severity and category';

-- ----------------------------------------------------------------------------
-- Autofix Results
-- ----------------------------------------------------------------------------
CREATE TABLE autofix_results (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id UUID NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    repo_id UUID NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    scorecard_id UUID REFERENCES scorecard_results(id),

    -- Autofix metadata
    fix_type VARCHAR(50) NOT NULL,  -- 'docs_regen', 'link_fix', 'selector_insertion', 'i18n_scaffold'
    mode VARCHAR(20) NOT NULL,  -- 'dry_run', 'apply', 'pr'

    -- Results
    status VARCHAR(20) NOT NULL,  -- 'pending', 'completed', 'failed'
    fixes_applied INT DEFAULT 0,
    fixes_skipped INT DEFAULT 0,

    -- Details
    details JSONB DEFAULT '{}'::jsonb,  -- Fix-specific details
    diff TEXT,  -- Unified diff
    pr_url TEXT,  -- Pull request URL (if mode = 'pr')

    -- Metadata
    git_commit VARCHAR(40),
    created_at TIMESTAMP DEFAULT NOW(),
    created_by UUID REFERENCES users(id),

    CONSTRAINT valid_mode CHECK (mode IN ('dry_run', 'apply', 'pr')),
    CONSTRAINT valid_status CHECK (status IN ('pending', 'completed', 'failed'))
);

CREATE INDEX idx_autofix_org ON autofix_results(org_id);
CREATE INDEX idx_autofix_repo ON autofix_results(repo_id);
CREATE INDEX idx_autofix_scorecard ON autofix_results(scorecard_id);
CREATE INDEX idx_autofix_created ON autofix_results(created_at DESC);

COMMENT ON TABLE autofix_results IS 'Autofix execution results';

-- ----------------------------------------------------------------------------
-- Background Jobs
-- ----------------------------------------------------------------------------
CREATE TABLE jobs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id UUID NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,

    -- Job identification
    job_type VARCHAR(50) NOT NULL,  -- 'index', 'scorecard', 'autofix', 'query'
    resource_type VARCHAR(50),  -- 'repo', 'symbol', etc.
    resource_id UUID,

    -- Status
    status VARCHAR(20) NOT NULL DEFAULT 'pending',  -- 'pending', 'running', 'completed', 'failed'
    progress INT DEFAULT 0,  -- 0-100

    -- Results
    result JSONB DEFAULT '{}'::jsonb,
    error_message TEXT,

    -- Celery metadata
    celery_task_id VARCHAR(255) UNIQUE,

    -- Timestamps
    created_at TIMESTAMP DEFAULT NOW(),
    started_at TIMESTAMP,
    completed_at TIMESTAMP,

    CONSTRAINT valid_status CHECK (status IN ('pending', 'running', 'completed', 'failed')),
    CONSTRAINT valid_progress CHECK (progress >= 0 AND progress <= 100)
);

CREATE INDEX idx_jobs_org ON jobs(org_id);
CREATE INDEX idx_jobs_status ON jobs(status);
CREATE INDEX idx_jobs_type ON jobs(job_type);
CREATE INDEX idx_jobs_celery ON jobs(celery_task_id);
CREATE INDEX idx_jobs_created ON jobs(created_at DESC);

COMMENT ON TABLE jobs IS 'Background job tracking';

-- ----------------------------------------------------------------------------
-- Audit Logs
-- ----------------------------------------------------------------------------
CREATE TABLE audit_logs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id UUID REFERENCES orgs(id) ON DELETE CASCADE,

    -- Actor
    user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    api_key_id UUID REFERENCES api_keys(id) ON DELETE SET NULL,
    ip_address INET,
    user_agent TEXT,

    -- Action
    action VARCHAR(100) NOT NULL,  -- 'user.login', 'repo.create', 'repo.index', 'query.ego', etc.
    resource_type VARCHAR(50),
    resource_id UUID,

    -- Details
    details JSONB DEFAULT '{}'::jsonb,

    -- Outcome
    status VARCHAR(20) NOT NULL,  -- 'success', 'failure'
    error_message TEXT,

    -- Timestamps
    created_at TIMESTAMP DEFAULT NOW(),

    CONSTRAINT valid_status CHECK (status IN ('success', 'failure'))
);

CREATE INDEX idx_audit_org ON audit_logs(org_id);
CREATE INDEX idx_audit_user ON audit_logs(user_id);
CREATE INDEX idx_audit_action ON audit_logs(action);
CREATE INDEX idx_audit_created ON audit_logs(created_at DESC);

-- Partition by month for scalability
-- ALTER TABLE audit_logs PARTITION BY RANGE (created_at);
-- CREATE TABLE audit_logs_2025_11 PARTITION OF audit_logs
--     FOR VALUES FROM ('2025-11-01') TO ('2025-12-01');

COMMENT ON TABLE audit_logs IS 'Audit trail of all system actions';

-- ============================================================================
-- ROW-LEVEL SECURITY (RLS) POLICIES
-- ============================================================================

-- Enable RLS on all multi-tenant tables
ALTER TABLE users ENABLE ROW LEVEL SECURITY;
ALTER TABLE api_keys ENABLE ROW LEVEL SECURITY;
ALTER TABLE repos ENABLE ROW LEVEL SECURITY;
ALTER TABLE symbols ENABLE ROW LEVEL SECURITY;
ALTER TABLE edges ENABLE ROW LEVEL SECURITY;
ALTER TABLE scorecard_results ENABLE ROW LEVEL SECURITY;
ALTER TABLE autofix_results ENABLE ROW LEVEL SECURITY;
ALTER TABLE jobs ENABLE ROW LEVEL SECURITY;
ALTER TABLE audit_logs ENABLE ROW LEVEL SECURITY;

-- ----------------------------------------------------------------------------
-- RLS Policies: Users
-- ----------------------------------------------------------------------------
CREATE POLICY tenant_isolation_policy ON users
    USING (org_id = current_setting('app.current_org', true)::uuid);

CREATE POLICY tenant_isolation_insert ON users
    FOR INSERT
    WITH CHECK (org_id = current_setting('app.current_org', true)::uuid);

-- ----------------------------------------------------------------------------
-- RLS Policies: API Keys
-- ----------------------------------------------------------------------------
CREATE POLICY tenant_isolation_policy ON api_keys
    USING (org_id = current_setting('app.current_org', true)::uuid);

CREATE POLICY tenant_isolation_insert ON api_keys
    FOR INSERT
    WITH CHECK (org_id = current_setting('app.current_org', true)::uuid);

-- ----------------------------------------------------------------------------
-- RLS Policies: Repos
-- ----------------------------------------------------------------------------
CREATE POLICY tenant_isolation_policy ON repos
    USING (org_id = current_setting('app.current_org', true)::uuid);

CREATE POLICY tenant_isolation_insert ON repos
    FOR INSERT
    WITH CHECK (org_id = current_setting('app.current_org', true)::uuid);

-- ----------------------------------------------------------------------------
-- RLS Policies: Symbols
-- ----------------------------------------------------------------------------
CREATE POLICY tenant_isolation_policy ON symbols
    USING (org_id = current_setting('app.current_org', true)::uuid);

CREATE POLICY tenant_isolation_insert ON symbols
    FOR INSERT
    WITH CHECK (org_id = current_setting('app.current_org', true)::uuid);

-- ----------------------------------------------------------------------------
-- RLS Policies: Edges
-- ----------------------------------------------------------------------------
CREATE POLICY tenant_isolation_policy ON edges
    USING (org_id = current_setting('app.current_org', true)::uuid);

CREATE POLICY tenant_isolation_insert ON edges
    FOR INSERT
    WITH CHECK (org_id = current_setting('app.current_org', true)::uuid);

-- ----------------------------------------------------------------------------
-- RLS Policies: Scorecard Results
-- ----------------------------------------------------------------------------
CREATE POLICY tenant_isolation_policy ON scorecard_results
    USING (org_id = current_setting('app.current_org', true)::uuid);

CREATE POLICY tenant_isolation_insert ON scorecard_results
    FOR INSERT
    WITH CHECK (org_id = current_setting('app.current_org', true)::uuid);

-- ----------------------------------------------------------------------------
-- RLS Policies: Autofix Results
-- ----------------------------------------------------------------------------
CREATE POLICY tenant_isolation_policy ON autofix_results
    USING (org_id = current_setting('app.current_org', true)::uuid);

CREATE POLICY tenant_isolation_insert ON autofix_results
    FOR INSERT
    WITH CHECK (org_id = current_setting('app.current_org', true)::uuid);

-- ----------------------------------------------------------------------------
-- RLS Policies: Jobs
-- ----------------------------------------------------------------------------
CREATE POLICY tenant_isolation_policy ON jobs
    USING (org_id = current_setting('app.current_org', true)::uuid);

CREATE POLICY tenant_isolation_insert ON jobs
    FOR INSERT
    WITH CHECK (org_id = current_setting('app.current_org', true)::uuid);

-- ----------------------------------------------------------------------------
-- RLS Policies: Audit Logs
-- ----------------------------------------------------------------------------
CREATE POLICY tenant_isolation_policy ON audit_logs
    USING (org_id = current_setting('app.current_org', true)::uuid);

CREATE POLICY tenant_isolation_insert ON audit_logs
    FOR INSERT
    WITH CHECK (org_id = current_setting('app.current_org', true)::uuid);

-- ============================================================================
-- FUNCTIONS
-- ============================================================================

-- ----------------------------------------------------------------------------
-- Function: Update updated_at timestamp
-- ----------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION update_updated_at_column()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

-- Apply to tables with updated_at
CREATE TRIGGER update_orgs_updated_at BEFORE UPDATE ON orgs
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

CREATE TRIGGER update_users_updated_at BEFORE UPDATE ON users
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

CREATE TRIGGER update_api_keys_updated_at BEFORE UPDATE ON api_keys
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

CREATE TRIGGER update_repos_updated_at BEFORE UPDATE ON repos
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

-- ----------------------------------------------------------------------------
-- Function: Generate symbol ID (hash)
-- ----------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION generate_symbol_id(
    p_repo_id UUID,
    p_file_path TEXT,
    p_symbol_name VARCHAR,
    p_line_number INT
)
RETURNS VARCHAR AS $$
BEGIN
    RETURN encode(
        digest(
            p_repo_id::text || '::' || p_file_path || '::' || p_symbol_name || '::' || p_line_number::text,
            'sha256'
        ),
        'hex'
    )::varchar(64);
END;
$$ LANGUAGE plpgsql IMMUTABLE;

COMMENT ON FUNCTION generate_symbol_id IS 'Generate deterministic symbol ID from components';

-- ----------------------------------------------------------------------------
-- Function: Generate edge ID (hash)
-- ----------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION generate_edge_id(
    p_source_id VARCHAR,
    p_target_id VARCHAR,
    p_edge_type VARCHAR
)
RETURNS VARCHAR AS $$
BEGIN
    RETURN encode(
        digest(
            p_source_id || '::' || p_target_id || '::' || p_edge_type,
            'sha256'
        ),
        'hex'
    )::varchar(64);
END;
$$ LANGUAGE plpgsql IMMUTABLE;

COMMENT ON FUNCTION generate_edge_id IS 'Generate deterministic edge ID from components';

-- ============================================================================
-- VIEWS
-- ============================================================================

-- ----------------------------------------------------------------------------
-- View: Repository summary with latest scorecard
-- ----------------------------------------------------------------------------
CREATE VIEW v_repo_summary AS
SELECT
    r.id,
    r.org_id,
    r.name,
    r.full_name,
    r.provider,
    r.status,
    r.last_indexed_at,
    r.symbol_count,
    r.file_count,
    sc.score AS latest_scorecard_score,
    sc.created_at AS latest_scorecard_at,
    (SELECT COUNT(*) FROM jobs j WHERE j.resource_id = r.id AND j.status = 'running') AS active_jobs
FROM repos r
LEFT JOIN LATERAL (
    SELECT score, created_at
    FROM scorecard_results
    WHERE repo_id = r.id
    ORDER BY created_at DESC
    LIMIT 1
) sc ON TRUE;

COMMENT ON VIEW v_repo_summary IS 'Repository summary with latest scorecard and active jobs';

-- ============================================================================
-- SAMPLE DATA (Development/Testing)
-- ============================================================================

-- Uncomment for local development

-- -- Create sample organization
-- INSERT INTO orgs (id, name, slug, plan)
-- VALUES ('00000000-0000-0000-0000-000000000001', 'Acme Corporation', 'acme-corp', 'pro');
--
-- -- Create sample user
-- INSERT INTO users (id, email, password_hash, org_id, role)
-- VALUES (
--     '00000000-0000-0000-0000-000000000002',
--     'admin@acme.com',
--     '$2b$12$LQv3c1yqBWVHxkd0LHAkCOYz6TtxMQJqhN8/LewY5pXuxKxCyGGMy',  -- 'password123'
--     '00000000-0000-0000-0000-000000000001',
--     'admin'
-- );

-- ============================================================================
-- MAINTENANCE
-- ============================================================================

-- Vacuum and analyze after bulk operations
-- VACUUM ANALYZE symbols;
-- VACUUM ANALYZE edges;

-- Monitor table sizes
-- SELECT
--     schemaname,
--     tablename,
--     pg_size_pretty(pg_total_relation_size(schemaname||'.'||tablename)) AS size
-- FROM pg_tables
-- WHERE schemaname = 'public'
-- ORDER BY pg_total_relation_size(schemaname||'.'||tablename) DESC;

-- Monitor index usage
-- SELECT
--     schemaname,
--     tablename,
--     indexname,
--     idx_scan,
--     idx_tup_read,
--     idx_tup_fetch
-- FROM pg_stat_user_indexes
-- WHERE schemaname = 'public'
-- ORDER BY idx_scan DESC;
