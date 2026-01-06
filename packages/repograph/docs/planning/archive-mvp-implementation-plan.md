> **⚠️ ARCHIVED DOCUMENT**
> This document is superseded by [ROADMAP.md](../../ROADMAP.md) (November 2025).
> Kept for historical reference only. Do not use for current planning.

# RepoGraph SaaS - MVP Implementation Plan

**Goal:** Launch functional SaaS in 6-8 weeks

---

## 🎯 MVP Scope (Minimum Viable Product)

### What's IN Scope
✅ User authentication (email/password + GitHub OAuth)
✅ Repository connection (GitHub only)
✅ Automatic indexing (webhook-triggered)
✅ Search API
✅ Ego graph API
✅ Web dashboard (repo management)
✅ API key generation
✅ Basic usage tracking
✅ Stripe billing (Free + Pro tiers)

### What's OUT of Scope (Post-MVP)
❌ GitLab/Bitbucket integration
❌ IDE plugins
❌ Team management
❌ SSO
❌ Advanced analytics
❌ On-premise deployment

---

## 📅 Week-by-Week Plan

### Week 1: Foundation

**Backend:**
- [ ] Set up FastAPI project structure
- [ ] Create multi-tenant database schema
- [ ] Implement authentication (JWT)
- [ ] Create API key management
- [ ] Set up PostgreSQL + Redis (Docker Compose)

**Frontend:**
- [ ] Create Next.js project
- [ ] Set up TailwindCSS + shadcn/ui
- [ ] Build login/signup pages
- [ ] Implement OAuth GitHub flow

**Deliverable:** Users can sign up and log in

---

### Week 2: Repository Connection

**Backend:**
- [ ] GitHub OAuth integration
- [ ] Repository listing (via GitHub API)
- [ ] Repository connection endpoint
- [ ] Webhook registration

**Frontend:**
- [ ] Dashboard layout
- [ ] Connect repository flow
- [ ] Repository list view

**Deliverable:** Users can connect GitHub repositories

---

### Week 3: Indexing Pipeline

**Backend:**
- [ ] Celery worker setup
- [ ] Repository cloning logic
- [ ] SCIP indexer integration (Python + TypeScript)
- [ ] Graph building (nodes + edges)
- [ ] Database insertion (batched)

**Infrastructure:**
- [ ] Set up GCS/S3 bucket
- [ ] Configure Redis queue
- [ ] Deploy Celery workers

**Deliverable:** Repositories are automatically indexed on connection

---

### Week 4: Webhook Processing

**Backend:**
- [ ] GitHub webhook endpoint
- [ ] Signature verification
- [ ] Push event handling
- [ ] Re-indexing trigger
- [ ] Job status tracking

**Frontend:**
- [ ] Repository status display (indexing/active/failed)
- [ ] Indexing logs view
- [ ] Re-index button

**Deliverable:** Repositories auto-update on push

---

### Week 5: Query APIs

**Backend:**
- [ ] Search endpoint (`POST /v1/search`)
- [ ] Ego graph endpoint (`POST /v1/ego`)
- [ ] Impact analysis endpoint (`POST /v1/impact`)
- [ ] Rate limiting (Redis-based)
- [ ] Query caching

**Frontend:**
- [ ] API playground (test queries)
- [ ] Code browser (navigate results)

**Deliverable:** Users can query their repositories

---

### Week 6: API Keys & Usage Tracking

**Backend:**
- [ ] API key generation
- [ ] API key authentication middleware
- [ ] Usage event logging
- [ ] Usage aggregation (daily)

**Frontend:**
- [ ] API keys management page
- [ ] Usage dashboard (charts)
- [ ] API documentation

**Deliverable:** Users can generate API keys and track usage

---

### Week 7: Billing Integration

**Backend:**
- [ ] Stripe integration
- [ ] Plan enforcement (free vs pro)
- [ ] Webhook handling (subscription updates)
- [ ] Overage calculation

**Frontend:**
- [ ] Pricing page
- [ ] Checkout flow (Stripe)
- [ ] Billing management
- [ ] Upgrade/downgrade

