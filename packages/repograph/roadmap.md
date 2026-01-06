# RepoGraph → Enterprise SaaS: Birdseye Roadmap

## Goal
Ship RepoGraph as an enterprise-ready AI ops platform. Two stages:
- **Stage 1:** CLI-first, service-grade backend with RLS, auth, scorecards, autofixers; no UI.
- **Stage 2:** Full frontend (dashboard/visual graph) on top of Stage 1 services.

## Stage 1 – CLI-First, Service-Grade Core (No UI)
- **Scope:** FastAPI/CLI only; multi-tenant RLS PostgreSQL; Redis cache; auth (JWT/OIDC); audit logging; background workers; observability; CI/CD; artifacts and APIs stable.
- **Core Features:** Indexing (SCIP + fallback), graph queries (search/ego/impact), AI-readiness scorecard, safe autofixers (docs/links/selectors), schema-first/sentinel gates, model routing, compaction/slots, playlists/outcome cards generation, cost/latency telemetry.
- **Data & Security:** RLS enforced; org/tenant isolation; scoped tokens; rate limits; PII avoidance; backup/restore.
- **Ops:** Docker Compose + K8s manifests; migrations; seeding; blue/green deploy; SLOs and alerts (availability/latency/error rate).
- **Quality/Evals:** Retrieval eval set; autofix correctness harness; performance benchmarks (indexing, query p95); scorecard KPI tracking.
- **Milestones:**
  1) **Hardening & Foundations:** Auth (JWT/OIDC), RLS policies, migrations, secrets management, structured logging/metrics/traces, rate limits.
  2) **Index/Query Reliability:** Validate SCIP+fallback on 5–10 real repos; perf targets (index <2 min for medium repo; search/ego/impact p95 <2s); freshness monitors.
  3) **AI-Readiness Pipeline:** Scorecard with rubric; safe autofixers (docs/links/selectors/i18n stubs) run in dry-run/PR mode; schema-first + sentinel gates; model router; compaction/slots; playlists/outcome cards.
  4) **Ops Readiness:** CI/CD with rollback; backups/restore; blue/green; SLOs/alerts; release v1 API/CLI; publish runbooks and API contract.
- **Deliverables:** API contract, CLI (`repograph ai-ready`, `repograph index`, `repograph query`), GitHub Action integration, runbooks, SLA doc, base eval reports.
- **Extra Detail (Stage 1 components):**
  - **Scorecard:** Scanners for data-ui coverage, FOLDER docs, relative links, i18n gaps, generated-file edits, type/schema mismatches, route/ability coverage; outputs JSON/Markdown; severity tiers.
  - **Autofixers:** Regenerate docs/indices; fix links; insert data-ui selectors with conventions; add i18n scaffolds; open PR/diff-only by default; approvals for risky fixes.
  - **Gates/Guardrails:** Schema-first planning; drift sentinels; plan/spec mode; cost/token budgets; model routing with fallback/escalation; context compaction/slots default-on.
  - **Telemetry:** Per-run tokens, latency, cost, violations prevented, fixes applied, cache hit rates; eval harness for retrieval and autofix accuracy.
  - **Reliability:** Health checks; readiness/liveness; error budgets; canary deploys; observability dashboards.

## Stage 2 – Frontend & UX Layer
- **Scope:** Web dashboard on Stage 1 APIs; graph visualization; scorecards and autofix results; admin/org management; tokens/usage/cost visibility; approvals and history.
- **Core Features:** Visual graph explorer; scorecard UI with autofix suggestions; task/agent sessions view (playlists, outcome cards); approvals workflow for risky fixes; notifications; multi-tenant UI with RLS-backed APIs.
- **UX/Frontend:** Next.js (or existing stack), auth via OIDC, RBAC for orgs/roles, audit trails in UI. Telemetry surfaced (latency, cost, tokens, retrieval quality).
- **Milestones:**
  1) **UI Foundation:** Auth shell (OIDC), org/tenant-aware routing, RBAC roles, basic dashboards (scorecard summary, index status).
  2) **Graph Explorer:** Visual search/ego/impact flows; filters by org/repo/language; drill-down to file:line; copyable citations.
  3) **Scorecard/Autofix UI:** Findings table with severity; run safe fixes; approval workflow for risky fixes; PR links; history of runs.
  4) **Sessions/Context UX:** Views for playlists and outcome cards; rehydrate context; per-task timelines; export prompts/context packs.
  5) **Observability & Ops UI:** Usage/cost/token charts; latency/errors; alerts; audit trail of actions and approvals.
  6) **GA Readiness:** Polish, e2e tests, docs, optional trials/billing hooks; accessibility and perf budgets.
