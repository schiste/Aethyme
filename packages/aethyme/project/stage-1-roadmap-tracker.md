# Stage 1 Roadmap Tracker

**Last Updated:** 2025-11-22
**Stage Goal:** Ship CLI/service-only Aethyme with RLS, auth, observability, AI-readiness scorecard, safe autofixers, and ops readiness.

## 📊 Overall Progress

| Metric | Value | Target | Status |
|--------|-------|--------|--------|
| Tasks Complete | 0/11 | 11 | 🔴 Not Started |
| Sprint 1 Tasks | 0/3 | 3 | 🔴 Not Started |
| Test Coverage | 0% | 80% | 🔴 Below Target |
| Performance (Query p95) | N/A | <2s | ⚪ Not Measured |
| Performance (Index) | N/A | <2min | ⚪ Not Measured |
| Security (RLS Coverage) | 0% | 100% | 🔴 Not Started |

## 🗓️ Task Breakdown

### Sprint 1: Foundation (Current)

#### S1-T1: Auth & RLS Hardening
- **Owner:** TBD
- **Estimate:** 3-4d human | 1-2d AI
- **Status:** 🟡 Partial
- **Dependencies:** DB schema stable
- **Progress:**
  - [ ] OIDC + scoped JWT implementation
  - [ ] RLS policies on all tables
  - [ ] Isolation fixtures and tests
  - [ ] Rate limiting middleware
  - [ ] API keys for CI/bots
- **Risks:**
  - RLS policy complexity may require iteration
  - OIDC provider configuration dependencies
- **Blockers:** None
- **Artifacts:** Auth/RLS docs, test suite

#### S1-T2: Indexing Reliability
- **Owner:** TBD
- **Estimate:** 3-5d human | 2-3d AI
- **Status:** 🔴 Missing
- **Dependencies:** Indexer binaries installed
- **Progress:**
  - [ ] Validate SCIP + fallback on 5-10 repos
  - [ ] Language guardrails, retry/backoff
  - [ ] Freshness monitor + re-index triggers
  - [ ] Metrics logging
  - [ ] Index status endpoint
- **Risks:**
  - SCIP parser failures on edge cases
  - Performance targets may need tuning
- **Blockers:** None
- **Artifacts:** Index performance report, freshness endpoint

#### S1-T3: Query Performance
- **Owner:** TBD
- **Estimate:** 3-4d human | 2d AI
- **Status:** 🔴 Missing
- **Dependencies:** Reliable index data (S1-T2)
- **Progress:**
  - [ ] Contract tests with fixtures
  - [ ] p95 target <2s implementation
  - [ ] Cache hot queries
  - [ ] Staleness invalidation
  - [ ] Latency metrics
- **Risks:**
  - Cache invalidation complexity
  - Query optimization may need database indexes
- **Blockers:** Waiting on S1-T2 completion
- **Artifacts:** Contract tests, performance report

### Sprint 2+: AI-Readiness & Autofixers

#### S1-T4: AI-Readiness Scorecard
- **Owner:** TBD
- **Estimate:** 4-6d human | 3-4d AI
- **Status:** 🔴 Missing
- **Dependencies:** Repo scanners, schema/routes access
- **Progress:**
  - [ ] Detectors (data-ui, FOLDER docs, links, i18n, etc.)
  - [ ] JSON/MD outputs with severities
  - [ ] CLI/API `ai-ready` command
  - [ ] Evidence linking
- **Risks:** Detector accuracy may need tuning
- **Blockers:** None
- **Artifacts:** Scorecard schema, sample outputs

#### S1-T5: Autofixers (Safe)
- **Owner:** TBD
- **Estimate:** 4-6d human | 3-4d AI
- **Status:** 🔴 Missing
- **Dependencies:** Scorecard detectors (S1-T4)
- **Progress:**
  - [ ] Doc regen, link fixes, data-ui insertion, i18n stubs
  - [ ] Generated-file skiplist
  - [ ] Dry-run/PR patch generation
  - [ ] Approval gate for risky scopes
  - [ ] Rollback/disable flags
