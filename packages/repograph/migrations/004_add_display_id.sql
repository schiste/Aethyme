-- Add human-readable deterministic display ID for nodes

ALTER TABLE repograph.nodes
ADD COLUMN IF NOT EXISTS display_id VARCHAR(512);

CREATE INDEX IF NOT EXISTS idx_nodes_display_id
    ON repograph.nodes (display_id);
