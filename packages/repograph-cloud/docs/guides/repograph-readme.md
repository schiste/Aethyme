# RepoGraph Cloud - Developer Onboarding Guide

**Welcome to RepoGraph Cloud!** 👋

This is your complete guide to understanding, setting up, and contributing to RepoGraph Cloud - a SaaS platform for queryable code intelligence.

---

## 🎯 What is RepoGraph Cloud?

RepoGraph Cloud turns your repositories into a **queryable knowledge graph**:

- 🔍 **Search code** across all your repos instantly
- 📊 **Analyze relationships** between functions, classes, files
- 🤖 **AI integration** for Claude, GPT, Cursor, and more
- 💻 **IDE plugins** for VS Code, JetBrains, Vim
- 🔐 **Multi-tenant SaaS** with enterprise security

**Think:** "GitHub for Code Intelligence" - hosted service that understands your codebase.

---

## 📊 Current Status (Oct 4, 2025)

**TL;DR:** Solid foundation (25%), core features pending (75%)

### ✅ What's Working
- **Authentication:** JWT, refresh tokens, API keys
- **Multi-tenancy:** Organization-scoped data, RLS
- **Repository Management:** CRUD operations (UI + API)
- **Beautiful UI:** Next.js 14, dark mode, keyboard shortcuts
- **Security:** Bcrypt hashing, rate limiting, input validation

### ⏳ What's Missing (Critical Path)
- **OAuth:** Can't connect GitHub/GitLab/Bitbucket yet
- **Indexing:** No code analysis workers yet
- **Search:** API exists, but no implementation
- **Graph Analysis:** Relationship analysis pending

**Bottom Line:** Professional scaffold ready for core features (6-8 weeks to MVP)

---

## 🚀 Quick Start (5 Minutes)

### Prerequisites

```bash
# Check you have:
docker --version        # Docker Desktop running
node --version          # Node.js 20+
python3 --version       # Python 3.11+
pnpm --version          # pnpm 8+
```

### 1. Clone & Install

```bash
# If in main Mockup repo
cd packages/repograph-cloud

# Install dependencies
pnpm install

# Install Python backend deps
cd apps/api
python3 -m venv venv
source venv/bin/activate  # On Windows: venv\Scripts\activate
pip install -r requirements.txt
cd ../..
```

### 2. Configure Environment

```bash
# Copy environment template
cp .env.example .env

# Generate secrets (macOS/Linux)
echo "JWT_SECRET_KEY=$(openssl rand -hex 32)" >> .env
echo "REFRESH_TOKEN_SECRET_KEY=$(openssl rand -hex 32)" >> .env

# Edit .env if needed (defaults work for local dev)
```

### 3. Start Infrastructure

```bash
# Start PostgreSQL, Redis, Elasticsearch
docker-compose up -d

# Wait for services to be healthy (~30 seconds)
docker-compose ps
```

### 4. Run Database Migrations

```bash
cd apps/api
source venv/bin/activate
alembic upgrade head
cd ../..
```

### 5. Start Development Servers

```bash
# Terminal 1: Backend API
cd apps/api
source venv/bin/activate
uvicorn app.main:app --reload --port 8000

# Terminal 2: Frontend Web (in packages/repograph-cloud/)
cd apps/web
pnpm dev

# Terminal 3: Celery Workers (optional, for indexing)
cd apps/workers
# Not yet implemented - coming in Phase 6
```

### 6. Verify Everything Works

Open in browser:
- 🌐 **Frontend:** http://localhost:3000
- 🔧 **API Docs:** http://localhost:8000/docs
- 📖 **ReDoc:** http://localhost:8000/redoc

Test API:
```bash
# Health check
curl http://localhost:8000/api/health

# Register user
curl -X POST http://localhost:8000/api/auth/register \
  -H "Content-Type: application/json" \
  -d '{
    "email": "dev@example.com",
    "password": "DevPassword123!",
    "full_name": "Dev User"
  }'
```

**🎉 You're ready to develop!**

---

## 📁 Project Structure

