# Aethyme Cloud - Repository Extraction Checklist

This document ensures Aethyme Cloud can be extracted to a separate repository at any time with minimal effort.

---

## ✅ Extraction-Ready Criteria

### Independence Checks

- [ ] **No imports from parent** - No code imports from `../../..` outside aethyme-cloud/
- [ ] **Own dependencies** - All dependencies listed in package.json
- [ ] **Own configuration** - .env.example has all required config
- [ ] **Own infrastructure** - docker-compose.yml works standalone
- [ ] **Own CI/CD** - .github/workflows/ has all necessary workflows
- [ ] **Own database** - Uses separate database instance
- [ ] **Own ports** - No port conflicts with parent-repo services
- [ ] **Own secrets** - No shared environment variables

### Functional Validation

```bash
# Run these commands to validate independence

# 1. Copy to temporary directory
cp -r packages/aethyme-cloud /tmp/aethyme-cloud-test
cd /tmp/aethyme-cloud-test

# 2. Install dependencies
pnpm install

# 3. Start infrastructure
docker-compose up -d

# 4. Run tests
pnpm test

# 5. Build
pnpm build

# 6. Verify no parent imports
! grep -r "from '\.\./\.\./\.\./\.\." apps/
! grep -r "from '\.\./\.\./\.\./\.\." packages/

# All should pass!
```

---

## 📋 Pre-Extraction Validation

Run before extracting to separate repository:

### Code Quality
- [ ] TypeScript compilation passes: `pnpm typecheck`
- [ ] All tests pass: `pnpm test`
- [ ] Linting passes: `pnpm lint`
- [ ] Build succeeds: `pnpm build`
- [ ] No security vulnerabilities: `pnpm audit`

### Infrastructure
- [ ] Docker Compose starts: `docker-compose up -d`
- [ ] All services healthy
- [ ] Database migrations run: `pnpm db:migrate`
- [ ] API accessible: `curl http://localhost:8000/health`
- [ ] Web accessible: `curl http://localhost:3000`

### Documentation
- [ ] README is complete
- [ ] API documentation exists
- [ ] Architecture docs updated
- [ ] Environment variables documented

---

## 🚀 Extraction Steps

When ready to extract to separate repository:

### Step 1: Create Extraction Branch

```bash
cd /path/to/parent-repo

# Create branch for extraction
git checkout -b extract-aethyme-cloud

# Verify everything works
cd packages/aethyme-cloud
pnpm install
pnpm test
pnpm build
```

### Step 2: Extract with Git History

```bash
cd /path/to/parent-repo

# Extract subdirectory with full history
git subtree split -P packages/aethyme-cloud -b aethyme-cloud-extraction

# This creates a new branch with only aethyme-cloud/ history
```

### Step 3: Create New Repository

```bash
# Create new directory
cd 
mkdir aethyme-cloud
cd aethyme-cloud

# Initialize with extracted history
git init
git pull ../parent-repo aethyme-cloud-extraction

# Verify structure
ls -la
# Should see: apps/ packages/ infrastructure/ etc. (no packages/aethyme-cloud/)

# Create GitHub repository
gh repo create aethyme-cloud --private --source=. --remote=origin

# Push
git push -u origin main
```

### Step 4: Configure CI/CD

```bash
# Add GitHub secrets (in new repo settings)
# - DATABASE_URL
# - REDIS_URL
# - STRIPE_SECRET_KEY
# - GITHUB_CLIENT_SECRET
# - etc.

# Trigger first build
git tag v0.1.0
git push --tags
```

### Step 5: Deploy to Production

```bash
# Deploy via CI/CD or manually
./scripts/deploy.sh production

# Verify deployment
curl https://api.example.com/health
```

### Step 6: Cleanup Parent Repository

```bash
cd /path/to/parent-repo

# Remove aethyme-cloud (or keep as archive)
git checkout main
git rm -rf packages/aethyme-cloud
git commit -m "Extract Aethyme Cloud to separate repository

See: https://github.com/yourorg/aethyme-cloud"

git push
```

---

## 📊 Extraction Effort Estimate

**If following this checklist:** 2-4 hours

