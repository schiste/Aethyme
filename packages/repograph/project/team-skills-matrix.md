# Team Skills Matrix

**Date:** 2025-11-22
**Owner:** Skills & Training Lead
**Status:** Planning

---

## Team Composition

**Total Developers Needed:** 11
**Current Team:** TBD
**Skills Coverage Target:** 90%

---

## Skills Matrix

| Developer | Role | Current Skills | Learning (Week 2) | Assigned Tasks | Week 1 Complete | Week 2 Complete |
|-----------|------|----------------|-------------------|----------------|-----------------|-----------------|
| **Developer 1** | Backend Lead | Python, FastAPI, PostgreSQL | Auth, RLS, Rate Limiting | S1-T1 (Auth & RLS) | ☐ | ☐ |
| **Developer 2** | Backend | Python, API Design | OIDC, API Keys | S1-T1 (Auth & RLS) | ☐ | ☐ |
| **Developer 3** | Backend | Python, Databases | Indexing, SCIP, Caching | S1-T2 (Indexing) | ☐ | ☐ |
| **Developer 4** | Backend | Python, Performance | Metrics, Logging | S1-T2 (Indexing) | ☐ | ☐ |
| **Developer 5** | Backend | PostgreSQL, Query Optimization | Caching, Testing | S1-T3 (Queries) | ☐ | ☐ |
| **Developer 6** | Backend | Python, Redis | API Conventions, Performance | S1-T3 (Queries) | ☐ | ☐ |
| **Developer 7** | Full Stack | Python, React, TypeScript | Data-UI, Docs Workflow | S1-T4 (Scorecard) | ☐ | ☐ |
| **Developer 8** | Backend/AI | Python, LLM Integration | Autofixers, Guardrails | S1-T5 (Autofixers) | ☐ | ☐ |
| **Developer 9** | AI/Backend | Python, ML/AI | LLM Guardrails, Context Efficiency | S1-T6, S1-T7 (Guardrails & Telemetry) | ☐ | ☐ |
| **Developer 10** | DevOps | Docker, Kubernetes | Helm, CI/CD, Monitoring | S1-T8 (Ops) | ☐ | ☐ |
| **Developer 11** | DevOps/Backend | Docker, Python | Deployment, Security, Docs | S1-T9, S1-T10 (Surfaces & Docs) | ☐ | ☐ |

---

## Skill Coverage by Developer

### Developer 1 - Backend Lead (S1-T1)
**Current Skills:**
- ✅ Python, FastAPI
- ✅ PostgreSQL
- ✅ API Design

**Learning Path:**
1. Week 1: Foundation skills
2. Week 2: Deep dive auth (OIDC, JWT, RLS)
3. Week 3: Rate limiting, API keys
4. Week 4: Code review, mentoring

**Skill Mastery Targets:**
- 🎯 authentication (expert)
- 🎯 rbac (expert)
- 🎯 rate-limiting (proficient)
- 🎯 api-keys-management (proficient)
- 🎯 database-migrations (proficient)

**Mentoring:** Leads auth team, mentors Developer 2

---

### Developer 2 - Backend (S1-T1)
**Current Skills:**
- ✅ Python
- ✅ API Design
- ⚠️ Limited auth experience

**Learning Path:**
1. Week 1: Foundation + extra auth workshop
2. Week 2: OIDC integration, API key management
3. Week 3: Pair with Developer 1 on RLS
4. Week 4: Independent implementation

**Skill Mastery Targets:**
- 🎯 authentication (proficient)
- 🎯 api-keys-management (proficient)
- 🎯 rate-limiting (competent)

---

### Developer 3 - Backend (S1-T2)
**Current Skills:**
- ✅ Python
- ✅ Databases
- ⚠️ Limited indexing experience

**Learning Path:**
1. Week 1: Foundation
2. Week 2: SCIP indexer deep dive, caching patterns
3. Week 3: Performance optimization
4. Week 4: Freshness monitoring

**Skill Mastery Targets:**
- 🎯 scripts-management (proficient)
- 🎯 caching (proficient)
- 🎯 logging (proficient)
- 🎯 metrics-dashboards (competent)

**Mentoring:** Leads indexing team with Developer 4

---

### Developer 4 - Backend (S1-T2)
**Current Skills:**
- ✅ Python
- ✅ Performance tuning
- ⚠️ Limited observability experience

**Learning Path:**
1. Week 1: Foundation + extra observability workshop
2. Week 2: Metrics, logging, OTEL
3. Week 3: Performance benchmarking
4. Week 4: Dashboard creation

