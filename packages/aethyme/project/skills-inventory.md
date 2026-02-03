# Skills Inventory for Aethyme Stage 1

**Date:** 2025-11-22
**Purpose:** Map all required skills for Stage 1 tasks and identify gaps
**Status:** Complete

---

## Executive Summary

- **Total unique skills needed:** 36
- **Skills available:** 4 (rbac, scripts-management, architecture, learnings-management)
- **Skills missing:** 32
- **Gap percentage:** 89%
- **Priority skills to create:** 10 (covers 70% of task needs)

---

## Required Skills by Task

### S1-T1: Auth & RLS Hardening
**Owner:** TBD | **Human ETA:** 3-4d | **AI ETA:** 1-2d | **Status:** Partial

**Skills Required:**
- ✅ **authentication** - OIDC + scoped JWT (org/repo/read/write)
- ✅ **rbac** - Role-based access control (EXISTS)
- ❌ **rate-limiting** - Rate limiter middleware (MISSING - Priority 1)
- ❌ **api-keys-management** - API keys for CI/bots (MISSING - Priority 2)
- ❌ **database-migrations** - RLS policies migration (MISSING - Priority 3)

**Coverage:** 1/5 (20%)

---

### S1-T2: Indexing Reliability
**Owner:** TBD | **Human ETA:** 3-5d | **AI ETA:** 2-3d | **Status:** Missing

**Skills Required:**
- ✅ **scripts-management** - Indexer binaries, automation (EXISTS)
- ❌ **caching** - Cache hot paths, freshness invalidation (MISSING - Priority 4)
- ❌ **logging** - Structured logging with correlation IDs (MISSING - Priority 5)
- ❌ **metrics-dashboards** - Index latency/failure metrics (MISSING - Priority 6)

**Coverage:** 1/4 (25%)

---

### S1-T3: Queries (search/ego/impact)
**Owner:** TBD | **Human ETA:** 3-4d | **AI ETA:** 2d | **Status:** Missing

**Skills Required:**
- ❌ **api-conventions** - FastAPI endpoint design patterns (MISSING)
- ❌ **caching** - Cache hot queries (MISSING - Priority 4)
- ❌ **performance-backend** - p95 optimization, load testing (MISSING)
- ❌ **testing** - Contract tests, fixtures (MISSING)

**Coverage:** 0/4 (0%)

---

### S1-T4: AI-Readiness Scorecard
**Owner:** TBD | **Human ETA:** 4-6d | **AI ETA:** 3-4d | **Status:** Missing

**Skills Required:**
- ❌ **data-ui-selectors** - Detect data-ui coverage (MISSING)
- ❌ **docs-workflow** - FOLDER docs validation (MISSING)
- ❌ **i18n-workflow** - i18n gap detection (MISSING)
- ❌ **api-contracts** - Schema/route validation (MISSING)
- ❌ **docs-link-validation** - Relative link checker (MISSING)

**Coverage:** 0/5 (0%)

---

### S1-T5: Autofixers (Safe)
**Owner:** TBD | **Human ETA:** 4-6d | **AI ETA:** 3-4d | **Status:** Missing

**Skills Required:**
- ❌ **autofixers** - Safe fix patterns, dry-run mode (MISSING)
- ❌ **patch-generation** - GitHub PR/patch generation (MISSING)
- ❌ **docs-workflow** - Doc regeneration (MISSING)
- ❌ **data-ui-selectors** - Selector insertion (MISSING)

**Coverage:** 0/4 (0%)

---

### S1-T6: Guardrails & Efficiency
**Owner:** TBD | **Human ETA:** 4-6d | **AI ETA:** 3-4d | **Status:** Missing

**Skills Required:**
- ❌ **llm-guardrails** - Schema-first gates, drift sentinels (MISSING - Priority 8)
- ❌ **llm-context-efficiency** - Compaction, slots, playlists (MISSING - Priority 9)
- ❌ **observability-otel** - Token/cost logging, OTEL spans (MISSING - Priority 7)
- ❌ **metrics-dashboards** - Routing/compaction metrics (MISSING - Priority 6)