- **Deliverables:** Frontend app, design system alignment, e2e tests, UX docs, onboarding guide for org admins.

## Cross-Cutting Concerns
- **Compliance/Security:** RLS, audit logs, secrets management, data retention, opt-in memory, privacy for logs/traces.
- **Performance Targets:** Index medium repo <2 min; search/ego/impact p95 <2s; autofix dry-run <10s.
- **Reliability:** SLOs, alerts, rollbacks, canary deployments, error budgets.
- **Package/Release:** Versioned API; migration policy; change log; RC cadence.

## Risks & Mitigations
- **Index quality/perf:** Build evals, cache layer, incremental indexing.
- **Autofix safety:** Default to dry-run/PR; approvals for risky changes.
- **Graph drift:** CI checks on parsers/indexers; freshness monitors.
- **Cost drift:** Model routing with budgets; observable token/cost spend.

---

## Stage 1 Detailed Workplan (Execution View)

**Objective:** Ship a CLI/service-only RepoGraph with RLS, auth, observability, AI-readiness scorecard, safe autofixers, and ops readiness.

### 1) Foundations & Security
- Auth: JWT + OIDC support; token scopes (org, repo, readonly/write); rate limits.
- RLS: Org/tenant policies on all tables; tests covering create/read/update/delete under isolation.
- Secrets: Centralized config; rotation docs; .env templates; non-secret configs checked in.
- Audit logging: Structured logs (ECS/OTEL) for auth, index, autofix actions with correlation IDs.

### 2) Indexing & Query Reliability
- Indexers: Validate SCIP + fallback on 5–10 real repos; ensure language detection; retry/backoff.
- Performance: Target index <2 min for a medium repo; cache hot paths; profile bottlenecks.
- Freshness: Staleness detector; scheduled re-index or on-change hooks; status in API.
- Query correctness: Ego/impact/search tests with fixtures; p95 <2s targets.

### 3) AI-Readiness Scorecard
- Rubric: data-ui coverage, FOLDER docs, relative links, i18n gaps, generated-file edits, type/schema/route/ability checks.
- Output: JSON + Markdown; severities (blocker/warn/info); link to evidence (file:line).
- CLI/API: `repograph ai-ready --format {json,md} --org --repo`; API endpoint with auth.

### 4) Autofixers (Safe by Default)
- Fixes: Regenerate FOLDER/docs indices; convert absolute→relative links; insert data-ui selectors; add i18n stubs; optional lint/format hooks.
- Modes: dry-run diff; PR patch generation; approval required for risky scopes.
- Safety: Skip generated files; guardrails for i18n/selector conventions; impact analysis before apply.

### 5) Guardrails & Efficiency Defaults
- Schema-first: Extract schemas/routes/abilities; require skeleton before gen/changes.
- Drift sentinels: Preflight invariant checks; minimal targeted fetch when risk detected.
- Context mgmt: Auto-compaction; working-memory slots; context playlists; outcome cards.
- Model routing: Fast/balanced/powerful with budgets; retry-escalation on failure; cost/latency logging.

### 6) Telemetry, Evals, and KPIs
- Telemetry: Tokens, latency, cost, violations prevented, fixes applied, cache hits; emit to logs/metrics.
- Evals: Retrieval precision/recall set; autofix correctness harness; scorecard precision; performance benchmarks.
- KPIs: Index latency, query p95, token per task, fix success rate, violation reduction, cost savings.

### 7) Ops & Reliability
- Deploy: Docker Compose + K8s manifests; readiness/liveness; canary/blue-green; rollbacks.
- Backups/Restore: PostgreSQL/Redis procedures; DR runbook.
- SLOs/Alerts: Availability, latency, error rate, staleness, cost budget breaches.
- CI/CD: Tests/evals on PR; migrations; security scans; signed images; changelog/versioning.