**Skill Mastery Targets:**
- 🎯 metrics-dashboards (proficient)
- 🎯 logging (proficient)
- 🎯 observability-otel (competent)
- 🎯 performance-backend (expert)

---

### Developer 5 - Backend (S1-T3)
**Current Skills:**
- ✅ PostgreSQL (expert)
- ✅ Query optimization
- ⚠️ Limited caching experience

**Learning Path:**
1. Week 1: Foundation
2. Week 2: Redis caching strategies
3. Week 3: Load testing, p95 optimization
4. Week 4: Contract testing

**Skill Mastery Targets:**
- 🎯 caching (expert)
- 🎯 performance-backend (expert)
- 🎯 testing (proficient)

**Mentoring:** Query optimization expert

---

### Developer 6 - Backend (S1-T3)
**Current Skills:**
- ✅ Python
- ✅ Redis
- ⚠️ Limited API design experience

**Learning Path:**
1. Week 1: Foundation + API conventions
2. Week 2: FastAPI patterns, contract testing
3. Week 3: Pair with Developer 5 on caching
4. Week 4: Integration testing

**Skill Mastery Targets:**
- 🎯 api-conventions (proficient)
- 🎯 caching (proficient)
- 🎯 testing (proficient)

---

### Developer 7 - Full Stack (S1-T4, S1-T5)
**Current Skills:**
- ✅ Python, React, TypeScript
- ✅ Full-stack development
- ⚠️ Limited data-ui experience

**Learning Path:**
1. Week 1: Foundation
2. Week 2: Data-UI selectors, docs workflow, i18n
3. Week 3: Scorecard detectors
4. Week 4: Autofix integration

**Skill Mastery Targets:**
- 🎯 data-ui-selectors (proficient)
- 🎯 docs-workflow (proficient)
- 🎯 i18n-workflow (competent)
- 🎯 autofixers (competent)

**Special:** Bridge between backend and frontend

---

### Developer 8 - Backend/AI (S1-T5)
**Current Skills:**
- ✅ Python
- ✅ LLM integration
- ⚠️ Limited autofix experience

**Learning Path:**
1. Week 1: Foundation
2. Week 2: Safe autofix patterns, patch generation
3. Week 3: Pair with Developer 7 on scorecard integration
4. Week 4: Dry-run/PR modes

**Skill Mastery Targets:**
- 🎯 autofixers (expert)
- 🎯 patch-generation (proficient)
- 🎯 llm-guardrails (competent)

---

### Developer 9 - AI/Backend (S1-T6, S1-T7)
**Current Skills:**
- ✅ Python
- ✅ ML/AI background
- ⚠️ Limited LLM production experience

**Learning Path:**
1. Week 1: Foundation
2. Week 2: LLM guardrails, context efficiency
3. Week 3: Token/cost tracking, telemetry
4. Week 4: Model routing, evals

**Skill Mastery Targets:**
- 🎯 llm-guardrails (expert)
- 🎯 llm-context-efficiency (expert)
- 🎯 observability-otel (proficient)
- 🎯 metrics-dashboards (proficient)

**Special:** AI/LLM subject matter expert

---

### Developer 10 - DevOps (S1-T8)
**Current Skills:**
- ✅ Docker, Kubernetes
- ✅ CI/CD pipelines
- ⚠️ Limited Helm experience

**Learning Path:**
1. Week 1: Foundation + backend basics
2. Week 2: Helm charts, K8s manifests
3. Week 3: CI/CD, security scanning
4. Week 4: SLOs, alerts, runbooks

**Skill Mastery Targets:**
- 🎯 kubernetes-helm (expert)
- 🎯 ci-cd (expert)
- 🎯 monitoring-observability (proficient)
- 🎯 deployment (proficient)

**Mentoring:** DevOps lead, mentors Developer 11

---

### Developer 11 - DevOps/Backend (S1-T9, S1-T10)
**Current Skills:**
- ✅ Docker
- ✅ Python
- ⚠️ Limited K8s experience

**Learning Path:**
1. Week 1: Foundation
2. Week 2: Deployment strategies, security
3. Week 3: Docs workflow, CLI development
4. Week 4: Runbooks, knowledge base

**Skill Mastery Targets:**
- 🎯 deployment (proficient)
- 🎯 security (proficient)
- 🎯 docs-workflow (proficient)
- 🎯 ci-cd (competent)

**Special:** Technical writing skills

---

