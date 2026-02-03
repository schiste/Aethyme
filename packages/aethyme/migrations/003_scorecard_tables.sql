-- Migration: Add scorecard tables
-- Created: 2025-11-22
-- Description: Creates tables for AI-readiness scorecard feature

-- Create scorecard_scans table
CREATE TABLE IF NOT EXISTS aethyme.scorecard_scans (
    id UUID PRIMARY KEY,
    repository_id UUID NOT NULL REFERENCES aethyme.repositories(id) ON DELETE CASCADE,
    tenant_id UUID NOT NULL REFERENCES aethyme.tenants(id) ON DELETE CASCADE,
    status VARCHAR(20) NOT NULL CHECK (status IN ('pending', 'running', 'completed', 'failed')),

    -- Scan results
    score INTEGER CHECK (score >= 0 AND score <= 100),
    blocker_count INTEGER DEFAULT 0,
    warning_count INTEGER DEFAULT 0,
    info_count INTEGER DEFAULT 0,
    total_findings INTEGER DEFAULT 0,

    -- Performance metrics
    scan_time_ms FLOAT,
    files_scanned INTEGER,

    -- Full report
    report_json JSONB,

    -- Error tracking
    error TEXT,

    -- Timestamps
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,

    -- Indexes for common queries
    CONSTRAINT scorecard_scans_tenant_fk FOREIGN KEY (tenant_id) REFERENCES aethyme.tenants(id)
);

-- Create indexes for efficient queries
CREATE INDEX IF NOT EXISTS idx_scorecard_scans_repository ON aethyme.scorecard_scans(repository_id, tenant_id);
CREATE INDEX IF NOT EXISTS idx_scorecard_scans_status ON aethyme.scorecard_scans(status, tenant_id);
CREATE INDEX IF NOT EXISTS idx_scorecard_scans_created ON aethyme.scorecard_scans(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_scorecard_scans_completed ON aethyme.scorecard_scans(completed_at DESC) WHERE status = 'completed';

-- Add RLS policy for scorecard_scans
ALTER TABLE aethyme.scorecard_scans ENABLE ROW LEVEL SECURITY;

-- Policy: Users can only see scans for their tenant
CREATE POLICY scorecard_scans_tenant_isolation ON aethyme.scorecard_scans
    FOR ALL
    USING (tenant_id = current_setting('app.tenant_id')::UUID);

-- Grant permissions
GRANT SELECT, INSERT, UPDATE, DELETE ON aethyme.scorecard_scans TO aethyme_app;

-- Add comment for documentation
COMMENT ON TABLE aethyme.scorecard_scans IS 'Stores AI-readiness scorecard scan results';
COMMENT ON COLUMN aethyme.scorecard_scans.score IS 'Overall AI-readiness score (0-100)';
COMMENT ON COLUMN aethyme.scorecard_scans.report_json IS 'Full scorecard report with findings';