```
repograph-cloud/
├── apps/
│   ├── api/                    # FastAPI backend (Python)
│   │   ├── app/
│   │   │   ├── api/           # REST & GraphQL endpoints
│   │   │   ├── core/          # Config, database, auth
│   │   │   ├── models/        # SQLAlchemy models
│   │   │   ├── schemas/       # Pydantic schemas
│   │   │   ├── services/      # Business logic
│   │   │   └── main.py        # FastAPI app entry
│   │   ├── alembic/           # Database migrations
│   │   ├── tests/             # Pytest tests
│   │   └── requirements.txt
│   │
│   ├── web/                    # Next.js 14 frontend (TypeScript)
│   │   ├── app/               # App Router pages
│   │   │   ├── (auth)/        # Login, register
│   │   │   ├── (app)/         # Authenticated pages
│   │   │   └── layout.tsx
│   │   ├── components/        # React components
│   │   │   ├── ui/            # shadcn/ui components
│   │   │   ├── dashboard/     # Dashboard components
│   │   │   └── repository/    # Repo components
│   │   ├── lib/               # Utilities
│   │   │   ├── api.ts         # API client
│   │   │   ├── auth.ts        # Auth helpers
│   │   │   └── hooks/         # Custom React hooks
│   │   └── package.json
│   │
│   └── workers/                # Celery background tasks
│       └── (TODO: Phase 6)
│
├── packages/                   # Shared libraries (future)
│   ├── database/              # Shared DB models
│   ├── auth/                  # Auth utilities
│   └── indexer/               # Code indexing logic
│
├── infrastructure/             # Deployment configs (future)
│   ├── docker/
│   ├── terraform/             # GCP infrastructure
│   └── kubernetes/            # K8s manifests
│
├── docs/                       # Documentation
│   ├── api/                   # API reference
│   ├── guides/                # User guides
│   └── architecture/          # System design
│
├── docker-compose.yml          # Local dev infrastructure
├── .env.example               # Environment template
├── README.md                  # Project overview
└── repograph-readme.md        # This file!
```

---

## 🏗️ Architecture Overview

### Tech Stack

**Backend (API):**
- FastAPI 0.110+ (async ASGI framework)
- PostgreSQL 15 (multi-tenant database)
- Redis 7 (cache + task queue)
- Elasticsearch 8 (code search - ready but unused)
- SQLAlchemy 2 (async ORM)
- Alembic (migrations)
- Pydantic v2 (validation)
- Slowapi (rate limiting)

**Frontend (Web):**
- Next.js 14 (App Router)
- React 18 (UI library)
- TypeScript 5 (strict mode)
- Tailwind CSS 4 (styling)
- shadcn/ui (component library)
- React Query 5 (state management)
- Zustand (client state)
- NextAuth v5 (authentication - configured)

**Infrastructure (Local):**
- Docker Compose (PostgreSQL, Redis, Elasticsearch)
- Separate ports: 5434, 6381, 9202 (no conflicts with parent GRC)

**Infrastructure (Production - Planned):**
- Google Cloud Platform (GCP)
- Cloud Run (API + Web)
- Cloud SQL (PostgreSQL)
- Cloud Memorystore (Redis)
- GKE (Celery workers)

### Data Model

```sql
-- Multi-tenant hierarchy
Organization
    ├── Users (multiple)
    ├── Repositories (multiple)
    └── API Keys (multiple)

-- Key tables
CREATE TABLE organizations (
    id UUID PRIMARY KEY,
    name TEXT NOT NULL,
    slug TEXT UNIQUE,
    plan TEXT DEFAULT 'free'
);

CREATE TABLE users (
    id UUID PRIMARY KEY,
    email TEXT UNIQUE NOT NULL,
    hashed_password TEXT NOT NULL,
    organization_id UUID REFERENCES organizations(id)
);

CREATE TABLE repositories (
    id UUID PRIMARY KEY,
    organization_id UUID REFERENCES organizations(id),
    name TEXT NOT NULL,
    git_url TEXT,
    provider TEXT,  -- github, gitlab, bitbucket
    status TEXT DEFAULT 'pending',
    last_indexed_at TIMESTAMPTZ
);

CREATE TABLE api_keys (
    id UUID PRIMARY KEY,
    organization_id UUID REFERENCES organizations(id),
    key_hash TEXT NOT NULL UNIQUE,
    scopes TEXT[],
    expires_at TIMESTAMPTZ
);
```

### API Endpoints (23 total)

**Authentication (3):**
- `POST /api/auth/register` - User registration
- `POST /api/auth/login` - Email/password login
- `POST /api/auth/refresh` - Refresh access token

**Users (5):**
- `GET /api/users/me` - Current user
- `PATCH /api/users/me` - Update profile
- `GET /api/users/{id}`
- `GET /api/users/`
- `DELETE /api/users/{id}`

**Organizations (3):**
- `GET /api/organizations/me`
- `PATCH /api/organizations/me`
- `GET /api/organizations/me/stats`

