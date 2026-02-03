# Getting Started with Aethyme Cloud

**Complete setup guide for local development**

---

## 🚀 Quick Start (5 minutes)

### Prerequisites

Ensure you have these installed:
- ✅ Docker Desktop (running)
- ✅ Node.js 20+ (`node --version`)
- ✅ Python 3.11+ (`python3 --version`)
- ✅ pnpm 8+ (`pnpm --version`)

### Step 1: Install Dependencies

```bash
cd Mockup/packages/aethyme-cloud

# Install all dependencies (frontend + backend tools)
pnpm install
```

### Step 2: Set Up Environment

```bash
# Copy environment template
cp .env.example .env

# Generate secrets
openssl rand -hex 32  # Use for JWT_SECRET_KEY
openssl rand -hex 32  # Use for REFRESH_TOKEN_SECRET_KEY

# Edit .env and fill in required values
# Required for Week 1-2:
# - DATABASE_URL=postgresql://aethyme:aethyme@localhost:5434/aethyme_cloud
# - REDIS_URL=redis://localhost:6381/0
# - ELASTICSEARCH_URL=http://localhost:9202
# - JWT_SECRET_KEY (from openssl command above)
# - REFRESH_TOKEN_SECRET_KEY (from openssl command above)
# - JWT_EXPIRATION_MINUTES=1440 (24 hours)
# - REFRESH_TOKEN_EXPIRATION_DAYS=30
```

### Step 3: Start Infrastructure

```bash
# Start PostgreSQL, Redis, Elasticsearch
docker-compose up -d

# Wait for services to be healthy (~30 seconds)
docker-compose ps
```

### Step 4: Set Up Backend

```bash
# Navigate to API directory
cd apps/api

# Create virtual environment
python3 -m venv venv
source venv/bin/activate  # On Windows: venv\Scripts\activate

# Install Python dependencies
pip install -r requirements.txt

# Run database migrations
alembic upgrade head

# Return to root
cd ../..
```

### Step 5: Start Development Servers

```bash
# Start API (in apps/api directory)
cd apps/api
source venv/bin/activate
uvicorn app.main:app --reload --port 8000

# In another terminal, start Web (not yet implemented)
# cd apps/web && pnpm dev

# Workers will be added in later phases
```

### Step 6: Verify Everything Works

Open your browser:

- 🔧 **API Docs (Swagger):** http://localhost:8000/docs
- 📖 **API Docs (ReDoc):** http://localhost:8000/redoc
- 🌐 **Frontend:** (Coming in Phase 2 - Week 4)
- 📊 **GraphQL:** (Coming in Phase 2 - Week 3)

Test API health:
```bash
curl http://localhost:8000/api/health
# Should return: {"status":"healthy"}

# Test detailed health check
curl http://localhost:8000/api/health/detailed
# Should return: {"status":"healthy","database":"connected","redis":"connected"}
```

Test authentication:
```bash
# Register a new user
curl -X POST http://localhost:8000/api/auth/register \
  -H "Content-Type: application/json" \
  -d '{
    "email": "test@example.com",
    "password": "SecurePass123",
    "full_name": "Test User"
  }'

# Should return: access_token, refresh_token, and user object
```

---

## 📁 Project Structure

```
aethyme-cloud/
├── apps/
│   ├── api/              # FastAPI backend (Python)
│   │   ├── app/
│   │   │   ├── api/     # REST & GraphQL endpoints
│   │   │   ├── core/    # Config, database, auth
│   │   │   ├── models/  # SQLAlchemy models
│   │   │   └── services/# Business logic
│   │   └── requirements.txt
│   │
│   ├── web/              # Next.js dashboard (TypeScript)
│   │   ├── app/         # App Router pages
│   │   ├── components/  # React components
│   │   └── lib/         # Utilities
│   │
│   └── workers/          # Celery background tasks
│
├── packages/             # Shared libraries
│   ├── database/        # Shared DB models
│   ├── auth/            # Auth utilities
│   └── indexer/         # Code indexing logic
│
├── infrastructure/       # Deployment configs
│   ├── docker/
│   ├── terraform/
│   └── kubernetes/
│
└── docs/                 # Documentation
```

