# Aethyme Migration Plan

**Version:** 1.0
**Date:** 2025-11-22
**Status:** Pre-Migration Planning

---

## Overview

This document outlines the migration strategy for transitioning **existing code intelligence data** (if any) into the new Aethyme platform.

**Scenarios:**

1. **Fresh Start** - No existing data (greenfield)
2. **Legacy Code Search** - Migrate from existing code search tool
3. **Manual Documentation** - Import from human-maintained code maps
4. **Other Graph System** - Migrate from Neo4j, JanusGraph, etc.

---

## Scenario 1: Fresh Start (Greenfield)

**Assumption:** No existing code intelligence data.

### Migration Steps

**✅ No migration required!**

Simply:

1. Install Aethyme (see `deployment.md`)
2. Register repositories via API/CLI
3. Trigger initial indexing
4. Wait for indexing to complete

**Timeline:** Immediate (0 days)

---

## Scenario 2: Migrate from Legacy Code Search Tool

**Example:** Migrating from Sourcegraph, OpenGrok, or custom grep-based system.

### Assessment

**Step 1: Inventory existing data**

```bash
# Questions to answer:
1. What code search tool are we using?
2. How many repositories are indexed?
3. What data is stored (symbols, references, metadata)?
4. Where is the data stored (database, filesystem)?
5. What format is the data in (SQL, JSON, proprietary)?
```

**Step 2: Identify data mapping**

| Legacy System | Aethyme Equivalent |
|---------------|----------------------|
| Repository URL | repos.url |
| File path | symbols.file_path |
| Symbol name | symbols.symbol_name |
| Symbol type (function, class) | symbols.kind |
| References/callers | edges (edge_type='calls') |
| Metadata (language, etc.) | symbols.language |

### Migration Strategy

**Option A: Full Re-Index (Recommended)**

**Pros:**
- Fresh, accurate data from source code
- Validates all relationships
- No data quality issues from legacy system

**Cons:**
- Takes time (depends on repo count)
- Temporary unavailability during migration

**Steps:**

```bash
# 1. Export repository list from legacy system
# Example: Sourcegraph
curl https://sourcegraph.com/api/repos | jq '.items[] | {name, url}' > repos.json

# 2. Import repositories into Aethyme
cat repos.json | jq -c '.[]' | while read repo; do
  curl -X POST https://api.aethyme.com/api/v1/repos \
    -H "Authorization: Bearer $TOKEN" \
    -H "Content-Type: application/json" \
    -d "$repo"
done

# 3. Trigger indexing for all repos
curl -X POST https://api.aethyme.com/api/v1/repos/bulk-index \
  -H "Authorization: Bearer $TOKEN"

# 4. Monitor progress
watch -n 10 'curl -s https://api.aethyme.com/api/v1/jobs?status=running | jq ".total"'
```

**Timeline:**

| Repo Count | Estimated Time | Notes |
|------------|----------------|-------|
| <10 | 1-2 hours | Small batch |
| 10-100 | 1 day | Parallel indexing |
| 100-1000 | 3-5 days | Needs worker scaling |
| 1000+ | 1-2 weeks | Gradual rollout |

**Option B: Data Import (If Legacy Data High Quality)**

**Use Case:** Legacy system already has accurate symbol + relationship data.

**Steps:**

1. **Export data from legacy system**

```bash
# Example: Export from SQL database
psql -h legacy-db -U user -d codeindex -c "
  COPY (
    SELECT file_path, symbol_name, symbol_type, line_number
    FROM symbols
  ) TO STDOUT CSV HEADER
" > legacy_symbols.csv
```

2. **Transform to Aethyme format**

```python
# migrate.py
import csv
import uuid
import hashlib

def transform_symbol(row):
    """Transform legacy symbol to Aethyme format."""
    return {
        "id": hashlib.sha256(f"{row['file_path']}:{row['symbol_name']}:{row['line_number']}".encode()).hexdigest()[:64],
        "org_id": ORG_ID,
        "repo_id": REPO_ID,
        "symbol_name": row["symbol_name"],
        "kind": map_symbol_type(row["symbol_type"]),
        "language": detect_language(row["file_path"]),
        "file_path": row["file_path"],
        "line_number": int(row["line_number"]),
        "col_number": 0,
        "indexed_at": "NOW()"
    }

def map_symbol_type(legacy_type):
    """Map legacy symbol types to Aethyme kinds."""
    mapping = {
        "func": "function",
        "cls": "class",
        "meth": "method",
        "var": "variable"
    }
    return mapping.get(legacy_type, "unknown")

# Read and transform
with open("legacy_symbols.csv") as f:
    reader = csv.DictReader(f)
    symbols = [transform_symbol(row) for row in reader]

# Write to Aethyme import format
import json
with open("aethyme_symbols.jsonl", "w") as f:
    for symbol in symbols:
        f.write(json.dumps(symbol) + "\n")
```

3. **Import into Aethyme**

