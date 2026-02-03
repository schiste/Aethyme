-- Add human-readable deterministic display ID for nodes

ALTER TABLE aethyme.nodes
ADD COLUMN IF NOT EXISTS display_id VARCHAR(512);

CREATE INDEX IF NOT EXISTS idx_nodes_display_id
    ON aethyme.nodes (display_id);