**Deliverable:** Users can subscribe to Pro plan

---

### Week 8: Polish & Launch

**Backend:**
- [ ] Error handling improvements
- [ ] Monitoring setup (Cloud Logging)
- [ ] Rate limit tuning
- [ ] Performance optimization

**Frontend:**
- [ ] Onboarding flow
- [ ] Help documentation
- [ ] Marketing landing page

**Infrastructure:**
- [ ] Deploy to GCP (production)
- [ ] Set up Cloud SQL
- [ ] Configure DNS
- [ ] SSL certificates

**Deliverable:** Beta launch to first 50 users

---

## 🛠️ Technology Stack

### Backend
- **Framework:** FastAPI
- **Database:** PostgreSQL 15 (Cloud SQL)
- **Cache/Queue:** Redis (Cloud Memorystore)
- **Task Queue:** Celery
- **Auth:** PyJWT, Authlib
- **Billing:** Stripe Python SDK
- **Indexing:** SCIP (scip-python, scip-typescript)

### Frontend
- **Framework:** Next.js 14 (App Router)
- **Styling:** TailwindCSS + shadcn/ui
- **Auth:** NextAuth.js
- **API Client:** Axios + React Query
- **Charts:** Recharts
- **Payments:** Stripe.js

### Infrastructure
- **Hosting:** Google Cloud Platform
- **Compute:** Cloud Run (containers)
- **Database:** Cloud SQL (PostgreSQL)
- **Cache:** Cloud Memorystore (Redis)
- **Storage:** Cloud Storage
- **CDN:** Cloud CDN
- **Monitoring:** Cloud Logging + Sentry

---

## 💻 Project Structure

```
repograph-saas/
├── backend/                    # FastAPI backend
│   ├── app/
│   │   ├── api/               # API routes
│   │   │   ├── auth.py
│   │   │   ├── repositories.py
│   │   │   ├── search.py
│   │   │   └── webhooks.py
│   │   ├── core/              # Core logic
│   │   │   ├── auth.py
│   │   │   ├── config.py
│   │   │   └── security.py
│   │   ├── models/            # SQLAlchemy models
│   │   │   ├── user.py
│   │   │   ├── organization.py
│   │   │   ├── repository.py
│   │   │   └── code_graph.py
│   │   ├── services/          # Business logic
│   │   │   ├── github.py
│   │   │   ├── indexer.py
│   │   │   └── billing.py
│   │   ├── workers/           # Celery tasks
│   │   │   ├── index.py
│   │   │   └── webhooks.py
│   │   └── main.py            # FastAPI app
│   ├── alembic/               # Database migrations
│   ├── requirements.txt
│   └── Dockerfile
│
├── frontend/                   # Next.js frontend
│   ├── app/
│   │   ├── (auth)/
│   │   │   ├── login/
│   │   │   └── signup/
│   │   ├── (dashboard)/
│   │   │   ├── repositories/
│   │   │   ├── api-keys/
│   │   │   ├── usage/
│   │   │   └── billing/
│   │   ├── api/               # Next.js API routes
│   │   └── layout.tsx
│   ├── components/
│   │   ├── ui/                # shadcn/ui components
│   │   ├── dashboard/
│   │   └── auth/
│   ├── lib/
│   │   ├── api.ts             # API client
│   │   └── utils.ts
│   ├── package.json
│   └── Dockerfile
│
├── ops/                        # Infrastructure
│   ├── docker/compose.dev.yml
│   ├── docker/compose.prod.yml
│   ├── cloudbuild.yaml        # GCP deployment
│   └── terraform/             # Infrastructure as code
│
└── docs/                       # Documentation
    ├── api/                   # API reference (OpenAPI)
    ├── guides/                # User guides
    └── development/           # Dev setup
```

---

## 🔧 Local Development Setup

### Prerequisites
- Docker Desktop
- Node.js 20+
- Python 3.11+
- PostgreSQL client
- Redis client

### Initial Setup