- **Risks:** Autofix safety requires careful testing
- **Blockers:** Waiting on S1-T4
- **Artifacts:** Patch generator, GitHub Action sample

#### S1-T6: Guardrails & Efficiency
- **Owner:** TBD
- **Estimate:** 4-6d human | 3-4d AI
- **Status:** 🔴 Missing
- **Dependencies:** Schema accessible via graph
- **Progress:**
  - [ ] Schema-first skeleton/gate
  - [ ] Drift sentinels preflight
  - [ ] Compaction/slots/playlists/outcome cards
  - [ ] Model routing with budgets
  - [ ] Token/cost logging
- **Risks:** Model routing may need provider-specific tuning
- **Blockers:** None
- **Artifacts:** Guardrail config docs, log samples

#### S1-T7: Telemetry & Evals
- **Owner:** TBD
- **Estimate:** 3-5d human | 2-3d AI
- **Status:** 🔴 Missing
- **Dependencies:** Core endpoints instrumented
- **Progress:**
  - [ ] Emit tokens/latency/cost/violations/fixes/cache hits
  - [ ] Retrieval eval set
  - [ ] Autofix correctness harness
  - [ ] Performance benchmarks
  - [ ] KPI CSV/CLI reports
- **Risks:** Eval dataset quality critical for accuracy
- **Blockers:** None
- **Artifacts:** Eval suites, KPI exports

### Sprint 3+: Ops & Developer Surfaces

#### S1-T8: Ops & Reliability
- **Owner:** TBD
- **Estimate:** 4-6d human | 3-4d AI
- **Status:** 🟡 Partial
- **Dependencies:** Service configs
- **Progress:**
  - [ ] K8s manifests with readiness/liveness
  - [ ] Blue-green/canary scripts
  - [ ] Backups/restore for Postgres/Redis
  - [ ] DR runbook
  - [ ] SLOs/alerts
  - [ ] CI/CD pipeline with tests/evals/security
- **Risks:** K8s complexity, DR testing required
- **Blockers:** None
- **Artifacts:** Manifests, runbooks, CI/CD config

#### S1-T9: Developer/Consumer Surfaces
- **Owner:** TBD
- **Estimate:** 3-5d human | 2-3d AI
- **Status:** 🟡 Partial
- **Dependencies:** Core APIs stable
- **Progress:**
  - [ ] CLI commands (index, query, ai-ready, autofix)
  - [ ] API endpoints (status, search, scorecard, etc.)
  - [ ] GitHub Action for scorecard + patches
  - [ ] Scoped tokens/access controls
  - [ ] Audit logging for autofix
- **Risks:** GitHub Action marketplace submission process
- **Blockers:** None
- **Artifacts:** CLI help, API schema, GitHub Action example

#### S1-T10: Docs & Runbooks
- **Owner:** TBD
- **Estimate:** 2-3d human | 1-2d AI
- **Status:** 🟡 Partial
- **Dependencies:** Features landed
- **Progress:**
  - [ ] API contract and CLI reference
  - [ ] Runbooks (index failures, staleness, rollback, backup/restore)
  - [ ] Security/privacy notes
  - [ ] Link validation
- **Risks:** Documentation drift if not maintained
- **Blockers:** None
- **Artifacts:** Docs set, runbooks

#### S1-T11: Agent-Enablement Parity & Ingestion
- **Owner:** TBD
- **Estimate:** 4-6d human | 3-4d AI
- **Status:** 🔴 Missing
- **Dependencies:** Scorecard + autofix scaffolding
- **Progress:**
  - [ ] Model enforced invariants
  - [ ] Detect gaps in connected repos
  - [ ] Emit autofix patches
  - [ ] Export minimal context packs
  - [ ] Staleness monitors for detectors
- **Risks:** Invariant enforcement may conflict with existing patterns
- **Blockers:** None
- **Artifacts:** Parity report, context pack generator

