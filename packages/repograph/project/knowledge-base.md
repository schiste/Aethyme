# RepoGraph Knowledge Base (FAQ)

**Purpose:** Quick answers to common questions
**Audience:** All developers
**Last Updated:** 2025-11-22

---

## Getting Started

### How do I run the development environment?
```bash
make dev
```
This starts:
- Backend API (port 8000)
- PostgreSQL (port 5432)
- Redis (port 6379)
- Adminer (port 8080)

**Troubleshooting:**
- Port conflict: Check `docker ps` and stop conflicting containers
- Database errors: Run `docker-compose down -v` to reset volumes
- Dependencies: Run `pnpm install` again

---

### How do I run tests?
```bash
# All tests
pnpm test

# Specific test file
pytest backend/tests/test_auth.py

# Frontend tests
pnpm test --filter customer

# With coverage
pnpm test:coverage
```

---

### How do I run quality checks?
```bash
# All quality checks (linting, types, tests)
pnpm quality:all

# Just linting
pnpm lint

# Just type checking
pnpm typecheck

# Auto-fix linting issues
pnpm lint:fix
```

---

## Development

### How do I search the codebase?
```bash
# Use pnpm discover (fastest)
pnpm discover <search-term>

# Examples
pnpm discover authentication
pnpm discover multi-tenant
pnpm discover suppliers

# Or use grep
grep -r "search-term" .

# Or use skills search
grep -r "search-term" Agents/skills/
```

---

### How do I add a new API endpoint?

**Step-by-step:**
1. **Load skill:** Read `Agents/skills/api-conventions/skill.md` (when available)
2. **Add model** with `org_id` for multi-tenant:
   ```python
   # backend/myapp/models.py
   class MyResource(Base):
       __tablename__ = "my_resources"
       id = Column(UUID, primary_key=True)
       org_id = Column(UUID, nullable=False, index=True)
       name = Column(String, nullable=False)
   ```

3. **Create migration** with RLS policy:
   ```bash
   alembic revision -m "add_my_resource_table"
   ```
   ```sql
   -- Enable RLS
   ALTER TABLE my_resources ENABLE ROW LEVEL SECURITY;

   -- Isolation policy
   CREATE POLICY my_resources_org_isolation ON my_resources
       FOR ALL
       USING (org_id = current_setting('app.current_org_id')::uuid);
   ```

4. **Add serializer:**
   ```python
   # backend/myapp/serializers.py
   class MyResourceSerializer(serializers.ModelSerializer):
       class Meta:
           model = MyResource
           fields = ['id', 'org_id', 'name']
   ```

5. **Add ViewSet with permission:**
   ```python
   # backend/myapp/api.py
   from rest_framework import viewsets
   from rest_framework.permissions import IsAuthenticated

   class MyResourceViewSet(viewsets.ModelViewSet):
       queryset = MyResource.objects.all()
       serializer_class = MyResourceSerializer
       permission_classes = [IsAuthenticated]
   ```

6. **Write tests:**
   ```python
   # backend/tests/test_my_resource.py
   def test_create_my_resource(client, org_user):
       response = client.post('/api/v1/my-resources/', {
           'name': 'Test Resource'
       })
       assert response.status_code == 201
   ```

7. **Update OpenAPI schema:**
   ```bash
   pnpm contracts:gen
   ```

---

### How do I debug authentication issues?

**Common Issues:**

**1. "401 Unauthorized"**
```bash
# Check JWT token in request
curl -H "Authorization: Bearer <token>" http://localhost:8000/api/v1/endpoint

# Decode token to verify claims
# Use jwt.io or:
python -c "import jwt; print(jwt.decode('token', options={'verify_signature': False}))"
```

**Check:**
- Token not expired
- Token has correct `org_id` claim
- Token has required scopes

**2. "403 Forbidden"**
```bash
# Check RLS policy
SELECT * FROM my_resources WHERE org_id = 'expected-org-id';

# Verify current_org_id setting
SELECT current_setting('app.current_org_id');
```

**Check:**
- RLS policy allows access
- User's org_id matches resource org_id
- Permission exists in rbac

