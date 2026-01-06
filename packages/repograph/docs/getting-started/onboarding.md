# Developer Onboarding Guide

Welcome to the RepoGraph development team! This guide will get you up and running.

---

## Day 1: Setup

### Prerequisites

Install required tools:

```bash
# macOS
brew install python@3.11 postgresql@15 redis node@20

# Ubuntu/Debian
sudo apt install python3.11 postgresql-15 redis-server nodejs npm

# Verify installations
python3 --version  # Should be 3.11+
psql --version     # Should be 15+
redis-cli --version
node --version     # Should be 20+
```

### Clone Repository

```bash
# Clone repo
git clone https://github.com/aeptus/repograph.git
cd repograph/packages/repograph

# Create virtual environment
python3 -m venv venv
source venv/bin/activate  # On Windows: venv\Scripts\activate

# Install dependencies
pip install -e ".[dev]"
```

### Database Setup

```bash
# Start PostgreSQL
brew services start postgresql@15  # macOS
sudo systemctl start postgresql    # Linux

# Create database and user
createdb repograph
psql -d repograph -c "CREATE USER repograph WITH PASSWORD 'password';"
psql -d repograph -c "GRANT ALL PRIVILEGES ON DATABASE repograph TO repograph;"

# Run migrations
bash scripts/migrate.sh
```

### Configuration

```bash
# Copy environment template
cp .env.example .env

# Edit .env with your settings
nano .env

# Required variables:
DATABASE_URL=postgresql://repograph:password@localhost:5432/repograph
REDIS_URL=redis://localhost:6379/0
JWT_SECRET_KEY=your-secret-key-change-this
```

### Start Services

```bash
# Start Redis
brew services start redis  # macOS
sudo systemctl start redis # Linux

# Start API server
bash scripts/start-api.sh

# Verify services
curl http://localhost:8001/health
# Expected: {"status": "healthy"}
```

### Run Tests

```bash
# Run all tests
pytest tests/

# Run with coverage
pytest tests/ --cov=src --cov-report=html

# View coverage report
open htmlcov/index.html  # macOS
xdg-open htmlcov/index.html  # Linux
```

### First Indexing

```bash
# Index a small repository
python -m src.cli index . --name repograph

# Query the index
python -m src.cli search "GraphStore"
python -m src.cli ego "GraphStore"
```

**Congratulations! Your development environment is ready.**

---

## Day 2-3: Codebase Tour

### Architecture Overview

```
repograph/
├── src/              # Source code
│   ├── api/          # FastAPI application
│   ├── graph/        # Graph storage and queries
│   ├── indexer/      # Code indexing (SCIP + fallback)
│   ├── auth/         # Authentication and authorization
│   └── cli.py        # CLI commands
├── tests/            # Test suite
├── docs/             # Documentation
├── ops/              # Deployment configs
└── scripts/          # Utility scripts
```

### Key Files

| File | Purpose |
|------|---------|
| `src/api/main.py` | FastAPI app entry point |
| `src/graph/store.py` | Graph database interface |
| `src/indexer/scip_indexer.py` | SCIP-based indexing |
| `src/indexer/fallback_indexer.py` | Regex-based fallback |
| `src/auth/middleware.py` | JWT authentication |
| `src/config.py` | Configuration management |
| `tests/conftest.py` | Pytest fixtures |

### Data Flow

```
1. Indexing: Repository → SCIP/Fallback → Nodes/Edges → PostgreSQL
2. Query: API Request → Auth → GraphStore → PostgreSQL → Redis Cache → Response
3. CLI: Command → GraphStore → Display Results
```

### Database Schema

Key tables:
- `tenants`: Multi-tenant isolation
- `repositories`: Indexed repositories
- `nodes`: Code symbols (classes, functions, etc.)
- `edges`: Relationships (imports, calls, etc.)
- `users`: User accounts
- `api_keys`: API authentication

---

## Week 1: First Contribution

### Development Workflow

```bash
# 1. Create feature branch
git checkout -b feature/your-feature-name

# 2. Make changes
# ... edit code ...

# 3. Run tests
pytest tests/
black src/ tests/  # Format code
ruff check src/ tests/  # Lint

# 4. Commit
git add .
git commit -m "feat: add your feature"

# 5. Push and create PR
git push origin feature/your-feature-name
# Open PR on GitHub
```

### Code Style

We follow:
- **PEP 8** for Python
- **Black** for formatting (line length: 88)
- **Ruff** for linting
- **Type hints** required for public APIs

Example:

