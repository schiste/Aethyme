> **⚠️ ARCHIVED DOCUMENT**
> This document is superseded by [ROADMAP.md](../../ROADMAP.md) (November 2025) and [docs/architecture/](../architecture/).
> Kept for historical reference only. Do not use for current planning.

# RepoGraph SaaS Architecture

**Vision:** "GitHub for Code Intelligence" - A hosted service for queryable code knowledge graphs

---

## 🎯 Product Overview

RepoGraph SaaS allows developers and teams to:
- Connect repositories (GitHub/GitLab/Bitbucket)
- Automatic indexing on every push
- Query via REST API, GraphQL, or IDE plugins
- AI assistant integration (MCP, OpenAI, Anthropic)
- Usage analytics and billing

---

## 🏗️ Architecture

### System Components

```
┌─────────────────────────────────────────────────────────────┐
│                    app.repograph.io                          │
│                    Web Dashboard (Next.js)                   │
│  - Authentication (OAuth, Email/Password)                    │
│  - Repository management                                     │
│  - API key generation                                        │
│  - Usage analytics & billing                                 │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                    api.repograph.io                          │
│                    API Gateway (FastAPI)                     │
│  - Multi-tenant isolation (RLS)                              │
│  - Rate limiting (Redis-based)                               │
│  - Authentication (JWT + API keys)                           │
│  - Usage tracking                                            │
├─────────────┬─────────────────┬─────────────────────────────┤
│  Search API │  Ego Graph API  │  Impact Analysis API        │
│  /search    │  /ego           │  /impact                    │
└─────────────┴─────────────────┴─────────────────────────────┘
                              │
        ┌─────────────────────┴─────────────────────┐
        │                                           │
┌───────▼────────┐                     ┌────────────▼──────────┐
│  PostgreSQL    │                     │  Redis (Cache+Queue)  │
│  Multi-tenant  │                     │  - Query cache        │
│  + RLS         │                     │  - Celery queue       │
└────────────────┘                     └───────────────────────┘
        │
        │ Async indexing
        ▼
┌─────────────────────────────────────────────────────────────┐
│                    Indexing Workers (Celery)                 │
│  - Clone repository                                          │
│  - Run SCIP indexers (Python, TypeScript, Go, Rust)         │
│  - Parse AST + build graph                                   │
│  - Upload to database                                        │
└─────────────────────────────────────────────────────────────┘
        │
        ▼
┌─────────────────────────────────────────────────────────────┐
│                    Cloud Storage (GCS/S3)                    │
│  - Repository snapshots                                      │
│  - Indexing artifacts                                        │
└─────────────────────────────────────────────────────────────┘
```

---

## 📊 Multi-Tenant Data Model

### Core Tables

#### Organizations
```sql
CREATE TABLE organizations (
    id UUID PRIMARY KEY,
    name TEXT NOT NULL,
    slug TEXT UNIQUE NOT NULL,
    plan TEXT NOT NULL DEFAULT 'free',
    stripe_customer_id TEXT,
    created_at TIMESTAMPTZ DEFAULT NOW()
);
```

#### Users & Memberships
```sql
CREATE TABLE users (
    id UUID PRIMARY KEY,
    email TEXT UNIQUE NOT NULL,
    name TEXT,
    avatar_url TEXT,
    github_id TEXT,
    gitlab_id TEXT
);

CREATE TABLE org_members (
    org_id UUID REFERENCES organizations(id),
    user_id UUID REFERENCES users(id),
    role TEXT NOT NULL, -- owner, admin, member
    PRIMARY KEY (org_id, user_id)
);
```

#### Repositories
```sql
CREATE TABLE repositories (
    id UUID PRIMARY KEY,
    org_id UUID NOT NULL REFERENCES organizations(id),
    name TEXT NOT NULL,
    git_url TEXT,
    provider TEXT, -- github, gitlab, bitbucket
    default_branch TEXT DEFAULT 'main',
    last_indexed_at TIMESTAMPTZ,
    last_indexed_commit TEXT,
    status TEXT NOT NULL DEFAULT 'pending',
    languages TEXT[],
    UNIQUE (org_id, name)
);

-- Row-Level Security
ALTER TABLE repositories ENABLE ROW LEVEL SECURITY;

CREATE POLICY repo_access ON repositories
    USING (org_id IN (
        SELECT org_id FROM org_members
        WHERE user_id = current_setting('app.user_id')::uuid
    ));
```