### 8) Developer/Consumer Surfaces
- CLI: `repograph index`, `repograph query {search,ego,impact}`, `repograph ai-ready --apply`, `repograph autofix --dry-run/--pr`.
- API: Authenticated endpoints for index status, search/ego/impact, scorecard, autofix (dry-run), telemetry snapshots.
- GitHub Action: PR comment with scorecard; optional autofix patch; status checks for blockers.

### 9) Documentation & Runbooks
- API contract and CLI reference.
- Runbooks: onboarding, index failure triage, staleness remediation, rollback, backup/restore.
- Security/Privacy: RLS explanation, data retention, opt-out for memory/caches.

### Exit Criteria for Stage 1
- RLS/auth tested; audit logs in place.
- Indexing/query perf targets met on sample repos; freshness monitor operational.
- Scorecard and autofixers produce actionable outputs; risky fixes gated.
- Model routing, compaction, slots enabled by default with telemetry.
- CI/CD, backups, SLOs, alerts live; runbooks published.
- v1 API/CLI released; baseline eval reports generated.

---

## Stage 1 → Sprint Plan with Task-Level Assessment

Format per task: `[UID] Title — Owner — Human ETA | AI ETA — Status`. Then: Goal, Prereqs, Ordered Steps, Subtasks, Skills, DoD, Metrics, Warnings/Flag, Artifacts. Status values: Missing / Partial / Done. Owners are placeholders.

Ordered execution for Stage 1:

1) [S1-T1] Auth & RLS Hardening — Owner: TBD — Human: 3–4d | AI: 1–2d — Status: Partial  
   - Goal: Enforce scoped auth and tenant isolation.  
   - Prereqs: DB schema stable.  
   - Ordered Steps: (a) OIDC + scoped JWT (org/repo/read/write); (b) RLS policies on all tables; (c) Isolation fixtures/tests; (d) Rate limits; (e) API keys for CI/bots.  
   - Subtasks: Configure OIDC/JWKS; add JWT scope claims/middleware; write RLS policies + migration; pytest isolation; rate limiter middleware; CI keys.  
   - Skills: authentication, rbac, rate-limiting, api-keys-management, database-migrations.  
   - DoD: Isolation tests pass; scoped tokens live; rate limits enforced; docs updated.  
   - Metrics: Auth error rate; RLS test coverage.  
   - Warnings/Flag: `AUTH_RLS_ENFORCED`.  
   - Artifacts: Auth/RLS doc; test suite.

2) [S1-T2] Indexing Reliability — Owner: TBD — Human: 3–5d | AI: 2–3d — Status: Missing  
   - Goal: Reliable indexing with fallbacks and freshness.  
   - Prereqs: Indexer binaries installed.  
   - Ordered Steps: (a) Validate SCIP+fallback on 5–10 real repos; (b) Language guardrails/retries/backoff; (c) Freshness monitor + re-index triggers; (d) Metrics logging.  
   - Subtasks: Repo matrix + benchmarks; retry/backoff + language allowlist; freshness timestamp + scheduler; index status endpoint; metrics emit.  
   - Skills: scripts-management, caching, logging, metrics-dashboards.  
   - DoD: Median index <2m (medium repo); fallback logged; freshness status API.  
   - Metrics: Index latency/failure rate.  
   - Warnings/Flag: `INDEX_REL_V1`.  
   - Artifacts: Index perf report; freshness endpoint.

3) [S1-T3] Queries (search/ego/impact) — Owner: TBD — Human: 3–4d | AI: 2d — Status: Missing  
   - Goal: Fast, tested query endpoints.  
   - Prereqs: Reliable index data.  
   - Ordered Steps: (a) Contract tests/fixtures; (b) p95 target <2s; (c) Cache hot queries; (d) Staleness invalidation.  
   - Subtasks: Fixture graph data; FastAPI endpoint tests; cache with invalidation; perf/load harness; latency/hit metrics.  
   - Skills: api-conventions, caching, performance-backend, testing.  
   - DoD: Tests green; p95 met; cache hit metric recorded.  
   - Metrics: Query p95/hit rate.  
   - Warnings/Flag: `QUERY_PERF_V1`.  
   - Artifacts: Contract tests; perf report.