```bash
# Clone repository
git clone https://github.com/yourusername/repograph-saas.git
cd repograph-saas

# Backend setup
cd backend
python -m venv .venv
source .venv/bin/activate
pip install -r requirements.txt

# Create .env file
cat > .env <<EOF
DATABASE_URL=postgresql://repograph:repograph@localhost:5432/repograph
REDIS_URL=redis://localhost:6379/0
JWT_SECRET_KEY=$(openssl rand -hex 32)
GITHUB_CLIENT_ID=your-github-oauth-app-id
GITHUB_CLIENT_SECRET=your-github-oauth-app-secret
STRIPE_SECRET_KEY=sk_test_...
STRIPE_WEBHOOK_SECRET=whsec_...
EOF

# Start dependencies
docker-compose -f ops/docker/compose.dev.yml up -d

# Run migrations
alembic upgrade head

# Start backend
uvicorn app.main:app --reload

# Frontend setup (new terminal)
cd frontend
npm install

# Create .env.local
cat > .env.local <<EOF
NEXT_PUBLIC_API_URL=http://localhost:8000
NEXTAUTH_SECRET=$(openssl rand -hex 32)
GITHUB_CLIENT_ID=your-github-oauth-app-id
GITHUB_CLIENT_SECRET=your-github-oauth-app-secret
EOF

# Start frontend
npm run dev

# Start Celery worker (new terminal)
cd backend
celery -A app.workers worker --loglevel=info
```

**Access:**
- Frontend: http://localhost:3000
- Backend API: http://localhost:8000
- API Docs: http://localhost:8000/docs

---

## 🚀 Deployment (GCP)

### Step 1: Set Up GCP Project

```bash
# Create project
gcloud projects create repograph-prod --name="RepoGraph Production"
gcloud config set project repograph-prod

# Enable APIs
gcloud services enable \
  run.googleapis.com \
  sql-component.googleapis.com \
  redis.googleapis.com \
  storage-component.googleapis.com \
  cloudscheduler.googleapis.com
```

### Step 2: Create Database

```bash
# Create Cloud SQL instance
gcloud sql instances create repograph-db \
  --database-version=POSTGRES_15 \
  --tier=db-f1-micro \
  --region=us-central1

# Create database
gcloud sql databases create repograph --instance=repograph-db

# Set password
gcloud sql users set-password postgres \
  --instance=repograph-db \
  --password=$(openssl rand -hex 16)
```

### Step 3: Create Redis

```bash
# Create Memorystore instance
gcloud redis instances create repograph-cache \
  --size=1 \
  --region=us-central1 \
  --redis-version=redis_7_0
```

### Step 4: Deploy Backend

```bash
# Build and push image
gcloud builds submit --tag gcr.io/repograph-prod/backend backend/

# Deploy to Cloud Run
gcloud run deploy repograph-api \
  --image gcr.io/repograph-prod/backend \
  --platform managed \
  --region us-central1 \
  --allow-unauthenticated \
  --set-env-vars DATABASE_URL=$DATABASE_URL,REDIS_URL=$REDIS_URL
```

### Step 5: Deploy Frontend

```bash
# Build and push
gcloud builds submit --tag gcr.io/repograph-prod/frontend frontend/

# Deploy
gcloud run deploy repograph-app \
  --image gcr.io/repograph-prod/frontend \
  --platform managed \
  --region us-central1 \
  --allow-unauthenticated
```

### Step 6: Configure DNS

```bash
# Map custom domain
gcloud run domain-mappings create \
  --service repograph-api \
  --domain api.repograph.io \
  --region us-central1

gcloud run domain-mappings create \
  --service repograph-app \
  --domain app.repograph.io \
  --region us-central1
```

---

## 📊 Monitoring Setup

### Cloud Logging

```bash
# View API logs
gcloud logging read "resource.type=cloud_run_revision AND resource.labels.service_name=repograph-api" --limit 50

# Error logs only
gcloud logging read "resource.type=cloud_run_revision AND severity>=ERROR" --limit 50
```

### Sentry