#### Code Graph
```sql
CREATE TABLE code_nodes (
    id UUID PRIMARY KEY,
    repo_id UUID NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    qualified_name TEXT NOT NULL, -- file.py:ClassName.method
    kind TEXT NOT NULL, -- class, function, method, variable
    file_path TEXT NOT NULL,
    line_start INT,
    line_end INT,
    signature TEXT,
    docstring TEXT,
    metadata JSONB
);

CREATE INDEX idx_nodes_search ON code_nodes
    USING gin(to_tsvector('english', name || ' ' || qualified_name));

CREATE TABLE code_edges (
    id UUID PRIMARY KEY,
    repo_id UUID NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    source_id UUID REFERENCES code_nodes(id) ON DELETE CASCADE,
    target_id UUID REFERENCES code_nodes(id) ON DELETE CASCADE,
    relationship TEXT NOT NULL -- calls, imports, inherits, implements
);
```

#### API Keys & Usage
```sql
CREATE TABLE api_keys (
    id UUID PRIMARY KEY,
    org_id UUID NOT NULL REFERENCES organizations(id),
    name TEXT NOT NULL,
    key_hash TEXT NOT NULL UNIQUE,
    last_used_at TIMESTAMPTZ,
    expires_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ
);

CREATE TABLE usage_events (
    id BIGSERIAL PRIMARY KEY,
    org_id UUID NOT NULL REFERENCES organizations(id),
    event_type TEXT NOT NULL, -- search, ego, impact, index
    repo_id UUID REFERENCES repositories(id),
    timestamp TIMESTAMPTZ DEFAULT NOW(),
    metadata JSONB
);

CREATE INDEX idx_usage_billing ON usage_events(org_id, timestamp DESC);
```

---

## 🔐 Authentication

### JWT Tokens (User Sessions)

**Current Implementation (Week 2):**
```json
{
  "sub": "user-uuid",
  "type": "access",
  "exp": 1735689600
}
```

**Planned Enhancements (Week 3+):**
```json
{
  "sub": "user-uuid",
  "email": "user@company.com",
  "org_id": "org-uuid",
  "org_role": "admin",
  "plan": "pro",
  "type": "access",
  "exp": 1735689600
}
```

**Refresh Tokens (30-day expiration):**
```json
{
  "sub": "user-uuid",
  "type": "refresh",
  "exp": 1738281600
}
```

### API Keys (Programmatic Access)
```
Format: rgph_{env}_{random}
Example: rgph_live_abc123def456ghi789jkl012mno345

Environments:
- rgph_test_*  (sandbox, free usage)
- rgph_live_*  (production, metered)
```

**Storage:** SHA256 hash in database
**Validation:** Constant-time comparison
**Rate Limiting:** Redis-based (per key)

---

## 💳 Pricing Plans

| Plan | Price | Repositories | Queries/Month | Features |
|------|-------|--------------|---------------|----------|
| **Free** | $0 | 3 public | 1,000 | Basic search, 7-day retention |
| **Pro** | $29/mo | 20 private | 50,000 | Full API, 90-day retention, CI webhooks |
| **Team** | $99/mo | 100 private | 500,000 | Multiple orgs, SSO, priority support |
| **Enterprise** | Custom | Unlimited | Unlimited | On-premise, SLA, custom integrations |

### Overage Pricing
- $0.01 per 1,000 additional queries
- $2 per additional repository

---

## 🚀 Infrastructure (Google Cloud Platform)

### Production Stack

```yaml
Frontend:
  Service: Cloud Run (Next.js container)
  Domain: app.repograph.io
  CDN: Cloud CDN
  SSL: Managed certificates

API:
  Service: Cloud Run (FastAPI)
  Domain: api.repograph.io
  Scaling: 1-100 instances
  CPU: 2 vCPU, 4GB RAM per instance

Database:
  Service: Cloud SQL (PostgreSQL 15)
  Tier: db-n1-standard-2 (2 vCPU, 7.5GB RAM)
  HA: Primary + standby replica
  Backups: Automated daily, 30-day retention
  Connection: Cloud SQL Proxy + PgBouncer

Cache & Queue:
  Service: Cloud Memorystore (Redis 7)
  Tier: Standard (3GB)
  Purpose: Query cache, Celery task queue

Workers:
  Service: Cloud Run Jobs (Celery workers)
  Concurrency: 10 workers
  Tasks: Repository cloning, indexing, graph building

Storage:
  Service: Cloud Storage (Standard)
  Buckets:
    - repograph-repos (repository snapshots)
    - repograph-artifacts (indexing outputs)
  Lifecycle: 30-day retention

Monitoring:
  - Cloud Logging
  - Cloud Monitoring (Prometheus-compatible)
  - Cloud Trace (distributed tracing)
  - Sentry (error tracking)
  - Uptime checks (1-minute intervals)

Security:
  - Cloud Armor (DDoS protection, WAF)
  - Secret Manager (API keys, DB passwords)
  - VPC (private networking)
  - IAM (least privilege)
```

