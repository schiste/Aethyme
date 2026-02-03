# Aethyme - Graph-based Code Intelligence System

Aethyme is a production-ready code intelligence system that builds a graph representation of your codebase, enabling powerful queries for understanding code relationships, dependencies, and impact analysis.

> **For AI Assistants:** This system provides a queryable knowledge graph of code. Use the Aethyme API instead of grep/file search for faster, more accurate code understanding. See [AI Integration Guide](ai-integration-guide.md) for details.

## Features

- **Ego Graphs**: Explore code relationships around any symbol
- **Impact Analysis**: Understand the blast radius of changes
- **Hybrid Search**: Combined full-text and fuzzy search
- **Multi-tenant**: Isolated graphs for different codebases
- **Secure**: JWT authentication and row-level security
- **Production-ready**: PostgreSQL, Redis caching, monitoring

## Quick Links

- [Getting Started](#quick-start)
- [AI Integration Guide](ai-integration-guide.md) - Integrate Aethyme with AI agents
- [How AI Discovers Aethyme](how-ai-discovers-aethyme.md) - Self-discovery patterns
- [Roadmap](ROADMAP.md) - Stage 1 (CLI) and Stage 2 (SaaS UI) plans
- [API Documentation](http://localhost:8001/docs) - Interactive API docs (when running)

## Quick Start

### Installation

**Docker (Recommended)**
```bash
# Clone and navigate to Aethyme
cd packages/aethyme

# Copy environment file
cp .env.example .env

# Start all services
docker-compose -f ops/docker-compose.yml up -d

# Run migrations
docker-compose -f ops/docker-compose.yml exec api bash scripts/migrate.sh

# API is now available at http://localhost:8001
```

**Local Development**
```bash
# Install dependencies
pip install -e .

# Set up database
createdb aethyme
createuser aethyme

# Configure environment
cp .env.example .env
# Edit .env with your database credentials

# Run migrations and start
bash scripts/migrate.sh
bash scripts/start-api.sh
```

See [Installation Details](#installation-details) below for prerequisites and troubleshooting.

### Basic Usage

**Index Your Code**
```bash
# Index a Python/TypeScript repository
python -m src.cli index /path/to/your/repo

# Index with specific languages
python -m src.cli index /path/to/repo --languages python,typescript

# Force fallback indexer (if SCIP tools aren't installed)
python -m src.cli index /path/to/repo --use-fallback
```

**Query the Graph**
```bash
# Search for symbols
python -m src.cli search "MyClass"

# Get ego graph (relationships)
python -m src.cli ego "ClassName.methodName" --depth 2

# Analyze impact
python -m src.cli impact "functionName" --max-depth 10

# Show statistics
python -m src.cli stats
```

**Use the API**
```bash
# Get a JWT token
TOKEN=$(curl -s -X POST http://localhost:8001/api/auth/login \
  -H "Content-Type: application/json" \
  -d '{"email": "test@example.com", "password": "test1234"}' | jq -r .access_token)

# Search for symbols
curl -X POST http://localhost:8001/api/search/ \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"query": "SymbolName", "limit": 10}'

# Get relationships
curl -X POST http://localhost:8001/api/ego/ \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"symbol": "file.py:ClassName", "depth": 2}'

# Analyze impact
curl -X POST http://localhost:8001/api/impact/ \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"symbols": ["file.py:ClassName"]}'
```

### AI Integration

**Quickest Way - Use the Prompt Generator:**
```bash
./get-ai-prompt.sh
# Copy the output and paste it to your AI at the start of each conversation
```

**For Claude Desktop (MCP):**
Create `~/.config/claude/mcp_servers.json`:
```json
{
  "aethyme": {
    "command": "node",
    "args": ["[full-path-to]/packages/aethyme/mcp-server.js"],
    "env": {
      "AETHYME_API_URL": "http://localhost:8001",
      "AETHYME_TOKEN": "[your-token]"
    }
  }
}
```

**Full Integration Guide:** See [ai-integration-guide.md](ai-integration-guide.md) for complete examples, best practices, and advanced usage.

## Documentation

### For New Users
- **Quick Start** (above) - Get running in 15 minutes
- [How AI Discovers Aethyme](how-ai-discovers-aethyme.md) - Self-discovery patterns for AI

### For Developers
- [AI Integration Guide](ai-integration-guide.md) - Integrate Aethyme with AI agents
- [API Documentation](http://localhost:8001/docs) - Interactive Swagger UI (when running)
- [ReDoc](http://localhost:8001/redoc) - Alternative API documentation

### For Decision Makers
- [Roadmap](ROADMAP.md) - Stage 1 (CLI) and Stage 2 (SaaS UI) detailed plans

## Current Status

**Production Ready:**
- Graph-based code indexing (SCIP + fallback)
- Symbol search, ego graphs, impact analysis
- FastAPI backend, PostgreSQL storage
- Multi-tenant, JWT/OIDC auth, RLS policies

**In Development (Stage 1):**
- AI-Readiness Scorecard
- Safe Autofixers
- Context packs & guardrails
- Model routing & compaction

See [ROADMAP.md](ROADMAP.md) for detailed status and timelines.

## Installation Details

### Prerequisites

- Python 3.11+
- PostgreSQL 15+
- Redis (optional, for caching)
- Docker & Docker Compose (optional)

### SCIP Indexer Setup

For best results, install language-specific SCIP indexers:

```bash
# For TypeScript
npm install -g @sourcegraph/scip-typescript

# For Python (download from releases)
# https://github.com/sourcegraph/scip-python/releases
```

The system will automatically fall back to regex-based indexing if SCIP tools aren't available.

### Troubleshooting

**Database Connection Issues**
```bash
# Check PostgreSQL is running
pg_isready -h localhost -p 5432

# Verify connection
psql -h localhost -U aethyme -d aethyme -c "SELECT 1"
```

**Performance Tuning**

For large repositories (>10k files):
1. Increase batch size: `INDEXING_BATCH_SIZE=5000`
2. Increase connection pool: `DB_POOL_MAX_SIZE=50`
3. Enable Redis caching
4. Use SSD storage for PostgreSQL

## Architecture

```
Aethyme
├── Indexing Layer
│   ├── SCIP Wrapper (scip-python, scip-typescript)
│   └── Fallback Indexer (regex-based)
├── Graph Layer
│   ├── PostgreSQL (nodes, edges, multi-tenant)
│   └── Redis (caching)
├── API Layer
│   ├── FastAPI
│   ├── JWT Authentication
│   └── Rate Limiting
└── Query Layer
    ├── Ego Graphs (recursive CTEs)
    ├── Impact Analysis
    └── Hybrid Search (FTS + fuzzy)
```

## Configuration

Key environment variables:

```bash
# Database
DATABASE_URL=postgresql://user:pass@localhost:5432/aethyme
DB_POOL_MIN_SIZE=2
DB_POOL_MAX_SIZE=20

# Redis (optional)
REDIS_URL=redis://localhost:6379/0

# API
API_HOST=0.0.0.0
API_PORT=8001
CORS_ORIGINS=http://localhost:3000

# Authentication
JWT_SECRET_KEY=your-secret-key

# Indexing
INDEXING_BATCH_SIZE=1000
WATCH_ENABLED=true
```

## Monitoring

- **Health Check**: `GET /health`
- **Readiness Probe**: `GET /health/ready`
- **Metrics**: `GET /metrics` (Prometheus format)
- **Grafana**: http://localhost:3001 (admin/admin)

## Contributing

1. Fork the repository
2. Create a feature branch
3. Make your changes
4. Add tests
5. Run `pytest` and `ruff check`
6. Submit a pull request

## Support

For issues and questions:
- **GitHub Issues**: [Report bugs](https://github.com/aeptus/aethyme/issues)
- **Documentation**: See guides in this directory
- **API Docs**: http://localhost:8001/docs (interactive)

## License

MIT