**Repositories (7):**
- `POST /api/repositories/` - Connect repository
- `GET /api/repositories/` - List with pagination
- `GET /api/repositories/stats`
- `GET /api/repositories/{id}`
- `PATCH /api/repositories/{id}`
- `DELETE /api/repositories/{id}`
- `POST /api/repositories/{id}/reindex` - Trigger indexing

**API Keys (5):**
- `POST /api/api-keys/` - Generate key
- `GET /api/api-keys/` - List keys
- `GET /api/api-keys/{id}`
- `PATCH /api/api-keys/{id}` - Update
- `DELETE /api/api-keys/{id}` - Revoke

**Health (2):**
- `GET /api/health` - Basic health
- `GET /api/health/detailed` - Detailed status

---

## 🛠️ Development Workflows

### Backend Development

```bash
cd apps/api
source venv/bin/activate

# Start API with hot reload
uvicorn app.main:app --reload --port 8000

# Run tests
pytest

# Type checking
pyright

# Linting
ruff check app/

# Create migration
alembic revision --autogenerate -m "Description"

# Apply migrations
alembic upgrade head

# Rollback
alembic downgrade -1
```

### Frontend Development

```bash
cd apps/web

# Start dev server (hot reload)
pnpm dev

# Run tests
pnpm test

# Type checking
pnpm typecheck

# Linting
pnpm lint

# Build for production
pnpm build
```

### Database Operations

```bash
# Connect to PostgreSQL
docker exec -it repograph-cloud-postgres psql -U repograph -d repograph_cloud

# View tables
\dt

# View users
SELECT * FROM users;

# Reset database (DANGER!)
docker-compose down -v
docker-compose up -d
cd apps/api && alembic upgrade head
```

### Docker Operations

```bash
# Start services
docker-compose up -d

# View logs
docker-compose logs -f

# Restart service
docker-compose restart postgres

# Stop all
docker-compose down

# Rebuild service
docker-compose up -d --build api
```

---

## 🧪 Testing

### Backend Tests

```bash
cd apps/api
source venv/bin/activate

# Run all tests
pytest

# Run with coverage
pytest --cov=app --cov-report=html

# Run specific test
pytest tests/api/test_auth.py -v

# Run integration tests only
pytest tests/integration/ -v
```

### Frontend Tests (Coming Soon)

```bash
cd apps/web

# Run unit tests
pnpm test

# Run with coverage
pnpm test:coverage

# Run E2E tests (Playwright)
pnpm test:e2e
```

---

## 🔒 Security

### Implemented

- ✅ **Password Security:** Bcrypt hashing with salt
- ✅ **JWT Tokens:** 24-hour access, 30-day refresh
- ✅ **API Keys:** Bcrypt hashed, one-time display
- ✅ **Rate Limiting:** 100/min, 1000/hour, 10k/day
- ✅ **Input Validation:** Pydantic schemas
- ✅ **CORS:** Configured origins
- ✅ **SQL Injection:** Parameterized queries (SQLAlchemy)
- ✅ **Multi-tenant Isolation:** Organization-scoped queries

### Planned

- ⏳ **OAuth 2.0:** GitHub, GitLab, Bitbucket (Phase 5)
- ⏳ **Email Verification:** Email confirmation (Phase 6)
- ⏳ **2FA/MFA:** Two-factor authentication (Phase 9)
- ⏳ **Webhook Signatures:** HMAC verification (Phase 6)
- ⏳ **SOC 2:** Compliance audit (Phase 12)

---

## 📋 Common Tasks

### Add New API Endpoint

1. **Create schema** (`apps/api/app/schemas/`)
```python
# app/schemas/example.py
from pydantic import BaseModel

class ExampleCreate(BaseModel):
    name: str
    description: str | None = None

class ExampleResponse(BaseModel):
    id: str
    name: str
    created_at: datetime
```

2. **Create route** (`apps/api/app/api/v1/endpoints/`)
```python
# app/api/v1/endpoints/example.py
from fastapi import APIRouter, Depends
from app.schemas.example import ExampleCreate, ExampleResponse
from app.core.deps import get_current_user

router = APIRouter()

@router.post("/", response_model=ExampleResponse)
async def create_example(
    data: ExampleCreate,
    current_user = Depends(get_current_user)
):
    # Implementation
    pass
```

3. **Register route** (`apps/api/app/api/v1/__init__.py`)
```python
from app.api.v1.endpoints import example

router.include_router(example.router, prefix="/examples", tags=["examples"])
```

