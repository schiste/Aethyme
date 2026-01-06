# RepoGraph Cloud - SaaS Platform

**AI-powered code intelligence for modern development teams**

🌐 **Production:** https://app.repograph.io (Coming Nov 2025)
📚 **Documentation:** https://docs.repograph.io
🔧 **API:** https://api.repograph.io

---

## 🎯 Overview

RepoGraph Cloud turns your repositories into a queryable knowledge graph, enabling:

- 🔍 **Instant Code Search** - Find anything across all repositories in <100ms
- 🤖 **AI Context Layer** - Claude/Copilot/Cursor with full codebase knowledge
- 📊 **Graph Analysis** - Understand dependencies, call hierarchies, impact
- 💻 **IDE Integration** - Search from VS Code, JetBrains, Vim
- 🔐 **Enterprise-Ready** - SSO, RBAC, audit logs, self-hosted option

**Status:** Phase 11/12 complete (90% of MVP) - [See Current Progress](STATUS.md)

---

## 📋 Key Documents

### Essential
- **[Status](STATUS.md)** - Current progress and completion status
- **[Roadmap](roadmap.md)** - Product roadmap and timeline
- **[Getting Started](docs/guides/getting-started.md)** - Setup and development guide

### API Documentation
- **[OAuth API Reference](docs/OAUTH_API_REFERENCE.md)** - OAuth endpoints documentation
- **[AI Features API](docs/API_AI_FEATURES.md)** - AI features API documentation

### Historical Documents
- **[Planning Docs](docs/planning/)** - Roadmaps, feature specs, competitive analysis
- **[Status Reports](docs/status-reports/)** - Historical status updates and audits
- **[Session Summaries](docs/sessions/)** - Development session reports

---

## 🏗️ Architecture

```
repograph-cloud/
├── apps/
│   ├── api/              # FastAPI backend
│   ├── web/              # Next.js dashboard
│   └── workers/          # Celery background workers
│
├── packages/
│   ├── database/         # Shared database models & migrations
│   ├── auth/             # Authentication utilities
│   ├── indexer/          # Code indexing logic
│   └── ui/               # Shared UI components
│
├── infrastructure/
│   ├── docker/           # Docker configurations
│   ├── terraform/        # Infrastructure as Code (GCP)
│   └── kubernetes/       # K8s manifests
│
└── docs/
    ├── api/              # API reference
    ├── guides/           # User guides
    └── architecture/     # System design docs
```

---

## 🚀 Quick Start

### Prerequisites

- Docker Desktop
- Node.js 20+
- Python 3.11+
- pnpm 8+

### Local Development

```bash
# 1. Install dependencies
pnpm install

# 2. Start infrastructure (PostgreSQL, Redis, Elasticsearch)
docker-compose up -d

# 3. Run database migrations
cd apps/api
alembic upgrade head

# 4. Start development servers
pnpm dev

# API: http://localhost:8000
# Web: http://localhost:3000
# API Docs: http://localhost:8000/docs
```

---

## 📦 Apps

### API (`apps/api`)

FastAPI backend providing GraphQL and REST APIs.

**Tech Stack:**
- FastAPI (async ASGI)
- PostgreSQL 15 (multi-tenant)
- Redis (cache + queue)
- Elasticsearch (search)
- Celery (background jobs)

**Run:**
```bash
cd apps/api
uvicorn main:app --reload
```

### Web (`apps/web`)

Next.js 14 dashboard for repository management.

**Tech Stack:**
- Next.js 14 (App Router)
- React 18
- TailwindCSS + shadcn/ui
- React Query
- Zustand

**Run:**
```bash
cd apps/web
pnpm dev
```

### Workers (`apps/workers`)

Celery workers for repository indexing and background tasks.

**Run:**
```bash
cd apps/workers
celery -A tasks worker --loglevel=info
```

---

## 🧪 Testing

```bash
# Run all tests
pnpm test

# Backend tests
cd apps/api
pytest

# Frontend tests
cd apps/web
pnpm test

# E2E tests
pnpm test:e2e
```

---

## 🚢 Deployment

### Production (GCP)

```bash
# Deploy via CI/CD (GitHub Actions)
git tag v1.0.0
git push --tags

# Or manually:
./scripts/deploy.sh production
```

**Infrastructure:**
- Cloud Run (API + Web)
- Cloud SQL (PostgreSQL)
- Cloud Memorystore (Redis)
- GKE (Celery workers)

---

## 📚 Documentation

- **[API Reference](./docs/api/)** - OpenAPI/GraphQL schema
- **[Architecture](./docs/architecture/)** - System design
- **[Development Guide](./docs/guides/development.md)** - Contributing
- **[Deployment Guide](./docs/guides/deployment.md)** - Production setup

---

## 🔒 Security

- Multi-tenant with Row-Level Security (RLS)
- JWT authentication + API keys
- Rate limiting
- CORS protection
- SOC 2 Type II compliant

**Report vulnerabilities:** security@repograph.io

---

## 🤝 Contributing

This is a private repository. See [CONTRIBUTING.md](./CONTRIBUTING.md) for team guidelines.

---

## 📄 License

Proprietary - All rights reserved

---

## 🆘 Support

- **Email:** support@repograph.io
- **Discord:** https://discord.gg/repograph
- **Status:** https://status.repograph.io

---

## 🗺️ Roadmap

See [roadmap.md](roadmap.md) for planned features and timeline.

---

**Built with ❤️ for developers**
