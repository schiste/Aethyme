# Quick Start - Next Actions

**What to do RIGHT NOW to move forward**

**Current Status:** Phase 5/12 (35% MVP) ✅
**Date:** October 4, 2025

---

## ⚡ Today's Priority

### Complete Phase 6: Repository Indexing (Start Now)

**Why this matters:** Everything depends on indexing. No indexing = no search = no product.

**Time estimate:** 7-10 days

---

## 🎯 Week 1-2 Plan: Repository Indexing

### Day 1-2: Celery Setup & Git Operations

**Morning (4 hours):**
```bash
# 1. Install Celery dependencies
cd apps/api
pip install celery[redis] GitPython

# 2. Create Celery app configuration
touch app/workers/__init__.py
touch app/workers/celery_app.py
touch app/workers/tasks/indexing.py

# 3. Configure Celery
# Edit app/workers/celery_app.py with Redis broker
```

**Afternoon (4 hours):**
```python
# Implement Git cloning service
# File: app/services/git.py

class GitService:
    def clone_repository(repo_url, access_token):
        # Clone with OAuth token authentication
        pass

    def pull_updates(repo_path):
        # Pull latest changes
        pass

    def get_changed_files(repo_path, since_commit):
        # Get list of changed files for incremental indexing
        pass
```

---

### Day 3-4: Tree-sitter Integration

**Install parsers:**
```bash
pip install tree-sitter
pip install tree-sitter-python tree-sitter-javascript tree-sitter-typescript
pip install tree-sitter-go tree-sitter-rust tree-sitter-java
```

**Implement parser:**
```python
# File: app/services/tree_sitter.py

class TreeSitterParser:
    def parse_file(file_path, language):
        # Parse file with Tree-sitter
        # Extract symbols (functions, classes, variables)
        # Return AST nodes
        pass

    def extract_symbols(ast_node):
        # Convert AST to symbol list
        pass
```

---

### Day 5-6: SCIP Index Generation

**Install SCIP:**
```bash
pip install scip-python  # Or implement SCIP format manually
```

**Implement SCIP generator:**
```python
# File: app/services/scip.py

class SCIPGenerator:
    def generate_index(repository):
        # Convert Tree-sitter symbols to SCIP format
        # Extract relationships (calls, imports, inheritance)
        # Generate SCIP index file
        pass
```

---

### Day 7-8: Elasticsearch Indexing

**Create search mappings:**
```python
# File: app/services/elasticsearch.py

MAPPINGS = {
    "code_file": {
        "properties": {
            "repository_id": {"type": "keyword"},
            "path": {"type": "keyword"},
            "content": {"type": "text", "analyzer": "code"},
            "language": {"type": "keyword"},
            "symbols": {"type": "nested"}
        }
    }
}

class ElasticsearchIndexer:
    def index_repository(repository, symbols):
        # Create Elasticsearch documents
        # Bulk index all files
        pass
```

---

### Day 9-10: Webhook Handlers & Testing

**Implement webhooks:**
```python
# File: app/api/v1/endpoints/webhooks.py

@router.post("/github/push")
async def github_push_webhook(payload: dict):
    # Verify webhook signature
    # Extract changed files
    # Trigger re-indexing job
    pass
```

**End-to-end test:**
1. Connect GitHub repo via OAuth ✅ (already working)
2. Trigger indexing job → verify Celery task runs
3. Check Elasticsearch → verify documents created
4. Push code to GitHub → verify webhook triggers re-index
5. Search for code → verify results returned

---

## 📁 Files to Create

### Backend (Priority Order)

1. **`apps/api/app/workers/celery_app.py`** (Day 1)
   ```python
   from celery import Celery
   from app.core.config import settings

   celery_app = Celery(
       "repograph",
       broker=str(settings.REDIS_URL),
       backend=str(settings.REDIS_URL)
   )
   ```

2. **`apps/api/app/workers/tasks/indexing.py`** (Day 1-2)
   - `index_repository_task(repository_id)`
   - `reindex_files_task(repository_id, file_paths)`

3. **`apps/api/app/services/git.py`** (Day 2)
   - GitService class

4. **`apps/api/app/services/tree_sitter.py`** (Day 3-4)
   - TreeSitterParser class

5. **`apps/api/app/services/scip.py`** (Day 5-6)
   - SCIPGenerator class

6. **`apps/api/app/services/elasticsearch.py`** (Day 7-8)
   - ElasticsearchIndexer class