### Add New UI Component

1. **Create component** (`apps/web/components/`)
```tsx
// components/example/ExampleCard.tsx
import { Card } from '@/components/ui/card'

interface ExampleCardProps {
  title: string
  description?: string
}

export function ExampleCard({ title, description }: ExampleCardProps) {
  return (
    <Card>
      <h3>{title}</h3>
      {description && <p>{description}</p>}
    </Card>
  )
}
```

2. **Use in page** (`apps/web/app/`)
```tsx
// app/(app)/example/page.tsx
import { ExampleCard } from '@/components/example/ExampleCard'

export default function ExamplePage() {
  return (
    <div>
      <ExampleCard title="Hello" description="World" />
    </div>
  )
}
```

### Create Database Migration

```bash
cd apps/api
source venv/bin/activate

# Edit model in app/models/
# Then generate migration:
alembic revision --autogenerate -m "Add example table"

# Review migration in alembic/versions/
# Apply:
alembic upgrade head
```

---

## 🚨 Troubleshooting

### Docker Issues

**Services won't start:**
```bash
# Check if ports are in use
lsof -i :5434  # PostgreSQL
lsof -i :6381  # Redis
lsof -i :9202  # Elasticsearch

# Kill and restart
docker-compose down
docker-compose up -d
```

**Database connection errors:**
```bash
# Check PostgreSQL logs
docker-compose logs postgres

# Verify service is healthy
docker-compose ps

# Test connection
psql postgresql://repograph:repograph@localhost:5434/repograph_cloud
```

### API Issues

**Module not found:**
```bash
cd apps/api
source venv/bin/activate
pip install -r requirements.txt
```

**Migration errors:**
```bash
# Check migration status
alembic current

# Rollback and retry
alembic downgrade -1
alembic upgrade head
```

**CORS errors:**
- Check `app/core/config.py` CORS settings
- Ensure frontend URL is in `BACKEND_CORS_ORIGINS`

### Frontend Issues

**Build errors:**
```bash
cd apps/web
rm -rf .next node_modules
pnpm install
pnpm dev
```

**API connection errors:**
- Check `.env.local` has correct `NEXT_PUBLIC_API_URL`
- Verify backend is running on correct port
- Check browser console for CORS errors

---

## 🗺️ Roadmap & Priorities

### Phase 5: OAuth Integration (NEXT - 2-3 weeks)
**Priority:** 🔴 CRITICAL (Unlocks everything)

- [ ] GitHub OAuth App setup
- [ ] GitLab OAuth integration
- [ ] Bitbucket OAuth integration
- [ ] OAuth callback handlers
- [ ] Repository discovery UI
- [ ] Webhook setup automation

**Why Critical:** Can't connect real repositories without this

### Phase 6: Repository Indexing (2 weeks)
**Priority:** 🔴 CRITICAL (Core functionality)

- [ ] Celery worker setup
- [ ] SCIP indexer integration (Python, TypeScript, Go, Rust)
- [ ] Repository cloning logic
- [ ] Incremental indexing
- [ ] Progress tracking (WebSocket)
- [ ] Error handling & retries

**Why Critical:** No code analysis without indexing

### Phase 7: Search API (1-2 weeks)
**Priority:** 🟠 HIGH (Core value)

- [ ] Elasticsearch query builder
- [ ] Full-text search implementation
- [ ] Filter support (language, file type, repo)
- [ ] Pagination & sorting
- [ ] Search history API
- [ ] Query performance optimization

**Why Important:** Main use case for users

### Phase 8: Graph Analysis (1-2 weeks)
**Priority:** 🟠 HIGH (Differentiator)

- [ ] Ego graph computation
- [ ] Impact analysis (forward/backward)
- [ ] Call graph generation
- [ ] Dependency tree analysis
- [ ] Cross-repository queries
- [ ] Graph caching

**Why Important:** Unique value proposition

### Phase 9-12: Polish & Launch (4-5 weeks)
**Priority:** 🟡 MEDIUM (Nice-to-have)

- Search UI & visualization
- Advanced search (fuzzy, semantic)
- IDE plugins (VS Code, JetBrains)
- AI integrations (MCP, OpenAI)
- Analytics dashboard
- Billing integration (Stripe)

---

## 📚 Key Documentation

**In this repository:**
- [README.md](README.md) - Project overview
- [getting-started.md](getting-started.md) - Setup guide
- [extraction-checklist.md](extraction-checklist.md) - Independence validation
- [SAAS_ARCHITECTURE.md](../repograph/SAAS_ARCHITECTURE.md) - System architecture
- [planning-production-implementation-plan.md](../repograph/planning-production-implementation-plan.md) - Roadmap