4) [S1-T4] AI-Readiness Scorecard — Owner: TBD — Human: 4–6d | AI: 3–4d — Status: Missing  
   - Goal: Detect agent-readiness gaps.  
   - Prereqs: Repo scanners; schema/routes access.  
   - Ordered Steps: (a) Detectors (data-ui, FOLDER docs, relative links, i18n gaps, generated-file edits, schema/route/ability); (b) JSON/MD outputs with severities/evidence; (c) CLI/API `ai-ready`.  
   - Subtasks: Build detectors; fixture repos with known violations; severity mapping; JSON/MD rendering; FastAPI/CLI endpoints.  
   - Skills: data-ui-selectors, docs-workflow, i18n-workflow, api-contracts, docs-link-validation.  
   - DoD: Scorecard runs on sample repo; evidence links valid; severity rules documented.  
   - Metrics: Detector precision/recall on fixtures.  
   - Warnings/Flag: `SCORECARD_V1`.  
   - Artifacts: Scorecard schema; sample outputs.

5) [S1-T5] Autofixers (Safe) — Owner: TBD — Human: 4–6d | AI: 3–4d — Status: Missing  
   - Goal: Safe/approved fixes for common gaps.  
   - Prereqs: Scorecard detectors.  
   - Ordered Steps: (a) Fixes for docs regen, link fixes, data-ui insertion, i18n stubs; (b) Skip generated files; (c) Dry-run/PR patch; (d) Approval gate for risky scopes.  
   - Subtasks: Doc regen/link-fixers; selector/i18n inserters; generated-file skiplist; dry-run diff; GitHub patch/PR generator; approval toggle; add rollback/disable flag for autofix runtime.  
   - Skills: autofixers, patch-generation, docs-workflow, data-ui-selectors.  
   - DoD: Dry-run/patch apply cleanly on samples; approvals enforced.  
   - Metrics: Fix success rate; skipped unsafe files.  
   - Warnings/Flag: `AUTOFIX_SAFE_V1`.  
   - Artifacts: Patch generator; GH Action sample.

6) [S1-T6] Guardrails & Efficiency — Owner: TBD — Human: 4–6d | AI: 3–4d — Status: Missing  
   - Goal: Default safety/efficiency during runs.  
   - Prereqs: Schema accessible via graph.  
   - Ordered Steps: (a) Schema-first skeleton/gate; (b) Drift sentinels preflight; (c) Default compaction/slots/playlists/outcome cards; (d) Model routing with budgets + retry escalation; (e) Token/cost logging.  
   - Subtasks: Schema extraction + gate; sentinel block + targeted fetcher; wire compaction/slots/playlists/cards into CLI; router with budgets/escalations; log token/cost; add feature flags/rollback paths for guardrails and routing.  
   - Skills: llm-guardrails, llm-context-efficiency, observability-otel, metrics-dashboards.  
   - DoD: Guards default-on with overrides; logs show routing/compaction; playlists/cards generated.  
   - Metrics: Tokens per task; violations prevented; routing escalations.  
   - Warnings/Flag: `GUARDRAILS_V1`.  
   - Artifacts: Guardrail config docs; log samples.

7) [S1-T7] Telemetry & Evals — Owner: TBD — Human: 3–5d | AI: 2–3d — Status: Missing  
   - Goal: Measured quality/performance.  
   - Prereqs: Core endpoints instrumented.  
   - Ordered Steps: (a) Emit tokens/latency/cost/violations/fixes/cache hits with trace IDs; (b) Build retrieval eval set; (c) Autofix correctness harness; (d) Perf benchmarks; (e) KPI CSV/CLI.  
   - Subtasks: Metrics middleware; OTEL spans; retrieval/autofix goldens; perf/load scripts; KPI reports; set evaluation cadence/thresholds for ship/block.  
   - Skills: observability-otel, metrics-dashboards, performance-backend, testing.  
   - DoD: Dashboards/CSV produced; evals runnable in CI.  
   - Metrics: Eval scores; perf p95; cost per task.  
   - Warnings/Flag: `TELEM_EVAL_V1`.  
   - Artifacts: Eval suites; KPI exports.

