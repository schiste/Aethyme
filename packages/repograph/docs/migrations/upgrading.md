# Upgrading Guide

Guide for upgrading RepoGraph to newer versions.

---

## Version Compatibility

| Version | Release Date | Support Until | Breaking Changes |
|---------|--------------|---------------|------------------|
| 1.2.x | 2025-11-22 | 2026-11-22 | None |
| 1.1.x | 2025-10-15 | 2026-04-15 | API endpoint changes |
| 1.0.x | 2025-09-01 | 2026-03-01 | Database schema |

---

## Upgrade Procedures

### Minor Version Upgrade (e.g., 1.1 → 1.2)

**Safe for production with minimal downtime.**

```bash
#!/bin/bash
# scripts/upgrade-minor.sh

# 1. Backup database
bash scripts/backup-database.sh

# 2. Pull latest code
git fetch
git checkout v1.2.0

# 3. Update dependencies
pip install -r requirements.txt

# 4. Run database migrations
alembic upgrade head

# 5. Restart services
docker-compose -f ops/docker-compose.yml restart

# 6. Verify upgrade
curl http://localhost:8001/health/detailed | jq .version
# Expected: "1.2.0"

# 7. Run smoke tests
bash scripts/smoke-tests.sh
```

### Major Version Upgrade (e.g., 1.x → 2.x)

**Requires maintenance window and careful planning.**

```bash
# 1. Review CHANGELOG for breaking changes
curl https://raw.githubusercontent.com/aeptus/repograph/main/CHANGELOG.md

# 2. Test in staging first
# Deploy to staging environment
# Run full test suite
# Validate all integrations

# 3. Schedule maintenance window
# Notify users 7 days in advance

# 4. During maintenance window:
bash scripts/upgrade-major.sh

# 5. Monitor for issues
# Watch logs, metrics, error rates
```

---

## Breaking Changes

### v1.2.0 → v1.3.0 (Planned)

**API Changes:**
- `/api/search/` now requires `search_type` parameter (default: `hybrid`)
- Rate limit headers changed to standard format

**Migration:**
```bash
# Update API calls
# Before:
curl -X POST /api/search/ -d '{"query":"test"}'

# After:
curl -X POST /api/search/ -d '{"query":"test","search_type":"hybrid"}'
```

**Database:**
- Added `search_metadata` column to `nodes` table
- Migration: `alembic upgrade head`

### v1.1.0 → v1.2.0

**No breaking changes** - fully backward compatible

### v1.0.0 → v1.1.0

**API Changes:**
- Renamed `/api/query` to `/api/search/`
- Auth token now required for all endpoints

**Migration:**
```bash
# Update API endpoint
sed -i 's|/api/query|/api/search/|g' client-code.py

# Add authentication
export REPOGRAPH_TOKEN="your-token"
```

---

## Rolling Upgrade (Zero Downtime)

For Kubernetes deployments:

```bash
#!/bin/bash
# scripts/rolling-upgrade.sh

# 1. Deploy new version alongside old (blue-green)
kubectl apply -f k8s/deployment-v1.2.yaml

# 2. Wait for new pods to be ready
kubectl wait --for=condition=ready pod -l version=v1.2.0

# 3. Gradually shift traffic (10% increments)
for weight in 10 20 30 40 50 60 70 80 90 100; do
  kubectl patch virtualservice repograph-api --type merge -p "
  spec:
    http:
    - route:
      - destination:
          host: repograph-api
          subset: v1.2.0
        weight: $weight
      - destination:
          host: repograph-api
          subset: v1.1.0
        weight: $((100-weight))
  "
  echo "Traffic: $weight% on v1.2.0"
  sleep 60  # Monitor for 1 minute
done

# 4. Remove old version
kubectl delete deployment repograph-api-v1.1.0
```

---

## Database Migration Strategy

### Backward-Compatible Migrations

For zero-downtime upgrades:

```python
# alembic/versions/xxx_add_new_column.py

def upgrade():
    # Add column with default value (safe)
    op.add_column('nodes',
        sa.Column('search_metadata', sa.JSON(), nullable=True, server_default='{}'))

def downgrade():
    op.drop_column('nodes', 'search_metadata')
```

### Multi-Step Migrations

For breaking schema changes:

**Step 1: Add new column (deploy v1.2.0)**
```python
op.add_column('nodes', sa.Column('new_symbol', sa.String()))
```

**Step 2: Backfill data (background job)**
```sql
UPDATE nodes SET new_symbol = symbol WHERE new_symbol IS NULL;
```

**Step 3: Make column required (deploy v1.3.0)**
```python
op.alter_column('nodes', 'new_symbol', nullable=False)
```

**Step 4: Drop old column (deploy v1.4.0)**
```python
op.drop_column('nodes', 'symbol')
```

---

## Configuration Changes

### Environment Variables

Check `.env.example` for new variables:

```bash
# v1.2.0 added:
INDEXING_CONCURRENCY=4
WATCH_BATCH_INTERVAL=300

# v1.1.0 added:
REDIS_CACHE_TTL=300
```

### Database Settings

```bash
# v1.2.0 recommended:
DB_POOL_MAX_SIZE=50  # Increased from 20
```

---

## Post-Upgrade Verification

### Health Checks

```bash
# 1. API health
curl http://localhost:8001/health/detailed

# 2. Database migrations
alembic current
# Should show latest revision

# 3. Search functionality
curl -X POST http://localhost:8001/api/search/ \
  -H "Authorization: Bearer $TOKEN" \
  -d '{"query":"test"}' | jq .

# 4. Indexing
python -m src.cli index /path/to/small/repo --name test

# 5. Metrics
curl http://localhost:9090/metrics | grep repograph_version
```

### Smoke Tests

```bash
bash scripts/smoke-tests.sh
```

---

## Rollback Procedures

If upgrade fails, see [Rollback Runbook](../runbooks/rollback.md).

Quick rollback:

```bash
# Kubernetes
kubectl rollout undo deployment/repograph-api

# Docker Compose
docker-compose -f ops/docker-compose.yml down
sed -i 's/v1.2.0/v1.1.0/g' ops/docker-compose.yml
docker-compose -f ops/docker-compose.yml up -d
```

---

## Support

For upgrade issues:
- Slack: #repograph-support
- Email: support@repograph.com
- Docs: https://docs.repograph.dev/upgrades

---

**Last Updated:** 2025-11-22