## 🔗 Dependency Graph

```
S1-T1 (Auth & RLS) ──┐
                     ├──> S1-T2 (Indexing) ──> S1-T3 (Queries)
                     │                            │
                     │                            v
                     │                         S1-T4 (Scorecard) ──> S1-T5 (Autofixers)
                     │                            │                      │
                     │                            v                      v
                     ├──> S1-T6 (Guardrails) ────┴──> S1-T7 (Telemetry) ┴──> S1-T8 (Ops)
                     │                                                           │
                     └──> S1-T9 (Dev Surfaces) <────────────────────────────────┘
                              │
                              v
                          S1-T10 (Docs) <──── S1-T11 (Agent Parity)
```

## 🚨 Risk Register

| Risk ID | Risk | Impact | Probability | Owner | Mitigation | Status |
|---------|------|--------|-------------|-------|------------|--------|
| R1 | RLS policy bugs allow cross-tenant data leaks | Critical | Medium | TBD | Comprehensive isolation tests, security review | Open |
| R2 | SCIP indexer fails on large repos | High | Medium | TBD | Fallback parser, incremental indexing | Open |
| R3 | Query performance degrades with graph size | High | Medium | TBD | Caching strategy, database indexes | Open |
| R4 | Autofix introduces bugs | High | Low | TBD | Dry-run default, approval workflow | Open |
| R5 | Model API costs exceed budget | Medium | Medium | TBD | Model routing, token budgets | Open |
| R6 | K8s deployment complexity delays launch | Medium | Low | TBD | Docker Compose fallback, staged rollout | Open |
| R7 | Documentation becomes outdated | Low | High | TBD | Automated link checks, review cycle | Open |

## 📈 Key Performance Indicators

| KPI | Current | Target | Trend | Notes |
|-----|---------|--------|-------|-------|
| Auth Success Rate | N/A | >99% | - | Not measured yet |
| Index Success Rate | N/A | >95% | - | Not measured yet |
| Index Duration (p95) | N/A | <2min | - | Target for medium repos |
| Query Latency (p95) | N/A | <2s | - | For search/ego/impact |
| Cache Hit Rate | N/A | >60% | - | For query caching |
| Test Coverage | 0% | >80% | - | Across all modules |
| Autofix Success Rate | N/A | >90% | - | For safe fixes |
| Violation Detection Rate | N/A | >85% | - | Scorecard accuracy |

## 🎯 Exit Criteria for Stage 1

### Must Have (Blockers)
- [ ] RLS/auth tested with 100% isolation coverage
- [ ] Audit logs in place for all critical operations
- [ ] Indexing/query performance targets met on 10+ sample repos
- [ ] Freshness monitor operational with alerts
- [ ] Scorecard and autofixers produce actionable outputs
- [ ] Risky fixes gated with approval workflow
- [ ] Model routing, compaction, slots enabled by default
- [ ] CI/CD, backups, SLOs, alerts live
- [ ] Runbooks published and dry-run tested
- [ ] v1 API/CLI released
- [ ] Baseline eval reports generated

### Should Have (Important)
- [ ] GitHub Action published to marketplace
- [ ] Blue-green deployment tested
- [ ] DR runbook rehearsed
- [ ] Security scan integrated in CI
- [ ] All documentation link-checked

### Nice to Have (Optional)
- [ ] Performance optimizations beyond targets
- [ ] Additional language support in indexer
- [ ] Enhanced telemetry dashboards
- [ ] Community feedback incorporated

## 📝 Change Log

| Date | Change | Author |
|------|--------|--------|
| 2025-11-22 | Initial tracker creation | DevOps Lead |

## 🔔 Status Legend

- 🔴 **Missing:** Not started, no code written
- 🟡 **Partial:** In progress or partially implemented
- 🟢 **Done:** Complete, tested, documented
- ⚪ **Not Measured:** Metrics not yet collected
- 🔵 **Blocked:** Dependencies not met