```python
from typing import List, Optional

async def search_symbols(
    query: str,
    limit: int = 20,
    filters: Optional[dict] = None
) -> List[SearchResult]:
    """
    Search for code symbols.

    Args:
        query: Search query string
        limit: Maximum results to return
        filters: Optional filters (kind, language, etc.)

    Returns:
        List of search results
    """
    # Implementation
    pass
```

### Testing Requirements

- **Unit tests**: Test individual functions
- **Integration tests**: Test API endpoints
- **Coverage**: Aim for > 80%

Example test:

```python
import pytest
from src.graph.store import GraphStore

@pytest.mark.asyncio
async def test_search_symbols(graph_store: GraphStore):
    """Test symbol search returns correct results."""
    results = await graph_store.search("GraphStore", limit=10)

    assert len(results) > 0
    assert results[0].symbol == "GraphStore"
    assert results[0].kind == "class"
```

### Pull Request Process

1. **Create PR** with description of changes
2. **Link issue** if applicable
3. **Add tests** for new functionality
4. **Update docs** if API/behavior changed
5. **Request review** from team
6. **Address feedback**
7. **Merge** after approval

---

## Common Development Tasks

### Add New API Endpoint

```python
# src/api/routes.py

from fastapi import APIRouter, Depends
from src.auth.middleware import get_current_user

router = APIRouter()

@router.get("/api/my-endpoint")
async def my_endpoint(user = Depends(get_current_user)):
    """Endpoint description."""
    return {"message": "Hello, " + user.email}
```

### Add New CLI Command

```python
# src/cli.py

@cli.command()
@click.argument("arg")
@click.option("--option", default="value")
def my_command(arg: str, option: str):
    """Command description."""
    click.echo(f"Argument: {arg}, Option: {option}")
```

### Add Database Migration

```bash
# Create migration
alembic revision -m "add_new_column"

# Edit migration file (alembic/versions/xxx_add_new_column.py)
def upgrade():
    op.add_column('nodes', sa.Column('new_field', sa.String(), nullable=True))

def downgrade():
    op.drop_column('nodes', 'new_field')

# Apply migration
alembic upgrade head
```

### Debug Issues

```bash
# Enable debug logging
LOG_LEVEL=DEBUG python -m src.cli index /path/to/repo

# Run API with auto-reload
uvicorn src.api.main:app --reload --log-level debug

# Interactive debugging
import pdb; pdb.set_trace()  # Add breakpoint
python -m pytest tests/test_file.py -k test_function -s  # Run with output
```

---

## Resources

### Documentation
- [Quickstart Guide](quickstart.md)
- [API Reference](../reference/api.md)
- [CLI Reference](../reference/cli.md)
- [Architecture Overview](../architecture/stage1-architecture.md)
- [Testing Guide](../guides/testing.md)

### Team Communication
- **Slack**: #repograph-dev
- **Standups**: Daily 10am PST
- **Sprint Planning**: Bi-weekly Mondays
- **Demo**: Bi-weekly Fridays

### Getting Help
- **Ask in #repograph-dev** for quick questions
- **Open GitHub Discussion** for longer topics
- **Pair programming**: Schedule with any team member
- **1-on-1 with mentor**: Weekly

---

## Your First Tasks

### Suggested Starter Tasks

Good first issues to build familiarity:

1. **Documentation**: Update a doc page with clarifications
2. **Testing**: Add test for existing functionality
3. **Bug Fix**: Pick a "good first issue" from GitHub
4. **Feature**: Implement small enhancement

Example starter tasks:
- Add `--verbose` flag to CLI command
- Improve error message formatting
- Add validation for API input
- Write integration test for endpoint

---

## Tips for Success

1. **Read the code**: Best way to learn the codebase
2. **Run tests frequently**: Catch issues early
3. **Ask questions**: No question is too simple
4. **Document as you learn**: Help future developers
5. **Contribute to reviews**: Learn from others' code
6. **Attend demos**: See what others are building

---

## Appendix: Troubleshooting

### Database Connection Failed

```bash
# Check PostgreSQL status
pg_isready -h localhost

# Restart PostgreSQL
brew services restart postgresql@15
```

### Redis Connection Failed

```bash
# Check Redis status
redis-cli ping

# Restart Redis
brew services restart redis
```

### Tests Failing

```bash
# Reset test database
dropdb repograph_test
createdb repograph_test
alembic upgrade head

# Clear pytest cache
pytest --cache-clear
```

### Import Errors

```bash
# Reinstall in development mode
pip install -e ".[dev]"

# Check PYTHONPATH
echo $PYTHONPATH
export PYTHONPATH="/path/to/repograph/packages/repograph:$PYTHONPATH"
```

---

**Welcome to the team! Happy coding!**