**3. API Key Not Working**
```bash
# Check API key validity
curl -H "X-API-Key: rg_..." http://localhost:8000/api/v1/endpoint
```

**Check:**
- API key not revoked
- API key not expired
- API key has required scopes
- Org ID matches

---

### How do I add data-ui selectors (Frontend)?

**Rule:** Every interactive element MUST have a `data-ui` selector.

```typescript
// ✅ Good
<button data-ui="submit-supplier-form" onClick={handleSubmit}>
  Submit
</button>

<input data-ui="supplier-name-input" type="text" />

<div data-ui="supplier-list">
  {suppliers.map(s => (
    <div key={s.id} data-ui={`supplier-row-${s.id}`}>
      {s.name}
    </div>
  ))}
</div>

// ❌ Bad (no data-ui)
<button onClick={handleSubmit}>Submit</button>
<input type="text" />
```

**Convention:**
- Format: `{action}-{entity}-{type}`
- Examples:
  - `submit-supplier-form`
  - `delete-supplier-button`
  - `supplier-name-input`
  - `supplier-list`

**Testing:**
```typescript
// Can select elements in tests
const submitButton = screen.getByTestId('submit-supplier-form');
```

---

### How do I register a new route (Frontend)?

**Use config-driven routing** (no manual registration):

```typescript
// apps/customer/src/config/menu.config.ts

export const menuConfig = {
  sections: [
    {
      title: 'My Section',
      items: [
        {
          label: 'My New Page',
          path: '/my-new-page',
          component: lazy(() => import('../pages/MyNewPage')),
          permission: 'view_my_resource',  // From rbac
          icon: 'IconName',
          datauiId: 'my-new-page-menu-item'
        }
      ]
    }
  ]
};
```

**That's it!** Route automatically registered, menu item added, permission checked.

---

## Operations

### How do I run migrations?

**Local:**
```bash
# Run all pending migrations
alembic upgrade head

# Rollback last migration
alembic downgrade -1

# Check current version
alembic current

# View migration history
alembic history
```

**Production:**
```bash
# Via Kubernetes job
kubectl apply -f deploy/k8s/migration-job.yaml

# Check job status
kubectl logs job/migration
```

---

### How do I deploy to staging/production?

**Staging:**
```bash
# Triggered automatically on merge to main
git checkout main
git pull
git push origin main

# CI/CD runs:
# 1. Tests
# 2. Build image
# 3. Push to registry
# 4. Deploy to staging
```

**Production:**
```bash
# Manual approval required

# 1. Create release tag
git tag -a v1.2.3 -m "Release v1.2.3"
git push origin v1.2.3

# 2. Approve deployment in CI/CD
# (GitHub Actions workflow requires approval)

# 3. Monitor rollout
kubectl rollout status deployment/repograph-api -n production
```

**Rollback:**
```bash
# Helm rollback
helm rollback repograph -n production

# Or kubectl rollback
kubectl rollout undo deployment/repograph-api -n production
```

---

### Where are the logs?

**Local Development:**
```bash
# All services
docker-compose logs -f

# API only
docker-compose logs -f api

# PostgreSQL
docker-compose logs -f postgres
```

**Staging/Production:**
```bash
# Kubernetes logs
kubectl logs -f deployment/repograph-api -n staging

# Tail last 100 lines
kubectl logs --tail=100 deployment/repograph-api -n staging

# All pods
kubectl logs -l app=repograph-api -n staging
```

**Structured Log Search:**
```bash
# Search for errors
cat logs/api.log | jq '. | select(.level == "ERROR")'

# Search by correlation ID
cat logs/api.log | jq '. | select(.correlation_id == "abc123")'

# Search by org
cat logs/api.log | jq '. | select(.org_id == "acme")'
```

---

### How do I check system health?

**Health Endpoints:**
```bash
# Liveness (is service running?)
curl http://localhost:8000/health/live

# Readiness (can service accept traffic?)
curl http://localhost:8000/health/ready

# Metrics
curl http://localhost:8000/metrics
```

**Database:**
```bash
# Check connection
psql -h localhost -U repograph -d repograph -c "SELECT 1;"

# Check active connections
psql -h localhost -U repograph -d repograph -c "SELECT count(*) FROM pg_stat_activity;"
```