## Skill Proficiency Levels

| Level | Description | Time to Achieve |
|-------|-------------|-----------------|
| **Expert** | Can teach others, solve complex problems, design patterns | 6+ months |
| **Proficient** | Can work independently, handle most scenarios | 2-3 months |
| **Competent** | Can complete tasks with occasional guidance | 2-4 weeks |
| **Learning** | Actively studying, requires supervision | 1-2 weeks |

---

## Team Skill Development Paths

### Junior → Mid (0-2 years experience)
- Master 3 core skills (proficient level)
- Complete 2+ tasks in Stage 1
- Participate in code reviews (2/week)
- Present 1 skill share

**Timeline:** 3-6 months

### Mid → Senior (2-5 years experience)
- Expert in 2 core skills
- Lead 1 major task (S1-T1 to S1-T11)
- Mentor 2 junior developers
- Create 1+ new skills
- Design architecture patterns

**Timeline:** 6-12 months

### Senior → Staff (5+ years experience)
- Expert in 4+ skills
- Lead Stage 1 or Stage 2
- Design system architecture
- Review all critical code
- Define engineering standards

**Timeline:** 12+ months

---

## Cross-Team Skill Sharing

### Scheduled Rotations (Week 3)

| From Team | To Team | Focus Area | Duration |
|-----------|---------|------------|----------|
| Auth → Indexing | Learn indexing internals | 4 hours |
| Indexing → Query | Learn query optimization | 4 hours |
| Query → AI-Readiness | Learn scorecard detectors | 4 hours |
| AI-Readiness → Guardrails | Learn LLM safety | 4 hours |
| Guardrails → Ops | Learn deployment | 4 hours |
| Ops → Auth | Learn auth flow | 4 hours |

### Benefits:
- Eliminate knowledge silos
- Enable cross-team collaboration
- Build T-shaped skill profiles
- Improve system understanding

---

## Skill Gap Analysis

### Current Gaps (Before Training)
| Skill Category | Team Coverage | Gap |
|----------------|---------------|-----|
| **Backend** | 70% (Python, APIs) | 30% (RLS, migrations) |
| **Auth** | 30% (Basic auth) | 70% (OIDC, RLS, rate limiting) |
| **Caching** | 40% (Redis basics) | 60% (Advanced patterns) |
| **AI/LLM** | 50% (LLM usage) | 50% (Guardrails, efficiency) |
| **Ops** | 60% (Docker, CI/CD) | 40% (Helm, K8s) |
| **Observability** | 30% (Basic logging) | 70% (OTEL, metrics) |

### After Training (Week 4)
| Skill Category | Team Coverage | Gap |
|----------------|---------------|-----|
| **Backend** | 95% | 5% |
| **Auth** | 90% | 10% |
| **Caching** | 85% | 15% |
| **AI/LLM** | 80% | 20% |
| **Ops** | 90% | 10% |
| **Observability** | 85% | 15% |

---

## Hiring Recommendations

### Immediate Needs (if gaps persist)
- **Senior AI/LLM Engineer** - For S1-T6 (Guardrails)
  - Skills: LLM production, context optimization, guardrails
  - Impact: Critical for AI-readiness features

- **Senior DevOps Engineer** - For S1-T8 (Ops)
  - Skills: Kubernetes, Helm, production ops
  - Impact: Accelerates deployment readiness

### Future Needs (Stage 2)
- **Frontend Engineers** (2-3) - For Stage 2 UI
- **Product Manager** - For feature prioritization
- **Technical Writer** - For documentation

---

## Success Metrics

### Individual Developer
- ✅ Complete Week 1 foundation training
- ✅ Master 3+ skills (competent level minimum)
- ✅ Complete assigned Stage 1 task
- ✅ Participate in 5+ code reviews
- ✅ Present 1 skill share

### Team
- ✅ 90% skill coverage across all tasks
- ✅ All 11 Stage 1 tasks staffed
- ✅ Zero knowledge silos (cross-training complete)
- ✅ Code review participation: 100%
- ✅ Quality gates passing: 95%+

---

## Skills Tracking

### Weekly Check-ins
- Review skill mastery progress
- Identify blockers
- Adjust training plan
- Celebrate wins

### Monthly Reviews
- Assess skill coverage
- Identify new skills needed
- Update skill documents
- Plan next quarter training

---

**Document Owner:** Skills & Training Lead
**Review Cadence:** Weekly during Stage 1
**Last Updated:** 2025-11-22
**Next Review:** 2025-11-29