**Breakdown:**
- Validation: 30 minutes
- Extraction: 15 minutes
- New repo setup: 30 minutes
- CI/CD configuration: 1-2 hours
- Deployment: 30 minutes
- Testing: 30 minutes

---

## ⚠️ Common Pitfalls to Avoid

### ❌ Don't Do This:

```typescript
// BAD: Importing from parent monorepo
import { config } from '../../../../config/config/app.config.json'
import { getUserProfile } from '../../../../backend/accounts/models'

// BAD: Shared database
const db = new Database('postgresql://localhost/aeptus_grc')

// BAD: Hardcoded paths
const configPath = '/path/to/parent-repo/backend/config.json'
```

### ✅ Do This Instead:

```typescript
// GOOD: Local imports only
import { config } from '../config'
import { getUserProfile } from '@aethyme-cloud/database'

// GOOD: Own database
const db = new Database(process.env.DATABASE_URL)

// GOOD: Relative paths
const configPath = path.join(__dirname, '../config.json')
```

---

## 🔍 Continuous Validation

Add to CI/CD to prevent coupling:

```yaml
# .github/workflows/isolation-test.yml
name: Isolation Test

on:
  pull_request:
    paths:
      - 'packages/aethyme-cloud/**'

jobs:
  test-extraction-readiness:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3

      - name: Copy to temp directory
        run: |
          mkdir -p /tmp/aethyme-test
          cp -r packages/aethyme-cloud /tmp/aethyme-test/

      - name: Test standalone install
        working-directory: /tmp/aethyme-test/aethyme-cloud
        run: |
          pnpm install
          pnpm typecheck
          pnpm build

      - name: Check for parent imports
        run: |
          cd /tmp/aethyme-test/aethyme-cloud
          ! grep -r "from '\.\./\.\./\.\./\.\." apps/ packages/
```

---

## 📝 Post-Extraction

After extraction is complete:

- [ ] Update parent README to link to new repo
- [ ] Update documentation references
- [ ] Notify team of new repository
- [ ] Archive old directory (or remove)
- [ ] Update CI/CD pipelines
- [ ] Transfer GitHub issues (if any)

---

## 🎯 Success Criteria

Extraction is successful when:

1. ✅ New repository builds and deploys independently
2. ✅ All tests pass in new repository
3. ✅ Production deployment works
4. ✅ No dependencies on parent repository
5. ✅ Team can work in new repository
6. ✅ Zero downtime during migration

---

**Last Validated:** [Date]
**Next Validation:** [Every PR that touches aethyme-cloud/]

---

## ✅ Validation Results (Week 2 - 2025-10-02)

**Independence Checks:**
- ✅ No imports from parent (verified via grep)
- ✅ Own dependencies (package.json + requirements.txt)
- ✅ Own configuration (.env.example with correct ports: 5434, 6381, 9202)
- ✅ Own infrastructure (docker-compose.yml standalone)
- ✅ Own database (separate instance on port 5434)
- ✅ No port conflicts with parent-repo services
- ✅ Own secrets (no shared environment variables)
- ⚠️ CI/CD workflows (not yet implemented, planned for Week 3)

**Functional Validation:**
- ✅ Docker Compose starts successfully
- ✅ All services healthy (PostgreSQL, Redis, Elasticsearch)
- ✅ Database migrations run successfully
- ✅ API server starts on port 8000
- ✅ API documentation accessible at /docs
- ✅ Authentication endpoints working
- ⚠️ Web app (not yet implemented, planned for Week 4)
- ⚠️ Workers (not yet implemented, planned for Week 5)

**Code Quality:**
- ✅ No parent repository imports found
- ✅ All imports are relative or from app.* namespace
- ✅ Models, schemas, and endpoints self-contained
- ✅ Configuration loaded from environment variables only

**Extraction Readiness: 85%**
- Remaining work: CI/CD setup, frontend scaffold
- Estimated extraction time: 2-3 hours (reduced from 2-4 with current progress)

---

**Last Validated:** 2025-10-02 (Week 2 Complete)
**Next Validation:** Weekly with each phase completion
**Extraction Status:** ✅ READY (backend only, frontend pending)

---

**This checklist ensures extraction is always possible with minimal effort. Follow it religiously!**
