# Auth & RLS Migration Guide

This guide helps you migrate existing Aethyme data to the new authentication and RLS-hardened system.

## Overview

The Auth & RLS Hardening implementation (Sprint 1, Task S1-T1) adds:

- Multi-tenant Row-Level Security (RLS) policies
- OIDC + JWT authentication with scoped claims
- API key management with rotation
- Redis-backed rate limiting
- Enhanced security and tenant isolation

## Pre-Migration Checklist

Before running the migration:

- [ ] Backup your database
- [ ] Test migration on a staging environment
- [ ] Review and understand the new security model
- [ ] Prepare OIDC configuration (if using enterprise SSO)
- [ ] Plan API key distribution for existing integrations

## Step 1: Backup Database

```bash
# PostgreSQL backup
pg_dump -h localhost -U aethyme -d aethyme > aethyme_backup_$(date +%Y%m%d).sql

# Or using Docker
docker exec aethyme-db pg_dump -U aethyme aethyme > aethyme_backup_$(date +%Y%m%d).sql
```

## Step 2: Install Dependencies

```bash
# Navigate to aethyme package
cd packages/aethyme

# Install new Python dependencies
pip install httpx python-jose[cryptography] redis

# Or with poetry
poetry add httpx python-jose[cryptography] redis
```

## Step 3: Update Environment Variables

Add new configuration to your `.env` file:

```bash
# JWT Configuration
JWT_SECRET_KEY=<generate-with: python -c "import secrets; print(secrets.token_urlsafe(32))">
JWT_ALGORITHM=HS256
JWT_EXPIRATION_DELTA=86400

# OIDC (Optional - for enterprise SSO)
# OIDC_ISSUER_URL=https://auth.example.com/realms/aethyme
# OIDC_CLIENT_ID=aethyme-client
# OIDC_CLIENT_SECRET=your-client-secret
# OIDC_REDIRECT_URI=https://aethyme.example.com/auth/callback

# Rate Limiting
RATE_LIMIT_DEFAULT=100
RATE_LIMIT_ENABLED=true
REDIS_URL=redis://localhost:6379/0

# Database (ensure these are set)
DATABASE_URL=postgresql://aethyme:password@localhost:5432/aethyme
```

## Step 4: Run Database Migration

```bash
# Run the RLS hardening migration
psql -h localhost -U aethyme -d aethyme -f migrations/002_add_rls_hardening.sql

# Or using Docker
docker exec -i aethyme-db psql -U aethyme aethyme < migrations/002_add_rls_hardening.sql
```

The migration will:
- Add `scopes` and `repo_id` columns to `api_keys` table
- Create `users`, `refresh_tokens`, and `audit_logs` tables
- Add RLS helper functions (`current_tenant_id()`, `has_scope()`)
- Apply RLS policies to all tables
- Enable RLS on all tables with `FORCE ROW LEVEL SECURITY`

## Step 5: Verify Migration

```sql
-- Check that RLS is enabled
SELECT
    schemaname,
    tablename,
    rowsecurity
FROM pg_tables t
JOIN pg_class c ON c.relname = t.tablename
WHERE schemaname = 'aethyme'
AND tablename IN (
    'tenants', 'repositories', 'nodes', 'edges',
    'indexing_jobs', 'api_keys', 'users'
);
-- All should show rowsecurity = true

-- Check new tables exist
\dt aethyme.users
\dt aethyme.refresh_tokens
\dt aethyme.audit_logs

-- Check new columns
\d aethyme.api_keys
-- Should show 'scopes' and 'repo_id' columns
```

## Step 6: Migrate Existing API Keys (If Any)

If you have existing API keys with the old `permissions` column:

```sql
-- The migration script handles this automatically
-- But you can verify:
SELECT
    id,
    name,
    scopes,
    created_at
FROM aethyme.api_keys;

-- Update scopes if needed
UPDATE aethyme.api_keys
SET scopes = '["repo:read", "repo:write"]'::jsonb
WHERE scopes = '["read"]'::jsonb;
```

## Step 7: Create Initial Admin User

Create an admin API key for the migration period:

```python
from src.auth.api_keys import APIKeyManager
from src.graph.connection_pool import db_pool

# Get default tenant ID
query = "SELECT id FROM aethyme.tenants LIMIT 1"
result = db_pool.execute(query)
tenant_id = str(result[0]['id'])

# Create admin API key
admin_key = APIKeyManager.create(
    tenant_id=tenant_id,
    name="Migration Admin Key",
    scopes=["org:admin"],
    expires_in_days=30  # Temporary key
)

print(f"Admin API Key: {admin_key['api_key']}")
print(f"Save this securely! It will only be shown once.")
```