### Cost Estimation

**Startup (0-100 users, <50 repos):**
- Cloud Run (API + Frontend): $20-50/mo
- Cloud SQL (db-f1-micro): $15/mo
- Cloud Memorystore: $30/mo
- Cloud Storage: $5-10/mo
- Networking: $10/mo
- **Total: ~$80-120/mo**

**Growth (100-1000 users, 100-500 repos):**
- Cloud Run: $100-300/mo
- Cloud SQL (db-n1-standard-2): $150/mo
- Redis: $100/mo
- Storage: $50-100/mo
- Workers: $50-150/mo
- **Total: ~$500-800/mo**

**Scale (1000+ users, 1000+ repos):**
- Cloud Run: $500-2000/mo
- Cloud SQL (HA): $400/mo
- Redis: $200/mo
- Storage: $200-500/mo
- Workers: $300-1000/mo
- **Total: ~$2000-4000/mo**

---

## 🛠️ Development Roadmap

### Phase 1: MVP (6-8 weeks)

**Week 1-2: Foundation** ✅ **COMPLETE**
- [x] Multi-tenant database schema (User, Organization, Repository models)
- [x] Authentication system (JWT + refresh tokens)
- [x] Password hashing (bcrypt)
- [x] API endpoints (auth, users, organizations)
- [ ] API key management (planned for Week 3)
- [ ] Basic web dashboard (Next.js) (planned for Week 4)

**Week 3-4: Core Features**
- [ ] Repository connection (GitHub OAuth)
- [ ] Webhook handler (push events)
- [ ] Indexing worker (Celery + SCIP)
- [ ] Search API endpoint

**Week 5-6: Graph APIs**
- [ ] Ego graph endpoint
- [ ] Impact analysis endpoint
- [ ] Rate limiting & caching
- [ ] Usage tracking

**Week 7-8: Polish & Launch**
- [ ] Billing integration (Stripe)
- [ ] Documentation & examples
- [ ] CI/CD pipeline
- [ ] Beta launch

### Phase 2: Growth Features (8-12 weeks)

- [ ] GitLab integration
- [ ] Bitbucket integration
- [ ] IDE plugins (VS Code, JetBrains)
- [ ] AI assistant integrations (MCP server)
- [ ] Usage analytics dashboard
- [ ] Team management
- [ ] SSO (SAML, OIDC)

### Phase 3: Enterprise (12+ weeks)

- [ ] On-premise deployment option
- [ ] Advanced RBAC
- [ ] Audit logging
- [ ] Custom retention policies
- [ ] SLA guarantees
- [ ] Dedicated support

---

## 🔌 API Examples

### Search for Symbols

```bash
curl -X POST https://api.repograph.io/v1/search \
  -H "Authorization: Bearer rgph_live_abc123..." \
  -H "Content-Type: application/json" \
  -d '{
    "query": "UserProfile",
    "repo": "myorg/myrepo",
    "limit": 10
  }'
```

**Response:**
```json
{
  "results": [
    {
      "name": "UserProfile",
      "qualified_name": "models/user.py:UserProfile",
      "kind": "class",
      "file_path": "src/models/user.py",
      "line_start": 42,
      "signature": "class UserProfile(Model)"
    }
  ],
  "usage": {
    "quota_remaining": 49999,
    "reset_at": "2025-11-01T00:00:00Z"
  }
}
```

### Get Ego Graph

```bash
curl -X POST https://api.repograph.io/v1/ego \
  -H "Authorization: Bearer rgph_live_abc123..." \
  -d '{
    "symbol": "models/user.py:UserProfile",
    "repo": "myorg/myrepo",
    "depth": 2
  }'
```

### Webhook Setup (GitHub)

```bash
# RepoGraph automatically creates webhook on repository connection
POST https://api.github.com/repos/myorg/myrepo/hooks
{
  "name": "web",
  "active": true,
  "events": ["push"],
  "config": {
    "url": "https://api.repograph.io/webhooks/github",
    "content_type": "json",
    "secret": "webhook-secret"
  }
}
```

---

## 🧪 Testing Strategy

### Unit Tests
- Model validation (Pydantic)
- Graph algorithms (ego, impact)
- Authentication logic

### Integration Tests
- API endpoints with test database
- Webhook processing
- Indexing pipeline

### E2E Tests
- Repository connection flow
- Search → result → navigation
- Billing workflow

### Load Tests
- 1000 concurrent searches
- Large repository indexing (100k files)
- Rate limit enforcement

---

## 🚦 Monitoring & Observability

### Key Metrics

**Business Metrics:**
- Sign-ups per day
- Active organizations
- Repositories indexed
- Queries per day
- Revenue (MRR, ARR)

