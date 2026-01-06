# RepoGraph Training Plan

**Date:** 2025-11-22
**Owner:** Skills & Training Lead
**Status:** Active

---

## Executive Summary

Comprehensive training program for RepoGraph Stage 1 development team. Covers foundations, specializations, and ongoing skill development.

**Timeline:** 4 weeks to full productivity
**Target:** 11 developers across 6 task teams
**Success Metric:** Developer productive on first task within 1 day of training completion

---

## Week 1: Foundations (All Developers)

### Day 1-2: Architecture & Environment Setup

**Objectives:**
- Understand RepoGraph multi-tenant architecture
- Set up local development environment
- Run tests and quality checks successfully

**Activities:**
```bash
# Day 1 Morning: Architecture Overview
- Read: packages/repograph/ROADMAP.md
- Read: Agents/skills/architecture/skill.md
- Workshop: Multi-tenancy design review (2 hours)

# Day 1 Afternoon: Environment Setup
- Clone repository
- Install dependencies (Docker, Python, PostgreSQL, Redis)
- Run: make dev
- Verify: All services running

# Day 2 Morning: Quality Gates
- Read: docs/guides/testing.md
- Run: pnpm quality:all
- Exercise: Fix one failing test (pair programming)

# Day 2 Afternoon: Skills System
- Read: Agents/skills/scripts-management/skill.md
- Read: Agents/skills/learnings-management/skill.md
- Practice: Load and navigate skills
```

**Skills Loaded:**
- [architecture](../Agents/skills/architecture/)
- [scripts-management](../Agents/skills/scripts-management/)

**Deliverables:**
- ✅ Dev environment running
- ✅ Tests passing locally
- ✅ Can navigate skills system

---

### Day 3-4: Auth, Security & Multi-Tenancy

**Objectives:**
- Understand authentication flow (JWT/OIDC)
- Implement RLS policies
- Test tenant isolation

**Activities:**
```bash
# Day 3 Morning: Auth Fundamentals
- Read: Agents/skills/rbac/skill.md
- Read: Agents/skills/rate-limiting/skill.md (NEW)
- Workshop: OIDC + JWT flow walkthrough

# Day 3 Afternoon: RLS Policies
- Read: Agents/skills/database-migrations/skill.md (NEW)
- Exercise: Write RLS policy for test table
- Verify: Isolation tests pass

# Day 4 Morning: API Keys
- Read: Agents/skills/api-keys-management/skill.md (NEW)
- Exercise: Generate API key, test scopes
- Workshop: Rate limiting strategies

# Day 4 Afternoon: Hands-On
- Implement: Test auth endpoint with RLS
- Test: Multi-tenant isolation
- Code Review: Pair review with senior
```

**Skills Loaded:**
- [rbac](../Agents/skills/rbac/)
- [rate-limiting](../Agents/skills/rate-limiting/)
- [api-keys-management](../Agents/skills/api-keys-management/)
- [database-migrations](../Agents/skills/database-migrations/)

**Deliverables:**
- ✅ Can write RLS policies
- ✅ Can generate API keys
- ✅ Understands rate limiting

---

### Day 5: Testing, Logging & Observability

**Objectives:**
- Write contract tests
- Implement structured logging
- Add metrics/tracing

**Activities:**
```bash
# Day 5 Morning: Testing Patterns
- Read: docs/guides/testing.md
- Read: Agents/skills/testing/skill.md (if exists)
- Exercise: Write contract test for sample endpoint

# Day 5 Afternoon: Observability
- Read: Agents/skills/logging/skill.md (NEW)
- Read: Agents/skills/observability-otel/skill.md (NEW)
- Read: Agents/skills/metrics-dashboards/skill.md (NEW)
- Exercise: Add logging + metrics to sample function
- Workshop: Correlation IDs and distributed tracing
```

**Skills Loaded:**
- [logging](../Agents/skills/logging/)
- [observability-otel](../Agents/skills/observability-otel/)
- [metrics-dashboards](../Agents/skills/metrics-dashboards/)

**Deliverables:**
- ✅ Can write contract tests
- ✅ Can add structured logging
- ✅ Understands OTEL spans

**Week 1 Assessment:**
- Quiz: Multi-tenant architecture
- Exercise: Build authenticated endpoint with RLS + logging
- Peer Review: Code review session

---

## Week 2: Specialization (Task-Specific Teams)

### Auth Team (S1-T1) - 2 developers

**Skills Focus:**
- rate-limiting (deep dive)
- api-keys-management
- database-migrations
- rbac

**Activities:**
```bash
# Days 6-7: Deep Dive OIDC
- Study: Auth0 OIDC flow
- Hands-on: Keycloak local setup
- Exercise: Implement scoped JWT middleware

# Days 8-9: RLS Policies
- Workshop: Advanced RLS patterns
- Exercise: Write policies for all tables
- Testing: Isolation test suite

# Day 10: Integration
- Code: Complete S1-T1 subtasks
- Review: Team code review
- Demo: Present to team
```