```bash
# Bulk import via PostgreSQL COPY
psql $DATABASE_URL -c "
  COPY symbols (id, org_id, repo_id, symbol_name, kind, language, file_path, line_number)
  FROM '/path/to/aethyme_symbols.jsonl'
  WITH (FORMAT csv, HEADER true);
"

# Or via API (slower but safer)
cat aethyme_symbols.jsonl | while read line; do
  curl -X POST https://api.aethyme.com/api/v1/symbols \
    -H "Authorization: Bearer $TOKEN" \
    -d "$line"
done
```

4. **Validate import**

```sql
-- Check row counts
SELECT COUNT(*) FROM symbols WHERE org_id = '{ORG_ID}';

-- Compare with legacy system
-- Legacy: 50,000 symbols
-- Aethyme: 50,000 symbols ✓

-- Sample check
SELECT * FROM symbols ORDER BY RANDOM() LIMIT 10;
```

**Timeline:**

| Data Volume | Export | Transform | Import | Validate | Total |
|-------------|--------|-----------|--------|----------|-------|
| <100k symbols | 1 hour | 2 hours | 1 hour | 1 hour | 5 hours |
| 100k-1M | 4 hours | 8 hours | 4 hours | 2 hours | 18 hours |
| 1M-10M | 1 day | 2 days | 1 day | 4 hours | 4.5 days |

---

## Scenario 3: Import from Manual Documentation

**Use Case:** Team maintains code maps in Confluence, Notion, or wiki.

### Example: Code dependency graph in Markdown

```markdown
# Authentication Flow

## Components
- `AuthMiddleware` (backend/auth/middleware.py)
- `User` model (backend/users/models.py)
- `AuthService` (backend/auth/service.py)

## Relationships
- AuthMiddleware calls AuthService.authenticate()
- AuthService queries User model
- User model validates credentials
```

### Migration Strategy

**Option A: Manual Registration (Small Scale)**

```bash
# 1. Register key symbols via API
curl -X POST https://api.aethyme.com/api/v1/symbols \
  -H "Authorization: Bearer $TOKEN" \
  -d '{
    "symbol_name": "AuthMiddleware.authenticate",
    "kind": "method",
    "file_path": "backend/auth/middleware.py",
    "line_number": 42
  }'

# 2. Create relationships
curl -X POST https://api.aethyme.com/api/v1/edges \
  -H "Authorization: Bearer $TOKEN" \
  -d '{
    "source": "AuthMiddleware.authenticate",
    "target": "AuthService.authenticate",
    "edge_type": "calls"
  }'
```

**Option B: Parse Documentation (Automated)**