---

## 🛠️ Development Workflow

### Making Changes

**Frontend (Next.js):**
```bash
cd apps/web

# Start dev server (hot reload)
pnpm dev

# Run tests
pnpm test

# Type check
pnpm typecheck

# Lint
pnpm lint
```

**Backend (FastAPI):**
```bash
cd apps/api

# Start dev server (hot reload)
uvicorn app.main:app --reload

# Run tests
pytest

# Type check
pyright

# Lint
ruff check app/
```

### Database Migrations

```bash
cd apps/api

# Create new migration
alembic revision --autogenerate -m "Add users table"

# Apply migrations
alembic upgrade head

# Rollback last migration
alembic downgrade -1

# Reset database
alembic downgrade base && alembic upgrade head
```

### Running Tests

```bash
# Run all tests
pnpm test

# Backend only
cd apps/api && pytest

# Frontend only
cd apps/web && pnpm test

# E2E tests
cd apps/web && pnpm test:e2e
```

---

## 🐛 Troubleshooting

### Docker containers won't start

```bash
# Check if ports are already in use
lsof -i :5434  # PostgreSQL
lsof -i :6381  # Redis
lsof -i :9202  # Elasticsearch

# Stop all containers and restart
docker-compose down
docker-compose up -d
```

### Database connection errors

```bash
# Verify PostgreSQL is running
docker-compose ps postgres

# Check logs
docker-compose logs postgres

# Test connection
psql postgresql://aethyme:aethyme@localhost:5434/aethyme_cloud
```

### Frontend won't compile

```bash
cd apps/web

# Clear cache
rm -rf .next node_modules
pnpm install
pnpm dev
```

### Python dependencies issues

```bash
cd apps/api

# Recreate virtual environment
rm -rf venv
python3 -m venv venv
source venv/bin/activate
pip install -r requirements.txt
```

### Authentication errors

```bash
# Verify JWT secrets are set
grep JWT .env

# Check database has users table
cd apps/api
source venv/bin/activate
alembic current  # Should show latest revision

# View API logs for errors
# (while uvicorn is running in another terminal)
```

---

## 🔧 Configuration

### Environment Variables

Key variables to configure in `.env`:

**Required:**
- `DATABASE_URL` - PostgreSQL connection string
- `REDIS_URL` - Redis connection string
- `JWT_SECRET_KEY` - JWT signing key

**OAuth (optional for development):**
- `GITHUB_CLIENT_ID` / `GITHUB_CLIENT_SECRET`
- `GITLAB_CLIENT_ID` / `GITLAB_CLIENT_SECRET`

**Stripe (optional):**
- `STRIPE_SECRET_KEY`
- `STRIPE_WEBHOOK_SECRET`

See `.env.example` for full list.

---

## 📚 Next Steps

1. **Read the Architecture Docs:** `../aethyme/SAAS_ARCHITECTURE.md`
2. **Explore API Docs:** http://localhost:8000/docs
3. **Check Implementation Plan:** `../aethyme/planning-production-implementation-plan.md`
4. **Review Extraction Guide:** `extraction-checklist.md`
5. **See Week 1 Progress:** `week-1-development-complete.md`
6. **See Week 2 Progress:** `apps/api/week-2-authentication-complete.md`

---

## 🎯 Current Status (as of Week 2)

**✅ Completed:**
- Week 1: Infrastructure + Database Models
- Week 2: Authentication + User/Org Management APIs

**🚧 Next Up (Week 3-4):**
- Repository Management
- GitHub/GitLab/Bitbucket OAuth
- Webhook Handlers
- Basic Indexing Worker

**📊 Overall Progress:** ~12.5% of 16-20 week roadmap

---

## 🆘 Getting Help

- **Documentation:** `docs/`
- **API Reference:** http://localhost:8000/docs
- **Architecture:** `../aethyme/SAAS_ARCHITECTURE.md`
- **Weekly Reports:** `WEEK_*_COMPLETE.md` files

---

**Happy coding! 🚀**

_Last Updated: 2025-10-02 (Week 2 Complete)_
