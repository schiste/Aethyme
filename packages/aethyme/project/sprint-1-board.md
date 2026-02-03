# Sprint 1: Foundation & Core Infrastructure

## 📊 Sprint Overview
- **Start:** 2025-11-22
- **End:** 2025-12-03 (11 days)
- **Goal:** Auth, Indexing, Queries production-ready

## 📋 Tasks

### In Progress
(none - awaiting task assignment)

### To Do
- [ ] **S1-T1:** Auth & RLS Hardening (Owner: TBD, Est: 3-4d human | 1-2d AI)
  - Enforce scoped auth and tenant isolation
  - OIDC + JWT with org/repo/read/write scopes
  - RLS policies on all tables with isolation tests
  - Rate limits and API keys for CI/bots
  - **Prereqs:** DB schema stable
  - **Status:** Partial

- [ ] **S1-T2:** Indexing Reliability (Owner: TBD, Est: 3-5d human | 2-3d AI)
  - Reliable indexing with fallbacks and freshness monitoring
  - Validate SCIP + fallback on 5-10 real repos
  - Retry/backoff with language guardrails
  - Freshness monitor and re-index triggers
  - **Prereqs:** Indexer binaries installed
  - **Status:** Missing

- [ ] **S1-T3:** Query Performance (Owner: TBD, Est: 3-4d human | 2d AI)
  - Fast, tested query endpoints (search/ego/impact)
  - Contract tests with fixtures
  - p95 target <2s with caching
  - Staleness invalidation
  - **Prereqs:** Reliable index data
  - **Status:** Missing

### Blocked
(none)

### Done
(none)

## 🎯 Sprint Goals

### Primary Deliverables
1. **Authentication & Security**
   - ✅ Multi-tenant RLS enforced
   - ✅ Scoped JWT tokens working
   - ✅ Rate limiting active
   - ✅ Isolation tests passing

2. **Indexing Pipeline**
   - ✅ Index <2min for medium repos
   - ✅ SCIP + fallback validated on 10 repos
   - ✅ Freshness monitoring active
   - ✅ Failure rate <5%

3. **Query Service**
   - ✅ Search/ego/impact APIs live
   - ✅ p95 latency <2s
   - ✅ Cache hit rate >60%
   - ✅ Contract tests green

### Success Metrics
- **Performance:** Query p95 <2s, Index <2min
- **Reliability:** Uptime >99%, Error rate <1%
- **Security:** RLS coverage 100%, Auth tests pass
- **Quality:** Test coverage >80%, All CI checks green

## 📈 Metrics

### Sprint Progress
- **Velocity:** 0/11 days completed (0%)
- **Tasks:** 0/3 complete (0%)
- **Story Points:** 0/10 completed

### Quality Metrics
- **Tests:** 0 passing
- **Coverage:** 0%
- **CI Status:** Not configured
- **Blockers:** 0 active

### Performance Baselines
- **Index Time:** Not measured
- **Query p95:** Not measured
- **Auth Success Rate:** Not measured
- **Cache Hit Rate:** Not measured

## 🚨 Risks & Mitigations

| Risk | Impact | Probability | Mitigation |
|------|--------|-------------|------------|
| RLS complexity delays auth | High | Medium | Start with simple policies, iterate |
| SCIP indexer failures | High | Medium | Fallback parser ready, test matrix prepared |
| Cache invalidation bugs | Medium | Medium | Conservative TTLs, monitoring alerts |
| Performance targets missed | High | Low | Profile early, optimize hot paths |

## 📝 Daily Standup Notes

### 2025-11-22 (Day 1)
- Sprint kickoff
- Infrastructure setup in progress
- No blockers yet

## 🔗 Related Resources
- [Stage 1 Roadmap Tracker](./STAGE_1_ROADMAP_TRACKER.md)
- [Task Template](./templates/task-template.md)
- [ROADMAP.md](../ROADMAP.md)

## 📞 Team Contacts
- **Product Owner:** TBD
- **Scrum Master:** TBD
- **Tech Lead:** TBD
- **DevOps Lead:** TBD