```python
# backend/app/main.py
import sentry_sdk

sentry_sdk.init(
    dsn=os.getenv("SENTRY_DSN"),
    traces_sample_rate=0.1,
    environment="production"
)
```

### Uptime Monitoring

```bash
# Create uptime check
gcloud monitoring uptime-checks create api-health \
  --resource-type=uptime-url \
  --host=api.repograph.io \
  --path=/health \
  --period=60
```

---

## 📈 Success Metrics (First 90 Days)

### Week 1-2 (Foundation)
- [ ] 50 beta signups
- [ ] 10 active users
- [ ] 20 repositories connected

### Week 3-4 (Growth)
- [ ] 200 signups
- [ ] 50 active users
- [ ] 100 repositories indexed

### Week 5-6 (Monetization)
- [ ] 500 signups
- [ ] 150 active users
- [ ] 10 paying customers ($290 MRR)

### Week 7-8 (Scale)
- [ ] 1000 signups
- [ ] 300 active users
- [ ] 50 paying customers ($1,450 MRR)

### Technical Goals
- [ ] 99.5% uptime
- [ ] <300ms API latency (p95)
- [ ] <1% error rate
- [ ] <5min indexing time (average repo)

---

## 🎯 Launch Checklist

### Pre-Launch
- [ ] Security audit (OWASP Top 10)
- [ ] Load testing (100 concurrent users)
- [ ] Backup strategy implemented
- [ ] Monitoring & alerts configured
- [ ] Terms of Service & Privacy Policy
- [ ] Pricing page finalized
- [ ] Documentation complete

### Launch Day
- [ ] Deploy to production
- [ ] Verify all integrations
- [ ] Post on Hacker News
- [ ] Post on Reddit (r/programming)
- [ ] Tweet announcement
- [ ] Email beta waitlist

### Post-Launch (Week 1)
- [ ] Monitor error rates
- [ ] Fix critical bugs
- [ ] Respond to feedback
- [ ] Write launch retrospective
- [ ] Plan next features

---

## 💰 Budget (First 3 Months)

**Development Costs:**
- Developer time (640 hours × $100/hr): $64,000
- Design (40 hours × $100/hr): $4,000

**Infrastructure:**
- GCP credits (startup program): -$1,000
- Cloud Run: $50/mo × 3 = $150
- Cloud SQL: $20/mo × 3 = $60
- Redis: $30/mo × 3 = $90
- Storage: $10/mo × 3 = $30
- Total infrastructure: $330

**Services:**
- Domain registration: $20
- SSL certificates: Free (Let's Encrypt)
- Sentry: $26/mo × 3 = $78
- Total services: $98

**Marketing:**
- Product Hunt promotion: $500
- Reddit ads: $500
- Total marketing: $1,000

**Grand Total: ~$69,428**

**Break-even analysis:**
- Need 200 Pro customers ($29/mo) = $5,800 MRR
- Break even in ~12 months (assuming $5,800 revenue - $800 monthly costs)

---

## 🚦 Risk Assessment

### Technical Risks

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| GitHub API rate limits | High | Medium | Implement caching, request queuing |
| Large repo indexing timeout | Medium | High | Background processing, chunking |
| Database performance | Medium | High | Read replicas, connection pooling |
| Cold start latency | Low | Low | Cloud Run min instances |

### Business Risks

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Low adoption | Medium | High | Strong marketing, free tier |
| High churn | Medium | Medium | Excellent onboarding, support |
| Competitor launches | Low | Medium | First-mover advantage, unique features |
| Privacy concerns | Low | High | Clear terms, EU data residency option |

---

## 📞 Support Plan

**MVP Support Channels:**
- Email: support@repograph.io (response within 24 hours)
- Discord: Community support
- Documentation: docs.repograph.io

**Post-MVP:**
- Live chat (Intercom)
- Dedicated Slack channel (Enterprise)
- Video calls (Enterprise)

---

**Ready to start? Begin with Week 1 tasks!**

---

**Last Updated:** 2025-10-02
**Status:** Planning → In Progress
**Next Milestone:** Foundation (Week 1)