**Coverage:** 0/4 (0%)

---

### S1-T7: Telemetry & Evals
**Owner:** TBD | **Human ETA:** 3-5d | **AI ETA:** 2-3d | **Status:** Missing

**Skills Required:**
- ❌ **observability-otel** - OTEL spans, trace IDs (MISSING - Priority 7)
- ❌ **metrics-dashboards** - Dashboards/CSV exports (MISSING - Priority 6)
- ❌ **performance-backend** - Perf benchmarks (MISSING)
- ❌ **testing** - Eval harness, golden sets (MISSING)

**Coverage:** 0/4 (0%)

---

### S1-T8: Ops & Reliability
**Owner:** TBD | **Human ETA:** 4-6d | **AI ETA:** 3-4d | **Status:** Partial

**Skills Required:**
- ❌ **kubernetes-helm** - K8s manifests, probes (MISSING - Priority 10)
- ❌ **ci-cd** - CI pipeline, tests/evals (MISSING)
- ❌ **monitoring-observability** - SLOs, alerts (MISSING)
- ❌ **secrets-management** - Secret scanning, rotation (MISSING)
- ❌ **deployment** - Blue-green, canary (MISSING)
- ❌ **security** - PII scanning, data retention (MISSING)

**Coverage:** 0/6 (0%)

---

### S1-T9: Developer/Consumer Surfaces
**Owner:** TBD | **Human ETA:** 3-5d | **AI ETA:** 2-3d | **Status:** Partial

**Skills Required:**
- ✅ **scripts-management** - CLI implementation (EXISTS)
- ❌ **api-conventions** - OpenAPI, API endpoints (MISSING)
- ❌ **ci-cd** - GitHub Action template (MISSING)
- ❌ **docs-workflow** - CLI help, API docs (MISSING)
- ❌ **audit-logging** - Log autofix runs (MISSING)

**Coverage:** 1/5 (20%)

---

### S1-T10: Docs & Runbooks
**Owner:** TBD | **Human ETA:** 2-3d | **AI ETA:** 1-2d | **Status:** Partial

**Skills Required:**
- ❌ **docs-workflow** - API/CLI reference (MISSING)
- ❌ **docs-link-validation** - Link checks (MISSING)
- ✅ **learnings-management** - Runbooks (EXISTS)

**Coverage:** 1/3 (33%)

---

### S1-T11: Agent-Enablement Parity & Ingestion
**Owner:** TBD | **Human ETA:** 4-6d | **AI ETA:** 3-4d | **Status:** Missing

**Skills Required:**
- ❌ **data-ui-selectors** - Invariant rules (MISSING)
- ❌ **routing** - Config-driven routes (MISSING)
- ❌ **docs-workflow** - Context pack generator (MISSING)
- ✅ **learnings-management** - Onboarding prompts (EXISTS)
- ❌ **autofixers** - Patch generation (MISSING)
- ❌ **monitoring-observability** - Staleness monitors (MISSING)

**Coverage:** 1/6 (17%)

---

## Skills Summary Table