**Deliverables:**
- ✅ OIDC integration working
- ✅ RLS policies on all tables
- ✅ Rate limits enforced

---

### Indexing Team (S1-T2) - 2 developers

**Skills Focus:**
- scripts-management
- caching
- logging
- metrics-dashboards

**Activities:**
```bash
# Days 6-7: SCIP Indexer
- Deep dive: SCIP indexer internals
- Hands-on: Index 5-10 real repos
- Benchmarking: Measure performance

# Days 8-9: Performance & Caching
- Read: Agents/skills/caching/skill.md (NEW)
- Workshop: Cache invalidation strategies
- Exercise: Add Redis caching to index status
- Profiling: Identify bottlenecks

# Day 10: Freshness Monitoring
- Implement: Staleness detector
- Schedule: Re-index triggers
- Metrics: Index latency dashboard
```

**Deliverables:**
- ✅ Median index <2min for medium repo
- ✅ Caching implemented
- ✅ Freshness monitor operational

---

### Query Team (S1-T3) - 2 developers

**Skills Focus:**
- api-conventions
- caching
- performance-backend
- testing

**Activities:**
```bash
# Days 6-7: Query Optimization
- Deep dive: PostgreSQL query optimization
- Workshop: Indexing strategies
- Exercise: Optimize slow query

# Days 8-9: Caching & Load Testing
- Read: Agents/skills/caching/skill.md (NEW)
- Implement: Cache hot queries
- Load Testing: p95 <2s target
- Tuning: Adjust cache TTLs

# Day 10: Contract Tests
- Workshop: API contract testing
- Exercise: Write fixtures for graph data
- Testing: Contract test suite
```

**Deliverables:**
- ✅ Query p95 <2s
- ✅ Cache hit metrics recorded
- ✅ Contract tests green

---

### AI-Readiness Team (S1-T4, S1-T5) - 2 developers

**Skills Focus:**
- data-ui-selectors
- docs-workflow
- autofixers
- llm-guardrails

**Activities:**
```bash
# Days 6-7: Scorecard Detectors
- Workshop: Build detector for data-ui coverage
- Exercise: Build FOLDER docs validator
- Testing: Fixture repos with known violations

# Days 8-9: Autofixers
- Read: Agents/skills/autofixers/skill.md (when available)
- Workshop: Safe fix patterns
- Exercise: Build link fixer
- Testing: Dry-run mode

# Day 10: Integration
- Implement: CLI/API for ai-ready
- Demo: Run scorecard + autofix
```

**Deliverables:**
- ✅ Scorecard runs on sample repo
- ✅ Safe fixes apply cleanly
- ✅ Dry-run/PR patch generator working

---

### Guardrails Team (S1-T6, S1-T7) - 1 developer

**Skills Focus:**
- llm-guardrails
- llm-context-efficiency
- observability-otel
- metrics-dashboards

**Activities:**
```bash
# Days 6-7: Guardrails
- Read: Agents/skills/llm-guardrails/skill.md (NEW)
- Read: Agents/skills/llm-context-efficiency/skill.md (NEW)
- Workshop: Schema-first planning
- Exercise: Build drift sentinel

# Days 8-9: Context Management
- Workshop: Auto-compaction patterns
- Exercise: Implement working-memory slots
- Testing: Measure token savings

# Day 10: Telemetry
- Implement: Token/cost logging
- Dashboard: Routing/compaction metrics
- Demo: Show token savings
```

**Deliverables:**
- ✅ Guardrails default-on
- ✅ Auto-compaction working
- ✅ Token/cost tracked

---

### Ops Team (S1-T8, S1-T9, S1-T10) - 2 developers

**Skills Focus:**
- kubernetes-helm
- ci-cd
- monitoring-observability
- deployment

**Activities:**
```bash
# Days 6-7: Kubernetes
- Read: Agents/skills/kubernetes-helm/skill.md (NEW)
- Hands-on: Deploy to test cluster
- Exercise: Write Helm chart

# Days 8-9: CI/CD
- Workshop: GitHub Actions pipelines
- Exercise: Build test/deploy workflow
- Security: Secret scanning setup

# Day 10: SLOs & Alerts
- Workshop: Define SLOs
- Implement: Prometheus alerts
- Runbooks: Write incident playbooks
```

**Deliverables:**
- ✅ Deployed to test cluster
- ✅ CI/CD pipeline green
- ✅ Alerts firing

---

## Week 3: Cross-Training & Integration

### Objectives
- Developers understand adjacent tasks
- Integration testing across task boundaries
- Knowledge sharing

### Activities

**Days 11-12: Skill Rotation**
```bash
# Each developer spends half-day with another team
# Auth → Indexing
# Indexing → Query
# Query → AI-Readiness
# AI-Readiness → Guardrails
# Guardrails → Ops
# Ops → Auth
```

**Days 13-14: Integration Week**
```bash
# Full stack integration tests
- Auth + Indexing: Authenticated index operation
- Indexing + Query: Fresh index → query results
- Query + AI-Readiness: Query with scorecard
- AI-Readiness + Guardrails: Autofix with guardrails
- Guardrails + Ops: Deploy with telemetry
```