8) [S1-T8] Ops & Reliability — Owner: TBD — Human: 4–6d | AI: 3–4d — Status: Partial  
   - Goal: Deployable, resilient service.  
   - Prereqs: Service configs.  
   - Ordered Steps: (a) K8s manifests with readiness/liveness; (b) Blue-green/canary scripts; (c) Backups/restore for Postgres/Redis; DR runbook; (d) SLOs/alerts; (e) CI/CD with tests/evals, migrations, security scans, signed images, changelog/versioning.  
   - Subtasks: Helm/K8s manifests; probes; blue-green/canary scripts; backup/restore; SLOs/alerts; CI pipeline with tests/evals/security/signing; add secret/PII scanning in CI; data retention/opt-out defaults for logs/caches.  
   - Skills: kubernetes-helm, ci-cd, monitoring-observability, secrets-management, deployment, security.  
   - DoD: Deployed to test cluster; failover tested; alerts firing; pipeline green.  
   - Metrics: Availability, latency, error rate, staleness, cost budget alerts.  
   - Warnings/Flag: `OPS_V1`.  
   - Artifacts: Manifests; runbooks; CI/CD config.

9) [S1-T9] Developer/Consumer Surfaces — Owner: TBD — Human: 3–5d | AI: 2–3d — Status: Partial  
   - Goal: Usable interfaces (CLI/API/GH Action).  
   - Prereqs: Core APIs stable.  
   - Ordered Steps: (a) CLI (`index`, `query search/ego/impact`, `ai-ready --apply`, `autofix --dry-run/--pr`); (b) API endpoints for status/search/ego/impact/scorecard/autofix; (c) GH Action to post scorecard + optional patch.  
   - Subtasks: Implement CLI commands + help; finalize OpenAPI; build GH Action template; validate on sample repo; enforce scoped tokens/access controls for GH Action/CLI; log/audit autofix runs.  
   - Skills: scripts-management, api-conventions, ci-cd, docs-workflow, audit-logging.  
   - DoD: CLI help/docs; API contract; GH Action sample works.  
   - Metrics: CLI/Action success and adoption.  
   - Warnings/Flag: `SURFACES_V1`.  
   - Artifacts: CLI help; API schema; GH Action example.

10) [S1-T10] Docs & Runbooks — Owner: TBD — Human: 2–3d | AI: 1–2d — Status: Partial  
    - Goal: Consumable guidance.  
    - Prereqs: Features landed.  
    - Ordered Steps: (a) API contract, CLI reference, onboarding; (b) Runbooks for index failures, staleness, rollback, backup/restore; (c) Security/privacy notes.  
    - Subtasks: Write API/CLI refs; author runbooks and rehearse; add security/privacy sections; run link checks.  
    - Skills: docs-workflow, docs-link-validation, learnings-management.  
    - DoD: Published docs; runbooks dry-run tested; links valid.  
    - Metrics: Doc lint/link checks; runbook exercises passed.  
    - Warnings/Flag: `DOCS_RUNBOOKS_V1`.  
    - Artifacts: Docs set; runbooks.

11) [S1-T11] Agent-Enablement Parity & Ingestion — Owner: TBD — Human: 4–6d | AI: 3–4d — Status: Missing  
    - Goal: Export/enforce SPA agent-friendly invariants in RepoGraph.  
    - Prereqs: Scorecard + autofix scaffolding.  
    - Ordered Steps: (a) Model enforced invariants (data-ui, generated-file protections, relative links/i18n, FOLDER/indices, config-driven routes, onboarding prompts, discovery hooks); (b) Detect gaps in connected repos; (c) Emit autofix patches; (d) Export minimal context packs (menu/routes/env/tests) similar to existing guides.  
    - Subtasks: Encode invariant rules; map detectors to autofix patches; build context pack generator (routes/env/tests extracts); add staleness monitors for detectors when invariants change; run on sample repos; measure parity score.  
    - Skills: data-ui-selectors, routing, docs-workflow, learnings-management, autofixers, monitoring-observability.  
    - DoD: Connecting RepoGraph to a repo yields parity score + actionable fixes/context packs.  
    - Metrics: Invariant coverage; fix success; reduced manual onboarding.  
    - Warnings/Flag: `AGENT_PARITY_V1`.  
    - Artifacts: Parity report; context pack generator.

---

## Stage 2 → Sprint Plan with Task-Level Assessment

1) [S2-T1] UI Foundation — Owner: TBD — Human: 4–6d | AI: 3–4d — Status: Missing  
   Goal: Authenticated, multi-tenant shell. Prereqs: Stage 1 APIs stable. Ordered Steps: (a) OIDC auth shell; (b) org/tenant-aware routing; (c) RBAC roles; (d) basic dashboards (index status, scorecard summary). DoD: User can log in and see org-scoped dashboards. Metrics: auth success rate; RBAC tests. Warnings/Flag: `UI_FOUNDATION_V1`. Artifacts: UI shell, RBAC config, routing map.
   Subtasks: Wire OIDC client; implement protected layouts; add org/tenant switcher; create minimal dashboard cards. Skills: routing, state-management, authentication, rbac, design-system.