## Step 8: Update Application Code

### 8.1 Update FastAPI Application

Update your `main.py` to include the new middleware:

```python
from fastapi import FastAPI
from src.middleware.rate_limit import RateLimitMiddleware, rate_limiter
from src.auth.middleware import get_current_user, require_scope

app = FastAPI()

# Add rate limiting middleware
@app.on_event("startup")
async def startup():
    await rate_limiter.connect()

@app.on_event("shutdown")
async def shutdown():
    await rate_limiter.disconnect()

app.add_middleware(RateLimitMiddleware, limiter=rate_limiter)

# Update routes to require authentication
from src.auth.middleware import UserContext
from fastapi import Depends

@app.post("/api/index")
async def index_repository(
    user: UserContext = Depends(require_scope("repo:write"))
):
    # User context is automatically available
    # Tenant context is automatically set for database queries
    pass
```

### 8.2 Update Existing Routes

Add authentication to your routes:

```python
# Before
@router.get("/api/search/")
async def search(query: str):
    results = db_pool.execute("SELECT * FROM nodes WHERE symbol ILIKE %s", (f"%{query}%",))
    return results

# After
from src.auth.middleware import UserContext, require_scope
from fastapi import Depends

@router.get("/api/search/")
async def search(
    query: str,
    user: UserContext = Depends(require_scope("repo:read"))
):
    # set_tenant_context is called automatically by require_scope
    # Database queries are now automatically scoped to user's tenant
    results = db_pool.execute("SELECT * FROM nodes WHERE symbol ILIKE %s", (f"%{query}%",))
    return results
```

### 8.3 Update Database Query Functions

Ensure tenant context is set before queries:

```python
from src.auth.middleware import set_tenant_context, UserContext

async def query_with_context(user: UserContext):
    # Set tenant context
    await set_tenant_context(user)

    # Now all queries respect RLS policies
    results = db_pool.execute("SELECT * FROM repositories")
    # Only returns repositories for user's tenant

    return results
```

## Step 9: Test Tenant Isolation

Run the comprehensive test suite:

```bash
# Test tenant isolation
pytest tests/auth/test_isolation.py -v

# Test RLS policies
pytest tests/auth/test_rls.py -v

# Expected output:
# ✓ test_tenant_cannot_read_other_tenant_repos
# ✓ test_tenant_cannot_write_to_other_tenant_repos
# ✓ test_nodes_tenant_isolation
# ✓ test_read_only_token_cannot_write
# ✓ test_admin_scope_grants_all_permissions
# ✓ test_api_key_creation_and_verification
# ✓ test_rate_limit_basic
# ... and more
```

## Step 10: Migrate Existing Integrations

### CI/CD Pipelines

Replace old authentication with API keys:

```yaml
# Before (.github/workflows/index.yml)
- name: Index repository
  run: |
    curl -X POST http://localhost:8001/api/index \
      -H "Content-Type: application/json" \
      -d '{"repo_path": "."}'

# After
- name: Index repository
  env:
    AETHYME_API_KEY: ${{ secrets.AETHYME_API_KEY }}
  run: |
    curl -X POST http://localhost:8001/api/index \
      -H "Authorization: Bearer $AETHYME_API_KEY" \
      -H "Content-Type: application/json" \
      -d '{"repo_path": "."}'
```

Create API keys for each integration:

```bash
# Create CI/CD API key
python -c "
from src.auth.api_keys import APIKeyManager
key = APIKeyManager.create(
    tenant_id='your-tenant-id',
    name='GitHub Actions CI',
    scopes=['repo:read', 'repo:write']
)
print(key['api_key'])
"
```

### Scripts and Tools

Update scripts to use API keys:

```python
# Before
import requests
response = requests.get("http://localhost:8001/api/search/", params={"query": "test"})

# After
import requests
import os

API_KEY = os.environ["AETHYME_API_KEY"]
headers = {"Authorization": f"Bearer {API_KEY}"}
response = requests.get(
    "http://localhost:8001/api/search/",
    params={"query": "test"},
    headers=headers
)
```

## Step 11: Enable OIDC (Optional)

If using enterprise SSO:

1. Configure your OIDC provider (see auth-setup.md)
2. Set environment variables
3. Test OIDC flow:

```python
from src.auth.oidc import oidc_client

# Verify configuration
assert oidc_client.is_configured

# Test discovery
config = await oidc_client.discover_configuration()
print(f"OIDC configured: {config['issuer']}")
```

## Step 12: Monitor and Audit

### Check Audit Logs

```sql
-- View recent authentication events
SELECT
    action,
    user_id,
    resource_type,
    created_at
FROM aethyme.audit_logs
ORDER BY created_at DESC
LIMIT 20;
```

### Monitor Rate Limits

```bash
# Check Redis for rate limit data
redis-cli keys "ratelimit:*"

# Monitor rate limit hits
redis-cli monitor | grep ratelimit
```

### Verify RLS Policies

```sql
-- Test with different tenant contexts
SET app.current_tenant = 'tenant-a-uuid';
SET app.current_scopes = '["repo:read"]';

SELECT COUNT(*) FROM aethyme.repositories;
-- Should only count tenant A's repos

SET app.current_tenant = 'tenant-b-uuid';
SELECT COUNT(*) FROM aethyme.repositories;
-- Should only count tenant B's repos
```

## Rollback Plan

If issues occur, you can rollback:

### Database Rollback

```bash
# Restore from backup
psql -h localhost -U aethyme -d aethyme < aethyme_backup_YYYYMMDD.sql

# Or drop new objects
psql -h localhost -U aethyme -d aethyme <<EOF
-- Drop new tables
DROP TABLE IF EXISTS aethyme.audit_logs CASCADE;
DROP TABLE IF EXISTS aethyme.refresh_tokens CASCADE;
DROP TABLE IF EXISTS aethyme.users CASCADE;

-- Drop new columns
ALTER TABLE aethyme.api_keys DROP COLUMN IF EXISTS scopes;
ALTER TABLE aethyme.api_keys DROP COLUMN IF EXISTS repo_id;

-- Disable RLS
ALTER TABLE aethyme.tenants DISABLE ROW LEVEL SECURITY;
ALTER TABLE aethyme.repositories DISABLE ROW LEVEL SECURITY;
ALTER TABLE aethyme.nodes DISABLE ROW LEVEL SECURITY;
ALTER TABLE aethyme.edges DISABLE ROW LEVEL SECURITY;
ALTER TABLE aethyme.indexing_jobs DISABLE ROW LEVEL SECURITY;
ALTER TABLE aethyme.api_keys DISABLE ROW LEVEL SECURITY;
EOF
```

### Application Rollback

```bash
# Revert to previous version
git checkout <previous-commit>

# Reinstall dependencies
pip install -r requirements.txt

# Restart application
systemctl restart aethyme
```

## Common Issues

### Issue: "Permission denied" errors after migration

**Cause:** Tenant context not set in session

**Fix:**
```python
# Ensure set_tenant_context is called
from src.auth.middleware import set_tenant_context
await set_tenant_context(user)
```

### Issue: Queries return no results

**Cause:** `app.current_tenant` not set

**Fix:**
```sql
-- Check current tenant
SHOW app.current_tenant;

-- Set it if needed
SET app.current_tenant = 'your-tenant-uuid';
```

### Issue: Rate limiting too strict

**Cause:** Default limits too low

**Fix:**
```python
# Adjust limits in code
from src.middleware.rate_limit import RateLimiter

limiter = RateLimiter(default_limit=500, default_window=60)

# Or in environment
RATE_LIMIT_DEFAULT=500
```

## Post-Migration Checklist

- [ ] All tests pass (`pytest tests/auth/`)
- [ ] Tenant isolation verified
- [ ] API keys created for all integrations
- [ ] CI/CD pipelines updated and tested
- [ ] Rate limiting configured appropriately
- [ ] OIDC configured (if applicable)
- [ ] Audit logging enabled
- [ ] Monitoring set up
- [ ] Documentation updated
- [ ] Team trained on new auth model
- [ ] Old temporary admin keys revoked

## Support

If you encounter issues during migration:

1. Check logs: `tail -f /var/log/aethyme/app.log`
2. Run diagnostics: `pytest tests/auth/ -v`
3. Review audit logs: `SELECT * FROM aethyme.audit_logs`
4. Contact: support@aethyme.com

---

**Migration Version:** 002 (RLS Hardening)
**Date:** 2025-11-22