7. **`apps/api/app/api/v1/endpoints/webhooks.py`** (Day 9)
   - Webhook handlers for GitHub/GitLab/Bitbucket

### Frontend (Later)

8. **`apps/web/components/repositories/IndexingStatus.tsx`**
   - Show indexing progress
   - Files indexed count
   - Symbols extracted count

---

## 🧪 Testing Checklist

### Unit Tests
- [ ] Git clone with OAuth token works
- [ ] Tree-sitter parses Python correctly
- [ ] Tree-sitter parses TypeScript correctly
- [ ] SCIP index generates valid format
- [ ] Elasticsearch documents created correctly

### Integration Tests
- [ ] Celery task executes successfully
- [ ] Repository cloned to correct location
- [ ] All files parsed without errors
- [ ] Elasticsearch searchable after indexing
- [ ] Webhook triggers re-indexing

### Performance Tests
- [ ] 1000 files indexed in <5 minutes
- [ ] Incremental re-index <1 minute
- [ ] Memory usage <2GB during indexing

---

## 🚀 After Phase 6: Next Week

### Week 3: Phase 7 - Code Search

**Priority: Implement search API**

```bash
# Search endpoint
GET /api/search?q=function+authenticate&repo=api-backend

# Returns:
{
  "results": [
    {
      "file": "apps/api/auth/jwt.py",
      "line": 47,
      "content": "def authenticate(username, password):",
      "repository": "api-backend",
      "language": "python"
    }
  ],
  "total": 23,
  "took_ms": 45
}
```

**Files to create:**
- `apps/api/app/api/v1/endpoints/search.py`
- `apps/web/app/search/page.tsx`
- `apps/web/components/search/CommandPalette.tsx`

---

## 📊 Success Metrics

### This Week (Phase 6)
- ✅ Repository clones successfully
- ✅ 100+ files indexed
- ✅ Symbols extracted from code
- ✅ Elasticsearch documents created
- ✅ Webhook triggers re-index

### Next Week (Phase 7)
- ✅ Search returns results <100ms
- ✅ Symbol search finds definitions
- ✅ Search filters work

### End of Month (MVP Complete)
- ✅ All 12 phases complete
- ✅ 50 beta users signed up
- ✅ 10 paying teams
- ✅ $2K MRR

---

## 💡 Quick Wins

**Can't complete Phase 6 right now? Do these instead:**

1. **Set up monitoring** (2 hours)
   - Add Sentry for error tracking
   - Set up Datadog/New Relic
   - Create uptime monitoring

2. **Write documentation** (4 hours)
   - API documentation (OpenAPI)
   - Developer guide
   - Architecture diagrams

3. **Improve OAuth flow** (2 hours)
   - Add loading states
   - Better error messages
   - Success animations

4. **Talk to users** (Ongoing)
   - Post on Twitter about what you're building
   - Share on Hacker News
   - Get early feedback

---

## 🎯 The One Thing

**If you do NOTHING else: Start Day 1 of Phase 6 today.**

1. Create Celery app configuration (1 hour)
2. Implement basic Git clone (2 hours)
3. Test cloning a repository (1 hour)

**Total: 4 hours to first milestone**

---

## 📞 Need Help?

### Technical Questions
- **Celery:** https://docs.celeryq.dev/
- **Tree-sitter:** https://tree-sitter.github.io/tree-sitter/
- **SCIP:** https://github.com/sourcegraph/scip
- **Elasticsearch:** https://www.elastic.co/guide/en/elasticsearch/reference/current/index.html

### Stuck?
- Check [repograph-readme.md](repograph-readme.md) for project structure
- Read [PHASE_5_OAUTH_COMPLETE.md](PHASE_5_OAUTH_COMPLETE.md) for OAuth reference
- Review [PRODUCT_ROADMAP_SALES_READY.md](PRODUCT_ROADMAP_SALES_READY.md) for big picture

---

## ✅ Daily Checklist

Every day:
- [ ] Code for 6-8 hours
- [ ] Commit and push code
- [ ] Update project-status.md with progress
- [ ] Test what you built
- [ ] Document any blockers

Every Friday:
- [ ] Review week's progress
- [ ] Demo to yourself (screen recording)
- [ ] Plan next week
- [ ] Celebrate wins 🎉

---

**Current Priority: Day 1 of Phase 6 - Celery Setup**

**Next file to create: `apps/api/app/workers/celery_app.py`**

**Let's build. 🚀**
