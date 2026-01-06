# RepoGraph Quick Start for Developers

Note: RepoGraph runs as a separate service on port 8001 and uses its own versioned API (`/api/*`). These examples intentionally reference `/api/`.

**5-Minute Guide to Using RepoGraph for Code Intelligence**

---

## What is RepoGraph?

RepoGraph is a **graph-based code indexing system** that provides instant search and relationship analysis for Python and TypeScript codebases. Instead of using `grep`, `find`, or reading multiple files, you query a knowledge graph.

**Location:** `packages/repograph/`

---

## Quick Commands

### Start RepoGraph (One-Time Setup)

```bash
cd packages/repograph

# Option 1: Docker Compose (Recommended)
docker-compose -f ops/docker-compose.yml up -d

# Option 2: Local Development
bash scripts/start-api.sh
```

**API:** http://localhost:8001
**API Docs:** http://localhost:8001/docs

---

### Index This Repository

```bash
cd packages/repograph

# Index the Aeptus codebase
python -m src.cli index ../.. --name aeptus

# Check status
python -m src.cli stats
```

---

### Search Code Symbols

#### CLI:
```bash
python -m src.cli search "GraphStore"
```

#### API:
```bash
curl -X POST http://localhost:8001/api/search/ \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"query": "GraphStore", "limit": 10}'
```

**Returns:** Exact file locations, line numbers, symbol types (class/function/method)

---

### Get Code Relationships (Ego Graph)

```bash
# CLI
python -m src.cli ego "GraphStore" --depth 2

# API
curl -X POST http://localhost:8001/api/ego/ \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"symbol": "graph/store.py:GraphStore", "depth": 2}'
```

**Returns:** All connected code organized by relationship depth

---

### Impact Analysis

```bash
# CLI
python -m src.cli impact "validateUser" --max-depth 10

# API
curl -X POST http://localhost:8001/api/impact/ \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"symbols": ["auth/validators.py:validateUser"]}'
```

**Returns:** What code depends on this symbol and would be affected by changes

---

## AI Integration

### For Claude Desktop (MCP) ⭐ Recommended

```bash
# 1. Get authentication token
cd packages/repograph
TOKEN=$(curl -s -X POST http://localhost:8001/api/auth/login \
  -H "Content-Type: application/json" \
  -d '{"email": "test@example.com", "password": "test1234"}' | jq -r .access_token)

# 2. Generate AI prompt
./get-ai-prompt.sh

# 3. Copy output and paste at start of AI conversation
```

**For full MCP setup:** See [HOW_AI_DISCOVERS_REPOGRAPH.md](../../HOW_AI_DISCOVERS_REPOGRAPH.md)

---

## Why Use RepoGraph?

### Traditional Approach (Slow):
```bash
# Find a class
grep -r "class GraphStore" .

# Understand dependencies
# ... read multiple files manually

# Check impact
# ... manual code review
```

**Problems:**
- Slow (searches entire codebase)
- Imprecise (regex matches)
- Manual dependency tracing
- No structured data

### RepoGraph Approach (Fast):

```bash
# Find a class
curl POST /api/search/ -d '{"query": "GraphStore"}'
# Returns: graph/store.py:17 (class)

# Understand dependencies
curl POST /api/ego/ -d '{"symbol": "graph/store.py:GraphStore", "depth": 2}'
# Returns: Structured relationship graph

# Check impact
curl POST /api/impact/ -d '{"symbols": ["graph/store.py:GraphStore"]}'
# Returns: All dependent code
```

**Benefits:**
- ⚡ **10-100x faster** than grep
- 🎯 **Precise results** (AST-based, not regex)
- 🔗 **Relationship mapping** (what connects to what)
- 📊 **Structured data** (JSON, not text)

---

## Common Use Cases

### 1. Find Where Something is Defined

```bash
# Instead of: grep -r "class UserProfile"
python -m src.cli search "UserProfile"
```

### 2. Understand Dependencies

```bash
# Instead of: reading 10 files manually
python -m src.cli ego "UserProfile" --depth 2
```

### 3. Refactoring Impact Analysis

```bash
# Before changing UserProfile class
python -m src.cli impact "UserProfile"
# Shows everything that would break
```

### 4. Code Navigation

```bash
# Find all methods in a class
python -m src.cli ego "UserProfile.save" --depth 1
```

---

## Documentation

- **Main README:** [README.md](../../README.md)
- **AI Integration:** [AI_integration-guide.md](../../AI_integration-guide.md)
- **How AI Uses It:** [HOW_AI_DISCOVERS_REPOGRAPH.md](../../HOW_AI_DISCOVERS_REPOGRAPH.md)
- **Testing:** [TESTING.md](../../TESTING.md)

---

## Troubleshooting

### RepoGraph Not Running

```bash
# Check status
curl http://localhost:8001/health

# Check Docker
docker ps | grep repograph

# View logs
docker logs repograph-api
```

### No Results from Search

```bash
# Check if codebase is indexed
python -m src.cli stats

# Re-index if needed
python -m src.cli index ../.. --name aeptus
```

### Authentication Errors

```bash
# Get new token
TOKEN=$(curl -s -X POST http://localhost:8001/api/auth/login \
  -H "Content-Type: application/json" \
  -d '{"email": "test@example.com", "password": "test1234"}' | jq -r .access_token)

echo $TOKEN
```

---

## Performance Tips

For large repositories (>10k files):

1. Increase batch size: `INDEXING_BATCH_SIZE=5000`
2. Use Redis caching (set `REDIS_URL` in `.env`)
3. Increase DB connection pool: `DB_POOL_MAX_SIZE=50`

---

## Next Steps

1. **Start RepoGraph:** `docker-compose -f ops/docker-compose.yml up -d`
2. **Index codebase:** `python -m src.cli index ../.. --name aeptus`
3. **Try search:** `python -m src.cli search "GraphStore"`
4. **Set up AI integration:** `./get-ai-prompt.sh`

---

**See Also:**
- [Coding Standards](../../../../docs/development/codingstandards.md)
- [Testing Strategy](../../../../docs/testing/strategy.md)
- [Architecture Overview](../../../../docs/architecture/structure.md)