**Redis:**
```bash
# Check connection
redis-cli ping

# Check keys
redis-cli KEYS "*"

# Check memory usage
redis-cli INFO memory
```

---

## Troubleshooting

### Tests are failing

**1. Database-related failures:**
```bash
# Reset test database
docker-compose down -v
docker-compose up -d
alembic upgrade head

# Run tests again
pnpm test
```

**2. Import errors:**
```bash
# Clear Python cache
find . -type d -name __pycache__ -exec rm -rf {} +
find . -type f -name "*.pyc" -delete

# Reinstall dependencies
pip install -r requirements.txt
```

**3. Frontend test failures:**
```bash
# Clear node_modules
rm -rf node_modules
pnpm install

# Clear test cache
pnpm test --clearCache
```

---

### API returns 500 error

**1. Check logs:**
```bash
docker-compose logs -f api
```

**2. Check database connection:**
```bash
# In logs, look for:
# "could not connect to database" → Database down
# "relation does not exist" → Missing migration
```

**3. Check migrations:**
```bash
alembic current  # Check current version
alembic upgrade head  # Apply missing migrations
```

---

### Rate limit exceeded (429)

**1. Check rate limit headers:**
```bash
curl -i http://localhost:8000/api/v1/endpoint
# Look for:
# X-RateLimit-Limit: 100
# X-RateLimit-Remaining: 0
# Retry-After: 42
```

**2. Clear rate limit (local only):**
```bash
# Clear Redis cache
redis-cli FLUSHALL

# Or clear specific key
redis-cli DEL "rate:org:acme:12345"
```

**3. Increase limits (dev only):**
```bash
# In .env
RATE_LIMIT_DEFAULT=10000  # Increase from 1000
```

---

### Deployment is failing

**1. Check CI/CD logs:**
```bash
# GitHub Actions
# Go to: https://github.com/org/repo/actions

# View logs for failed step
```

**2. Check image build:**
```bash
# Build locally
docker build -t repograph-api:test .

# Run locally
docker run -p 8000:8000 repograph-api:test
```

**3. Check Kubernetes events:**
```bash
kubectl get events -n staging --sort-by='.lastTimestamp'

# Check pod status
kubectl describe pod <pod-name> -n staging
```

**Common Issues:**
- **ImagePullBackOff:** Image not pushed to registry
- **CrashLoopBackOff:** Service crashing on start (check logs)
- **Pending:** Insufficient resources

---

## Performance

### How do I optimize a slow query?

**1. Identify slow query:**
```bash
# Check logs for slow queries
cat logs/api.log | jq '. | select(.duration_ms > 2000)'

# Or enable PostgreSQL slow query log
# In postgresql.conf:
# log_min_duration_statement = 1000
```

**2. Analyze query:**
```sql
EXPLAIN ANALYZE
SELECT * FROM my_table WHERE condition;
```

**3. Add indexes:**
```sql
-- Create index on frequently queried columns
CREATE INDEX idx_my_table_org_id ON my_table(org_id);
CREATE INDEX idx_my_table_created_at ON my_table(created_at);
```

**4. Add caching:**
```python
from backend.cache import cache_manager

@cached(ttl=300, key_func=lambda org_id: f"cache:org:{org_id}:data")
async def get_data(org_id: str):
    # Expensive query
    return await db.query(...)
```

---

### How do I reduce API response time?

**Strategies:**

**1. Caching (biggest impact):**
```python
# Cache hot queries
@cached(ttl=600)
async def get_index_status(org_id, repo):
    return await db.query(...)
```

**2. Pagination:**
```python
# Don't return all results
class MyViewSet(viewsets.ModelViewSet):
    pagination_class = PageNumberPagination
    page_size = 50
```

**3. Select only needed fields:**
```python
# Don't use .all()
queryset = MyModel.objects.values('id', 'name')  # Not all fields
```

**4. Async operations:**
```python
# Run independent operations in parallel
results = await asyncio.gather(
    get_index_status(),
    get_query_results(),
    get_scorecard()
)
```