| Skill Name | Priority | Used By Tasks | Status |
|------------|----------|---------------|--------|
| **rate-limiting** | 1 | S1-T1 | ❌ MISSING |
| **api-keys-management** | 2 | S1-T1 | ❌ MISSING |
| **database-migrations** | 3 | S1-T1 | ❌ MISSING |
| **caching** | 4 | S1-T2, S1-T3 | ❌ MISSING |
| **logging** | 5 | S1-T2 | ❌ MISSING |
| **metrics-dashboards** | 6 | S1-T2, S1-T6, S1-T7 | ❌ MISSING |
| **observability-otel** | 7 | S1-T6, S1-T7 | ❌ MISSING |
| **llm-guardrails** | 8 | S1-T6 | ❌ MISSING |
| **llm-context-efficiency** | 9 | S1-T6 | ❌ MISSING |
| **kubernetes-helm** | 10 | S1-T8 | ❌ MISSING |
| api-conventions | - | S1-T3, S1-T9 | ❌ MISSING |
| performance-backend | - | S1-T3, S1-T7 | ❌ MISSING |
| testing | - | S1-T3, S1-T7 | ❌ MISSING |
| data-ui-selectors | - | S1-T4, S1-T5, S1-T11 | ❌ MISSING |
| docs-workflow | - | S1-T4, S1-T5, S1-T9, S1-T10, S1-T11 | ❌ MISSING |
| i18n-workflow | - | S1-T4 | ❌ MISSING |
| api-contracts | - | S1-T4 | ❌ MISSING |
| docs-link-validation | - | S1-T4, S1-T10 | ❌ MISSING |
| autofixers | - | S1-T5, S1-T11 | ❌ MISSING |
| patch-generation | - | S1-T5 | ❌ MISSING |
| ci-cd | - | S1-T8, S1-T9 | ❌ MISSING |
| monitoring-observability | - | S1-T8, S1-T11 | ❌ MISSING |
| secrets-management | - | S1-T8 | ❌ MISSING |
| deployment | - | S1-T8 | ❌ MISSING |
| security | - | S1-T8 | ❌ MISSING |
| audit-logging | - | S1-T9 | ❌ MISSING |
| routing | - | S1-T11 | ❌ MISSING |
| authentication | - | S1-T1 | ✅ COVERED BY rbac |
| **rbac** | - | S1-T1 | ✅ EXISTS |
| **scripts-management** | - | S1-T2, S1-T9 | ✅ EXISTS |
| **architecture** | - | Cross-cutting | ✅ EXISTS |
| **learnings-management** | - | S1-T10, S1-T11 | ✅ EXISTS |

---

## Gap Analysis by Task Status

### Partial Tasks (Have Some Skills)
- **S1-T1:** 1/5 skills (20% coverage)
- **S1-T2:** 1/4 skills (25% coverage)
- **S1-T8:** 0/6 skills (0% coverage, marked partial in roadmap)
- **S1-T9:** 1/5 skills (20% coverage)
- **S1-T10:** 1/3 skills (33% coverage)
- **S1-T11:** 1/6 skills (17% coverage)

### Missing Tasks (No Skills)
- **S1-T3:** 0/4 skills (0% coverage)
- **S1-T4:** 0/5 skills (0% coverage)
- **S1-T5:** 0/4 skills (0% coverage)
- **S1-T6:** 0/4 skills (0% coverage)
- **S1-T7:** 0/4 skills (0% coverage)

---

## Recommendations

### Phase 1: Foundation Skills (Week 1-2)
Create the top 10 priority skills that unlock 70% of tasks:
1. rate-limiting
2. api-keys-management
3. database-migrations
4. caching
5. logging
6. metrics-dashboards
7. observability-otel
8. llm-guardrails
9. llm-context-efficiency
10. kubernetes-helm

### Phase 2: Feature Skills (Week 3-4)
Create skills for specific features:
- docs-workflow (used by 5 tasks)
- data-ui-selectors (used by 3 tasks)
- api-conventions (used by 2 tasks)
- autofixers (used by 2 tasks)
- monitoring-observability (used by 2 tasks)

### Phase 3: Specialized Skills (Week 5+)
Create remaining specialized skills:
- testing, performance-backend, ci-cd, deployment, security, etc.

---

## Success Metrics

- **Coverage Target:** 90% skill coverage before Stage 1 kickoff
- **Training Time:** New developer can load priority skills in <2 hours
- **Skill Quality:** Each skill has runnable examples and troubleshooting
- **Onboarding Speed:** Developer productive on first task within 1 day

---

## Next Steps

1. Create top 10 priority skills (see TRAINING_PLAN.md)
2. Assign skill creation to team members
3. Review and validate each skill with task owners
4. Update skills as Stage 1 tasks progress
5. Measure skill usage and effectiveness

---

**Document Owner:** Skills & Training Lead
**Review Cadence:** Weekly during Stage 1
**Last Updated:** 2025-11-22