**Technical Metrics:**
- API latency (p50, p95, p99)
- Error rate (4xx, 5xx)
- Database connections
- Cache hit rate
- Indexing queue depth
- Worker utilization

**Alerts:**
- API latency > 1s (p95)
- Error rate > 1%
- Database CPU > 80%
- Queue depth > 1000
- Failed indexing jobs > 10%

### Dashboards

**Grafana Dashboards:**
1. **Overview:** Sign-ups, revenue, active users
2. **API Performance:** Latency, throughput, errors
3. **Infrastructure:** CPU, memory, disk, network
4. **Indexing:** Queue size, job duration, success rate

---

## 🔒 Security

### Best Practices

- [ ] All traffic over HTTPS (TLS 1.3)
- [ ] API keys hashed with bcrypt
- [ ] JWT tokens expire after 24 hours
- [ ] Rate limiting (100 req/min per API key)
- [ ] Input validation (Pydantic)
- [ ] SQL injection prevention (parameterized queries)
- [ ] CORS restrictions
- [ ] DDoS protection (Cloud Armor)
- [ ] Regular security audits
- [ ] Dependency scanning (Dependabot)

### Compliance

- GDPR (EU data residency option)
- SOC 2 Type II (planned)
- Data encryption at rest
- 30-day data retention policy
- Right to deletion

---

## 📦 Deployment

### CI/CD Pipeline (GitHub Actions)

```yaml
name: Deploy to Production

on:
  push:
    branches: [main]

jobs:
  deploy:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3

      - name: Build API
        run: docker build -t gcr.io/repograph/api:${{ github.sha }} .

      - name: Push to GCR
        run: docker push gcr.io/repograph/api:${{ github.sha }}

      - name: Deploy to Cloud Run
        run: |
          gcloud run deploy repograph-api \
            --image gcr.io/repograph/api:${{ github.sha }} \
            --platform managed \
            --region us-central1 \
            --set-env-vars DATABASE_URL=${{ secrets.DATABASE_URL }}

      - name: Run migrations
        run: |
          gcloud run jobs execute repograph-migrate \
            --region us-central1
```

---

## 📚 Documentation

### User Documentation
- Quickstart guide
- API reference (OpenAPI)
- IDE plugin setup
- AI assistant integration
- Billing FAQ

### Developer Documentation
- Architecture overview (this doc)
- Database schema
- API design patterns
- Contribution guide
- Local development setup

---

## 🎯 Success Metrics (Year 1)

**Growth:**
- 1,000 registered users
- 100 paying customers
- 5,000 repositories indexed
- $5,000 MRR

**Product:**
- 99.9% uptime
- <200ms API latency (p95)
- 100M queries processed
- 50+ integrations shipped

**Community:**
- 100 GitHub stars
- 10 open-source contributors
- 1,000 Discord members

---

## 🚀 Go-to-Market Strategy

### Launch Phases

**Private Beta (Month 1-2):**
- 50 hand-picked developers
- Free access
- Feedback collection

**Public Beta (Month 3-4):**
- Open sign-ups
- Free tier + paid plans
- Product Hunt launch

**General Availability (Month 5+):**
- Full feature set
- SLA guarantees
- Enterprise sales

### Marketing Channels

- Developer communities (Reddit, Hacker News)
- Technical blog posts
- Conference talks
- YouTube tutorials
- Partnership with IDE vendors

---

## 📞 Support

- Email: support@repograph.io
- Discord: discord.gg/repograph
- Documentation: docs.repograph.io
- Status page: status.repograph.io

---

## 📊 Current Implementation Status

**Phase 1 Progress:** Week 2 of 8 Complete (25%)

**✅ Implemented (Week 1-2):**
- FastAPI application structure
- PostgreSQL multi-tenant schema with RLS
- Alembic migrations
- JWT authentication (access + refresh tokens)
- User registration & login
- Password hashing (bcrypt)
- Organization creation (automatic on user signup)
- Protected API endpoints with dependencies
- Health check endpoints
- Pydantic v2 validation
- Row-level security policies
- Auto-generated OpenAPI docs

**🚧 In Progress (Week 3-4):**
- Repository management endpoints
- GitHub/GitLab/Bitbucket OAuth
- API key management
- Webhook handlers
- Next.js dashboard
- Repository indexing worker

**📅 Upcoming (Week 5+):**
- Search API
- Ego graph API
- Impact analysis API
- Rate limiting
- Usage tracking
- Billing integration (Stripe)

---

**Last Updated:** 2025-10-02 (Week 2 Complete)
**Version:** 1.1
**Status:** Active Development (Week 3 Starting)