```python
# parse_docs.py
import re

def parse_markdown_code_map(md_text):
    """Extract symbols and relationships from markdown."""
    symbols = []
    edges = []

    # Extract symbols: `SymbolName` (file/path.py)
    symbol_pattern = r'`([^`]+)`\s+\(([^)]+)\)'
    for match in re.finditer(symbol_pattern, md_text):
        symbols.append({
            "symbol_name": match.group(1),
            "file_path": match.group(2),
            "kind": "component"  # Default kind
        })

    # Extract relationships: A calls B
    rel_pattern = r'(\w+)\s+(calls|uses|depends on|queries)\s+(\w+)'
    for match in re.finditer(rel_pattern, md_text):
        edges.append({
            "source": match.group(1),
            "target": match.group(3),
            "edge_type": "calls"  # Normalize to Aethyme types
        })

    return symbols, edges

# Usage
with open("code_map.md") as f:
    symbols, edges = parse_markdown_code_map(f.read())

# Import via API
# ... (similar to above)
```

**Timeline:**

| Documentation Size | Parsing | Manual Review | Import | Total |
|--------------------|---------|---------------|--------|-------|
| <10 pages | 1 hour | 2 hours | 1 hour | 4 hours |
| 10-50 pages | 4 hours | 1 day | 2 hours | 1.5 days |
| 50+ pages | 1 day | 2 days | 4 hours | 3.5 days |

**Recommendation:** After import, **trigger full re-index** to validate and enrich data.

---

## Scenario 4: Migrate from Graph Database (Neo4j, etc.)

**Use Case:** Existing code graph in Neo4j or similar.

### Export from Neo4j

```cypher
// Export nodes (symbols)
MATCH (n:Symbol)
RETURN n.id AS id,
       n.name AS symbol_name,
       n.kind AS kind,
       n.file AS file_path,
       n.line AS line_number

// Save to CSV
CALL apoc.export.csv.query(
  "MATCH (n:Symbol) RETURN n.id, n.name, n.kind, n.file, n.line",
  "symbols.csv",
  {}
)

// Export edges (relationships)
MATCH (a:Symbol)-[r:CALLS]->(b:Symbol)
RETURN a.id AS source_id,
       b.id AS target_id,
       type(r) AS edge_type

CALL apoc.export.csv.query(
  "MATCH (a:Symbol)-[r]->(b:Symbol) RETURN a.id, b.id, type(r)",
  "edges.csv",
  {}
)
```

### Transform and Import

```python
# transform_neo4j.py
import csv
import hashlib

def transform_neo4j_export():
    """Transform Neo4j export to Aethyme format."""

    # Read symbols
    with open("symbols.csv") as f:
        reader = csv.DictReader(f)
        symbols = []
        for row in reader:
            symbols.append({
                "id": hashlib.sha256(f"{row['file_path']}:{row['symbol_name']}:{row['line_number']}".encode()).hexdigest()[:64],
                "org_id": ORG_ID,
                "repo_id": REPO_ID,
                "symbol_name": row["symbol_name"],
                "kind": row["kind"].lower(),
                "file_path": row["file_path"],
                "line_number": int(row["line_number"])
            })

    # Read edges
    with open("edges.csv") as f:
        reader = csv.DictReader(f)
        edges = []
        for row in reader:
            edges.append({
                "source_id": row["source_id"],
                "target_id": row["target_id"],
                "edge_type": row["edge_type"].lower()
            })

    return symbols, edges

symbols, edges = transform_neo4j_export()

# Import to PostgreSQL
# ... (bulk COPY or API)
```

**Timeline:**

| Neo4j Graph Size | Export | Transform | Import | Validate | Total |
|------------------|--------|-----------|--------|----------|-------|
| <100k nodes | 1 hour | 1 hour | 1 hour | 30 min | 3.5 hours |
| 100k-1M nodes | 4 hours | 4 hours | 4 hours | 1 hour | 13 hours |
| 1M-10M nodes | 1 day | 1 day | 1 day | 2 hours | 3 days |

---

## Migration Validation

**Validation Checklist:**

1. **Data Completeness**

```sql
-- Check row counts
SELECT COUNT(*) AS total_symbols FROM symbols WHERE org_id = '{ORG_ID}';
SELECT COUNT(*) AS total_edges FROM edges WHERE org_id = '{ORG_ID}';

-- Compare with source system
-- Source: 50,000 symbols, 150,000 edges
-- Aethyme: 50,000 symbols, 150,000 edges ✓
```

2. **Sample Validation**

```sql
-- Random sample check
SELECT * FROM symbols
WHERE org_id = '{ORG_ID}'
ORDER BY RANDOM()
LIMIT 10;

-- Expected: Symbols look correct (name, file, line)
```

3. **Graph Integrity**

```sql
-- Check for orphaned edges (edge references non-existent symbol)
SELECT COUNT(*)
FROM edges e
LEFT JOIN symbols s_from ON e.source_id = s_from.id
LEFT JOIN symbols s_to ON e.target_id = s_to.id
WHERE s_from.id IS NULL OR s_to.id IS NULL;

-- Expected: 0
```

4. **Query Testing**

```bash
# Test ego graph
curl "https://api.aethyme.com/api/v1/query/ego?symbol=MyClass&depth=2" \
  -H "Authorization: Bearer $TOKEN"

# Expected: Returns graph (validate against known relationships)
```

5. **Performance Check**

```bash
# Run load test
locust --users 100 --spawn-rate 10 --run-time 5m

# Expected: Meets performance budgets (p95 < 2s)
```

---

## Rollback Plan

**If migration fails:**

1. **Restore from backup**

```bash
# PostgreSQL restore
pg_restore -h localhost -U postgres -d aethyme /backups/pre_migration.dump

# Or RDS snapshot restore
aws rds restore-db-instance-from-db-snapshot \
  --db-instance-identifier aethyme-restored \
  --db-snapshot-identifier pre-migration-snapshot
```

2. **Revert application deployment**

```bash
# Kubernetes rollback
kubectl rollout undo deployment/aethyme-api -n aethyme

# Or redeploy previous version
kubectl set image deployment/aethyme-api api=aethyme/api:v0.9.0 -n aethyme
```

3. **Notify users**

```
Subject: Aethyme Migration Rollback

We've rolled back the Aethyme migration due to [REASON].
The system is now running on the previous version.

We'll communicate the new migration timeline shortly.
```

---

## Migration Timeline (Summary)

| Scenario | Duration | Risk | Recommendation |
|----------|----------|------|----------------|
| **Fresh Start** | 0 days | Low | Default |
| **Full Re-Index (100 repos)** | 1 day | Low | **Recommended** |
| **Data Import (100k symbols)** | 1 day | Medium | Use if legacy data high quality |
| **Manual Docs (10 pages)** | 4 hours | Low | Small scale only |
| **Neo4j Migration (1M nodes)** | 3 days | Medium | Test in staging first |

---

## Pre-Migration Checklist

- [ ] Backup existing data (if any)
- [ ] Test migration in staging environment
- [ ] Validate data mapping (legacy → Aethyme)
- [ ] Set up monitoring (track progress)
- [ ] Communicate timeline to users
- [ ] Prepare rollback plan
- [ ] Schedule maintenance window (if needed)

---

## Post-Migration Tasks

- [ ] Validate data completeness (row counts)
- [ ] Run sample queries (spot check)
- [ ] Check graph integrity (no orphaned edges)
- [ ] Performance test (meets budgets)
- [ ] Update documentation
- [ ] Train users on new system
- [ ] Decommission legacy system (after 30-day soak period)

---

**Document Status:** ✅ Complete - Migration Strategy Defined
**Next Steps:** Assess existing systems, choose migration scenario, execute in staging