**API Documentation:**
- Swagger UI: http://localhost:8000/docs
- ReDoc: http://localhost:8000/redoc
- OpenAPI JSON: http://localhost:8000/openapi.json

**External Resources:**
- FastAPI: https://fastapi.tiangolo.com
- Next.js 14: https://nextjs.org/docs
- shadcn/ui: https://ui.shadcn.com
- React Query: https://tanstack.com/query

---

## 🤝 Contributing

### Development Workflow

1. **Pick a task** from Phase 5-8 (see Roadmap above)
2. **Create branch:** `git checkout -b feature/oauth-github`
3. **Make changes** following code style
4. **Write tests** (backend + frontend)
5. **Update docs** (API docs, README)
6. **Test locally** (run full test suite)
7. **Create PR** with description

### Code Style

**Backend (Python):**
- Follow PEP 8
- Use type hints
- Write docstrings
- Use `ruff` for linting
- Use `pyright` for type checking

**Frontend (TypeScript):**
- Follow Airbnb style guide
- Use strict TypeScript
- Use ESLint + Prettier
- Prefer functional components
- Use React Query for data fetching

### Commit Messages

```
feat(api): add GitHub OAuth integration
fix(ui): resolve dark mode toggle issue
docs(readme): update setup instructions
test(api): add repository indexing tests
chore(deps): upgrade FastAPI to 0.110
```

---

## 💡 Tips & Best Practices

### Backend Tips

1. **Always use async/await** for database operations
2. **Use Pydantic models** for validation (never raw dicts)
3. **Organization-scope all queries** (multi-tenant isolation)
4. **Add type hints** to all functions
5. **Write integration tests** for API endpoints

### Frontend Tips

1. **Use React Query** for all API calls
2. **Prefer server components** when possible (Next.js 14)
3. **Use shadcn/ui components** (don't reinvent)
4. **Add loading states** (Suspense, skeleton screens)
5. **Implement error boundaries** for robust UX

### Database Tips

1. **Always use migrations** (never manual schema changes)
2. **Index frequently queried columns**
3. **Use UUID** for all primary keys (security)
4. **Add timestamps** (created_at, updated_at)
5. **Test migrations** up AND down

---

## 🎯 Current Focus

**This Week (Oct 4-11):**
- 🔴 Implement GitHub OAuth (Phase 5 - critical)
- 🔴 Set up Celery workers (Phase 6 - critical)
- 🟠 Create basic SCIP indexer (Phase 6)

**Next Week (Oct 11-18):**
- 🟠 Build search API (Phase 7)
- 🟠 Implement ego graph (Phase 8)
- 🟡 Search UI components (Phase 9)

**Goal:** Working search demo by Oct 18 (2 weeks)

---

## 📞 Getting Help

**Questions about:**
- **Architecture:** Read `SAAS_ARCHITECTURE.md`
- **Setup issues:** Check "Troubleshooting" section above
- **API usage:** Visit http://localhost:8000/docs
- **Code patterns:** Look at existing endpoints/components

**Still stuck?**
- Check existing issues in repo
- Ask in team Slack: `#repograph-cloud`
- Create issue with `[question]` tag

---

## ✅ Checklist for New Developers

- [ ] Clone repository
- [ ] Install all dependencies (pnpm, pip)
- [ ] Start Docker services
- [ ] Run database migrations
- [ ] Start backend API (verify http://localhost:8000/docs)
- [ ] Start frontend web (verify http://localhost:3000)
- [ ] Register test user
- [ ] Create test organization
- [ ] Generate API key
- [ ] Read SAAS_ARCHITECTURE.md
- [ ] Review current Phase 5 tasks
- [ ] Pick first task and create branch!

---

## 🚀 Ready to Build!

You now have everything you need to:
- ✅ Understand the project architecture
- ✅ Set up your development environment
- ✅ Navigate the codebase
- ✅ Contribute to critical features
- ✅ Deploy to production (eventually)

**Next Steps:**
1. Complete the checklist above
2. Review Phase 5 tasks (OAuth)
3. Pick a task and start coding!

**Welcome to the team! Let's build something amazing.** 🎉

---

**Last Updated:** October 4, 2025
**Status:** Foundation Complete (25% MVP) - Core Features Next
**Questions?** Ask in #repograph-cloud Slack channel
