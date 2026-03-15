//! Schema definition and migration for the SurrealDB graph store.

use surrealdb::Surreal;
use surrealdb::engine::local::Db;

/// Apply the full graph schema. Idempotent — uses DEFINE IF NOT EXISTS semantics.
pub async fn apply(db: &Surreal<Db>) -> Result<(), surrealdb::Error> {
    db.query(SCHEMA).await?;
    Ok(())
}

const SCHEMA: &str = r#"
-- ============================================================
-- STRUCTURAL LAYER
-- ============================================================

DEFINE TABLE IF NOT EXISTS area SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS name       ON area TYPE string;
DEFINE FIELD IF NOT EXISTS parent     ON area TYPE option<record<area>>;
DEFINE FIELD IF NOT EXISTS depth      ON area TYPE int;
DEFINE FIELD IF NOT EXISTS file_count ON area TYPE int;
DEFINE FIELD IF NOT EXISTS role       ON area TYPE option<string>;
DEFINE INDEX IF NOT EXISTS idx_area_depth ON area FIELDS depth;

DEFINE TABLE IF NOT EXISTS file SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS path         ON file TYPE string;
DEFINE FIELD IF NOT EXISTS area         ON file TYPE option<record<area>>;
DEFINE FIELD IF NOT EXISTS role         ON file TYPE string;
DEFINE FIELD IF NOT EXISTS language     ON file TYPE option<string>;
DEFINE FIELD IF NOT EXISTS line_count   ON file TYPE option<int>;
DEFINE FIELD IF NOT EXISTS size_bytes   ON file TYPE option<int>;
DEFINE FIELD IF NOT EXISTS content_hash ON file TYPE option<string>;
DEFINE INDEX IF NOT EXISTS idx_file_path ON file FIELDS path UNIQUE;
DEFINE INDEX IF NOT EXISTS idx_file_area ON file FIELDS area;
DEFINE INDEX IF NOT EXISTS idx_file_lang ON file FIELDS language;

-- ============================================================
-- SYMBOL LAYER
-- ============================================================

DEFINE TABLE IF NOT EXISTS symbol SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS name       ON symbol TYPE string;
DEFINE FIELD IF NOT EXISTS kind       ON symbol TYPE string;
DEFINE FIELD IF NOT EXISTS file       ON symbol TYPE record<file>;
DEFINE FIELD IF NOT EXISTS line       ON symbol TYPE option<int>;
DEFINE FIELD IF NOT EXISTS signature  ON symbol TYPE option<string>;
DEFINE FIELD IF NOT EXISTS visibility ON symbol TYPE option<string>;
DEFINE FIELD IF NOT EXISTS language   ON symbol TYPE option<string>;
DEFINE INDEX IF NOT EXISTS idx_symbol_name ON symbol FIELDS name;
DEFINE INDEX IF NOT EXISTS idx_symbol_file ON symbol FIELDS file;

-- ============================================================
-- EDGE LAYER (typed relations)
-- ============================================================

DEFINE TABLE IF NOT EXISTS imports       TYPE RELATION FROM file   TO file | symbol;
DEFINE TABLE IF NOT EXISTS calls         TYPE RELATION FROM symbol TO symbol;
DEFINE TABLE IF NOT EXISTS extends_rel   TYPE RELATION FROM symbol TO symbol;
DEFINE TABLE IF NOT EXISTS configures    TYPE RELATION FROM file   TO file;
DEFINE TABLE IF NOT EXISTS tests         TYPE RELATION FROM file   TO file;
DEFINE TABLE IF NOT EXISTS contains      TYPE RELATION FROM area   TO file;
DEFINE TABLE IF NOT EXISTS entrypoint_for TYPE RELATION FROM file  TO area;

-- Shared fields on relation tables
DEFINE FIELD IF NOT EXISTS confidence ON imports       TYPE option<float>;
DEFINE FIELD IF NOT EXISTS line       ON imports       TYPE option<int>;
DEFINE FIELD IF NOT EXISTS source     ON imports       TYPE option<string>;

DEFINE FIELD IF NOT EXISTS confidence ON calls         TYPE option<float>;
DEFINE FIELD IF NOT EXISTS line       ON calls         TYPE option<int>;

DEFINE FIELD IF NOT EXISTS confidence ON extends_rel   TYPE option<float>;

DEFINE FIELD IF NOT EXISTS confidence ON configures    TYPE option<float>;
DEFINE FIELD IF NOT EXISTS confidence ON tests         TYPE option<float>;
DEFINE FIELD IF NOT EXISTS confidence ON contains      TYPE option<float>;
DEFINE FIELD IF NOT EXISTS confidence ON entrypoint_for TYPE option<float>;

-- ============================================================
-- METADATA
-- ============================================================

DEFINE TABLE IF NOT EXISTS repo SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS root_path   ON repo TYPE string;
DEFINE FIELD IF NOT EXISTS commit_hash ON repo TYPE option<string>;
DEFINE FIELD IF NOT EXISTS indexed_at  ON repo TYPE datetime;
DEFINE FIELD IF NOT EXISTS file_count  ON repo TYPE int;
DEFINE FIELD IF NOT EXISTS languages   ON repo TYPE option<array<string>>;

DEFINE TABLE IF NOT EXISTS risk SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS scope  ON risk TYPE string;
DEFINE FIELD IF NOT EXISTS area   ON risk TYPE option<record<area>>;
DEFINE FIELD IF NOT EXISTS level  ON risk TYPE string;
DEFINE FIELD IF NOT EXISTS reason ON risk TYPE string;

-- ============================================================
-- OPTIONAL: Embeddings
-- ============================================================

DEFINE TABLE IF NOT EXISTS embedding SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS file         ON embedding TYPE record<file>;
DEFINE FIELD IF NOT EXISTS model        ON embedding TYPE string;
DEFINE FIELD IF NOT EXISTS vector       ON embedding TYPE array<float>;
DEFINE FIELD IF NOT EXISTS content_hash ON embedding TYPE string;
DEFINE FIELD IF NOT EXISTS computed_at  ON embedding TYPE datetime;
"#;