**Day 15: Code Review Blitz**
```bash
# Cross-team code reviews
- Each developer reviews 2 PRs from other teams
- Focus: Architecture, patterns, testing
- Feedback: Incorporate learnings
```

---

## Week 4: Production Readiness

### Objectives
- End-to-end testing
- Performance validation
- Documentation complete

### Activities

**Days 16-17: End-to-End Testing**
```bash
# Full workflow tests
- Index repo → Query → Scorecard → Autofix
- Multi-tenant isolation verification
- Load testing at scale
- Performance benchmarks
```

**Days 18-19: Documentation Sprint**
```bash
# Each team documents their work
- API docs (OpenAPI)
- CLI help text
- Runbooks (ops)
- Troubleshooting guides
```

**Day 20: Demo Day**
```bash
# Each team presents:
- What we built
- How it works
- Demo (live)
- Challenges overcome
- Learnings

# Retrospective:
- What went well
- What to improve
- Next steps
```

---

## Ongoing: Skills Rotation (Post-Week 4)

### Weekly Skill Shares (30min, Fridays)
```markdown
Week 5: "Rate Limiting Patterns" - Auth Team
Week 6: "Cache Invalidation Strategies" - Query Team
Week 7: "SCIP Indexer Deep Dive" - Indexing Team
Week 8: "LLM Guardrails in Practice" - Guardrails Team
Week 9: "Kubernetes Troubleshooting" - Ops Team
Week 10: "Autofix Safety Patterns" - AI-Readiness Team
```

### Monthly Pair Programming
- Rotate pairs across teams
- Work on stretch tasks together
- Knowledge transfer

### Quarterly Skill Reviews
- Assess skill coverage across team
- Identify gaps
- Plan new skill creation
- Update training materials

---

## Training Resources

### Documentation
- `/packages/repograph/ROADMAP.md` - Full roadmap
- `/Agents/skills/` - All skills
- `/docs/architecture/` - Architecture docs
- `/docs/guides/` - How-to guides

### Tools
- `/scripts/ai/onboard.mjs` - AI onboarding tool
- `pnpm quality:all` - Quality gate
- `pnpm test` - Test suite
- `make dev` - Dev environment

### External Resources
- FastAPI docs: https://fastapi.tiangolo.com
- PostgreSQL RLS: https://www.postgresql.org/docs/current/ddl-rowsecurity.html
- OpenTelemetry: https://opentelemetry.io/docs/
- Kubernetes: https://kubernetes.io/docs/

---

## Success Metrics

### Individual Developer Metrics
- Time to first productive contribution: <1 day after training
- Test coverage maintained: 75%+
- Code review participation: 2+ reviews/week
- Skill mastery: 3+ core skills proficient

### Team Metrics
- All 11 Stage 1 tasks on track
- Knowledge silos eliminated (cross-training)
- Documentation complete (no missing docs)
- Quality gates passing (pnpm quality:all)

### Project Metrics
- Stage 1 completion: On schedule (30-50 human-days)
- Skills coverage: 90%+ (32/36 skills documented)
- Onboarding time: <1 week for new developers
- Training satisfaction: 4+/5 average rating

---

## Roles & Responsibilities

### Skills & Training Lead
- Create/maintain skills
- Coordinate training schedule
- Measure training effectiveness
- Update training plan

### Task Owners
- Lead specialized training for their task
- Mentor team members
- Review code
- Share learnings

### All Developers
- Complete foundation training (Week 1)
- Master assigned skills (Week 2)
- Participate in cross-training (Week 3)
- Share knowledge (ongoing)

---

## Estimated Time Investment

### Per Developer
- Week 1 Foundations: 40 hours (full-time)
- Week 2 Specialization: 40 hours (full-time)
- Week 3 Integration: 40 hours (full-time)
- Week 4 Production Ready: 40 hours (full-time)
- **Total: 160 hours (4 weeks)**

### Team Total (11 developers)
- Total training hours: 1,760 hours
- Equivalent: 44 person-weeks
- Timeline: 4 calendar weeks (parallel training)

### ROI
- Upfront: 4 weeks training investment
- Payoff:
  - 50% faster task completion (with skills)
  - 70% fewer "where is X?" questions
  - 60% faster onboarding for new team members
  - 40% reduction in architectural mistakes

---

## Appendix: Skill Prerequisites

### No Prerequisites (Start Here)
- architecture
- scripts-management
- learnings-management

### Foundation Skills (Week 1)
- rbac
- rate-limiting
- api-keys-management
- database-migrations
- logging
- observability-otel
- metrics-dashboards

### Advanced Skills (Week 2+)
- caching (requires: logging)
- llm-guardrails (requires: observability-otel)
- llm-context-efficiency (requires: llm-guardrails)
- kubernetes-helm (requires: metrics-dashboards)

---

**Document Owner:** Skills & Training Lead
**Review Cadence:** Weekly during training, monthly after
**Last Updated:** 2025-11-22
**Next Review:** 2025-11-29