---

## Skills System

### How do I find a specific skill?

**1. Browse directory:**
```bash
ls Agents/skills/
```

**2. Search by name:**
```bash
find Agents/skills/ -name "skill.md" | grep <skill-name>
```

**3. Search by tag:**
```bash
grep -r "tags: \[.*caching.*\]" Agents/skills/
```

**4. Read skill:**
```bash
cat Agents/skills/caching/skill.md
```

---

### How do I create a new skill?

**1. Use template:**
```bash
cp Agents/skills/_meta/skill.md Agents/skills/my-skill/skill.md
```

**2. Fill in frontmatter:**
```yaml
---
name: my-skill
description: "What this skill teaches"
domain: backend|frontend|ops|ai|docs|tool
tags: [tag1, tag2]
owner: "@team-name"
status: stable|incubating|deprecated
onboarding_priority: 1-10
code_paths: ["backend/myapp"]
docs_paths: ["docs/myapp"]
last_updated: 2025-11-22
---
```

**3. Write content:**
- Overview
- Quick Operations
- Implementation
- Best Practices
- Troubleshooting
- Related Skills
- Examples

**4. Add to skills index:**
```yaml
# Agents/skills/index.yml
- name: my-skill
  path: my-skill/skill.md
  priority: 5
```

---

### How do I update an existing skill?

**1. Edit skill document:**
```bash
vim Agents/skills/my-skill/skill.md
```

**2. Update `last_updated`:**
```yaml
last_updated: 2025-11-22
```

**3. Add to learn-log (optional):**
```bash
python Agents/skills/_meta/scripts/log_learning.py my-skill \
  --context "Updated based on production usage" \
  --outcome "Improved examples section"
```

**4. Commit changes:**
```bash
git add Agents/skills/my-skill/
git commit -m "docs: update my-skill with new examples"
```

---

## Getting Help

### Who do I ask for help?

**By Topic:**
- **Architecture:** @platform-team
- **Backend API:** @backend-team
- **Frontend:** @frontend-team
- **DevOps/Infrastructure:** @devops-team
- **AI/LLM:** @ai-team
- **Skills/Training:** Skills & Training Lead

**By Urgency:**
- **Urgent (production down):** Post in #incidents, page on-call
- **Blocking:** Post in #repograph-help
- **Question:** Post in #repograph-dev
- **Non-urgent:** Comment on relevant GitHub issue

### Where do I report bugs?

**1. Check if bug already reported:**
```bash
# Search GitHub issues
https://github.com/org/repograph/issues?q=is%3Aissue+<search-term>
```

**2. Create new issue:**
- Use bug report template
- Include:
  - Steps to reproduce
  - Expected behavior
  - Actual behavior
  - Logs/screenshots
  - Environment (local/staging/prod)

**3. Label appropriately:**
- `bug` - Something broken
- `critical` - Production issue
- `p1/p2/p3` - Priority

---

## Quick Reference

### Essential Commands
```bash
# Development
make dev                 # Start dev environment
pnpm test               # Run tests
pnpm quality:all        # Run all quality checks
pnpm discover <term>    # Search codebase

# Database
alembic upgrade head    # Run migrations
alembic downgrade -1    # Rollback migration
alembic current         # Check current version

# Deployment
kubectl get pods -n staging              # Check pods
kubectl logs -f deployment/repograph-api # View logs
helm upgrade repograph ./deploy/helm/   # Deploy
helm rollback repograph                 # Rollback
```

### Essential Files
```
packages/repograph/ROADMAP.md          - Full roadmap
Agents/skills/                         - All skills
docs/architecture/                     - Architecture docs
.env.example                          - Environment template
docker-compose.yml                    - Local services
```

### Essential URLs
```
Local API:       http://localhost:8000
API Docs:        http://localhost:8000/docs
Adminer (DB):    http://localhost:8080
Staging API:     https://api.staging.repograph.com
Production API:  https://api.repograph.com
```

---

**Document Owner:** Skills & Training Lead
**Review Cadence:** Monthly
**Contributions:** Submit PR to update FAQ
**Last Updated:** 2025-11-22
