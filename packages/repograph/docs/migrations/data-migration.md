# Data Migration Guide

Guide for migrating data between RepoGraph instances or versions.

---

## Export Data

### Full Database Export

```bash
#!/bin/bash
# scripts/export-data.sh

EXPORT_DIR="/exports/repograph-$(date +%Y%m%d)"
mkdir -p $EXPORT_DIR

# Export PostgreSQL
pg_dump -U repograph -d repograph \
  --format=custom \
  --file=$EXPORT_DIR/repograph.dump

# Export Redis (optional)
redis-cli --rdb $EXPORT_DIR/dump.rdb

# Export configuration
cp .env $EXPORT_DIR/config.env
cp -r k8s/ $EXPORT_DIR/k8s/

# Create tarball
tar -czf repograph-export-$(date +%Y%m%d).tar.gz $EXPORT_DIR

echo "Export complete: repograph-export-$(date +%Y%m%d).tar.gz"
```

### Selective Export (Single Tenant)

```bash
# Export single organization's data
pg_dump -U repograph -d repograph \
  --table=repositories \
  --table=nodes \
  --table=edges \
  --where="org_id='your-org-id'" \
  > org-export.sql
```

---

## Import Data

### Full Database Import

```bash
#!/bin/bash
# scripts/import-data.sh

IMPORT_FILE=$1

# Extract tarball
tar -xzf $IMPORT_FILE

# Import PostgreSQL
pg_restore -U repograph -d repograph \
  --clean \
  --if-exists \
  repograph-*/repograph.dump

# Import Redis
redis-cli FLUSHDB
redis-cli --rdb repograph-*/dump.rdb CONFIG SET dir /var/lib/redis/

echo "Import complete"
```

### Tenant Migration

```sql
-- Migrate data to new tenant
BEGIN;

-- Create new tenant
INSERT INTO tenants (id, name) VALUES ('new-tenant-id', 'New Tenant');

-- Copy repositories
INSERT INTO repositories (id, tenant_id, name, path)
SELECT gen_random_uuid(), 'new-tenant-id', name, path
FROM repositories WHERE tenant_id = 'old-tenant-id';

-- Copy nodes (adjust repository_id)
INSERT INTO nodes (id, repository_id, symbol, kind, language)
SELECT gen_random_uuid(), new_repo.id, n.symbol, n.kind, n.language
FROM nodes n
JOIN repositories old_repo ON n.repository_id = old_repo.id
JOIN repositories new_repo ON new_repo.name = old_repo.name
WHERE old_repo.tenant_id = 'old-tenant-id'
  AND new_repo.tenant_id = 'new-tenant-id';

COMMIT;
```

---

## Cross-Version Migration

### From v1.0 to v1.2

```bash
# 1. Export from v1.0
# On v1.0 instance:
pg_dump -U repograph -d repograph > v1.0-export.sql

# 2. Transfer to v1.2 instance
scp v1.0-export.sql new-server:/tmp/

# 3. Import and migrate
# On v1.2 instance:
psql -U repograph -d repograph < /tmp/v1.0-export.sql
alembic upgrade head  # Apply v1.1 and v1.2 migrations

# 4. Verify
python -m src.cli stats
```

---

## Cloud Provider Migration

### AWS RDS to Google Cloud SQL

```bash
# 1. Create snapshot
aws rds create-db-snapshot \
  --db-snapshot-identifier repograph-migration \
  --db-instance-identifier repograph-prod

# 2. Export snapshot to S3
aws rds start-export-task \
  --export-task-identifier repograph-export \
  --source-arn arn:aws:rds:us-east-1:123456789012:snapshot:repograph-migration \
  --s3-bucket-name repograph-exports

# 3. Download from S3
aws s3 sync s3://repograph-exports /tmp/export/

# 4. Upload to GCS
gsutil -m rsync -r /tmp/export gs://repograph-imports/

# 5. Import to Cloud SQL
gcloud sql import sql repograph-instance \
  gs://repograph-imports/export.sql \
  --database=repograph
```

---

## Re-indexing After Migration

After data migration, re-index repositories:

```bash
#!/bin/bash
# scripts/reindex-all.sh

# Get all repositories
REPOS=$(psql -U repograph -d repograph -tAc \
  "SELECT id, path FROM repositories;")

# Re-index each
while IFS='|' read -r repo_id repo_path; do
  echo "Re-indexing: $repo_path"
  python -m src.cli index "$repo_path"
  sleep 5
done <<< "$REPOS"

echo "Re-indexing complete"
```

---

## Validation

### Data Integrity Checks

```sql
-- Check orphaned edges
SELECT COUNT(*) FROM edges e
WHERE NOT EXISTS (SELECT 1 FROM nodes WHERE id = e.source_id)
   OR NOT EXISTS (SELECT 1 FROM nodes WHERE id = e.target_id);
-- Expected: 0

-- Check repository counts
SELECT tenant_id, COUNT(*) as repo_count
FROM repositories
GROUP BY tenant_id;

-- Check node/edge counts
SELECT
  r.name,
  (SELECT COUNT(*) FROM nodes WHERE repository_id = r.id) as nodes,
  (SELECT COUNT(*) FROM edges WHERE repository_id = r.id) as edges
FROM repositories r;
```

---

## Related Documentation

- [Backup & Restore Runbook](../runbooks/backup-restore.md)
- [Upgrading Guide](upgrading.md)

---

**Last Updated:** 2025-11-22