2) [S2-T2] Graph Explorer — Owner: TBD — Human: 4–6d | AI: 3–4d — Status: Missing  
   Goal: Visual search/ego/impact. Prereqs: Query APIs performant. Ordered Steps: (a) Search/ego/impact views with filters; (b) Drill-down to file:line + citations; (c) Loading/error states; (d) Perf telemetry surfaced. DoD: Users can navigate relationships visually; p95 <2s preserved. Metrics: view load p95; click-to-cite accuracy. Warnings/Flag: `GRAPH_UI_V1`. Artifacts: UI pages, perf dashboard tiles.
   Subtasks: Build graph visualization component; integrate search/ego/impact APIs; add filters and breadcrumb navigation; surface citations and copy links; wire perf tracing. Skills: graph-visualization-ui, client-side-api, performance-frontend.

3) [S2-T3] Scorecard/Autofix UI — Owner: TBD — Human: 4–6d | AI: 3–4d — Status: Missing  
   Goal: Operate scorecards/fixes from UI. Prereqs: Scorecard/autofix APIs. Ordered Steps: (a) Findings table with severity; (b) Run safe fixes; (c) Approval workflow for risky fixes; (d) PR links and history of runs/diffs. DoD: UI can trigger safe fixes and request approvals; history visible. Metrics: fix success from UI; approval turnaround. Warnings/Flag: `SCORECARD_UI_V1`. Artifacts: UI flows, approval component, history log.
   Subtasks: Implement findings list with filters; actions to run safe fixes; approval modal/queue; display run history and PR links. Skills: data-ui-selectors, client-side-api, state-management, ui-testing.

4) [S2-T4] Sessions/Context UX — Owner: TBD — Human: 3–5d | AI: 2–3d — Status: Missing  
   Goal: Surface playlists/outcome cards. Prereqs: Stage 1 guardrails/context outputs. Ordered Steps: (a) Views for playlists and outcome cards; (b) Rehydrate context to export prompts/packs; (c) Per-task timelines. DoD: Users can inspect and reuse context artifacts. Metrics: reuse rate; export success. Warnings/Flag: `CONTEXT_UI_V1`. Artifacts: Playlist/outcome UI; export endpoints hooked.
   Subtasks: Build list/detail views for playlists and outcome cards; export/download buttons; timeline component; integrate with backend endpoints. Skills: learnings-management, data-ui-selectors, client-side-api.

5) [S2-T5] Observability & Ops UI — Owner: TBD — Human: 3–5d | AI: 2–3d — Status: Missing  
   Goal: Show usage/cost/tokens and audits. Prereqs: Telemetry live. Ordered Steps: (a) Usage/cost/token charts; (b) Alerts surface; (c) Audit trail of actions/approvals. DoD: Dashboards render; audit drill-down works. Metrics: chart availability; audit query latency. Warnings/Flag: `OBS_UI_V1`. Artifacts: Dashboards, audit views.
   Subtasks: Chart components for usage/cost/tokens; alerts list; audit events list/detail with filters; link to underlying traces. Skills: metrics-dashboards, logging, client-side-api.

6) [S2-T6] QA & Delivery — Owner: TBD — Human: 3–5d | AI: 2–3d — Status: Missing  
   Goal: Ship UI with quality gates. Prereqs: UI features built. Ordered Steps: (a) E2E tests (auth, graph explorer, scorecard runs, approvals); (b) Accessibility/perf budgets; (c) GA polish; (d) Docs/onboarding for org admins; (e) Optional trials/billing hooks. DoD: E2E green; budgets met; docs published. Metrics: E2E pass rate; lighthouse/a11y scores. Warnings/Flag: `UI_GA_V1`. Artifacts: E2E suite, onboarding docs.
   Subtasks: Write Playwright/Cypress flows; run lighthouse/a11y; finalize copy and empty/error states; produce admin onboarding doc; add billing/trial toggle if needed. Skills: e2e-testing, accessibility, performance-frontend, docs-workflow.
